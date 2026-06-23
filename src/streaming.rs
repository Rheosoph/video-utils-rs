use crate::{
    error::{Error, Result},
    packet::{EncodedPacket, PacketSlice},
};

/// One segment in an HLS media playlist.
#[derive(Clone, Debug, PartialEq)]
pub struct HlsSegment {
    /// Segment duration in seconds.
    pub duration_seconds: f64,
    /// Segment URI.
    pub uri: String,
    /// Optional title.
    pub title: Option<String>,
    /// Optional byterange `(length, offset)`.
    pub byte_range: Option<(u64, Option<u64>)>,
}

impl HlsSegment {
    /// Create a segment.
    #[must_use]
    pub fn new(duration_seconds: f64, uri: impl Into<String>) -> Self {
        Self {
            duration_seconds,
            uri: uri.into(),
            title: None,
            byte_range: None,
        }
    }
}

/// HLS media playlist model.
#[derive(Clone, Debug, PartialEq)]
pub struct HlsPlaylist {
    /// Playlist version.
    pub version: u8,
    /// Target duration in seconds.
    pub target_duration: u32,
    /// First media sequence number.
    pub media_sequence: u64,
    /// Whether all segments start on independent frames.
    pub independent_segments: bool,
    /// Optional fMP4 init segment URI.
    pub map_uri: Option<String>,
    /// Media segments.
    pub segments: Vec<HlsSegment>,
    /// Whether to emit `#EXT-X-ENDLIST`.
    pub end_list: bool,
}

impl HlsPlaylist {
    /// Create a VOD playlist and derive target duration from segments.
    #[must_use]
    pub fn vod(segments: Vec<HlsSegment>) -> Self {
        let target_duration = segments
            .iter()
            .map(|segment| segment.duration_seconds.ceil() as u32)
            .max()
            .unwrap_or(0);

        Self {
            version: 7,
            target_duration,
            media_sequence: 0,
            independent_segments: true,
            map_uri: None,
            segments,
            end_list: true,
        }
    }

    /// Serialize a media playlist.
    #[must_use]
    pub fn to_m3u8(&self) -> String {
        let mut output = String::from("#EXTM3U\n");
        output.push_str(&format!("#EXT-X-VERSION:{}\n", self.version));
        if self.independent_segments {
            output.push_str("#EXT-X-INDEPENDENT-SEGMENTS\n");
        }
        output.push_str(&format!("#EXT-X-TARGETDURATION:{}\n", self.target_duration));
        output.push_str(&format!("#EXT-X-MEDIA-SEQUENCE:{}\n", self.media_sequence));
        if let Some(map_uri) = &self.map_uri {
            output.push_str(&format!(
                "#EXT-X-MAP:URI=\"{}\"\n",
                escape_attribute(map_uri)
            ));
        }

        for segment in &self.segments {
            output.push_str(&format!(
                "#EXTINF:{:.6},{}\n",
                segment.duration_seconds,
                segment.title.as_deref().unwrap_or("")
            ));
            if let Some((length, offset)) = segment.byte_range {
                match offset {
                    Some(offset) => {
                        output.push_str(&format!("#EXT-X-BYTERANGE:{length}@{offset}\n"))
                    }
                    None => output.push_str(&format!("#EXT-X-BYTERANGE:{length}\n")),
                }
            }
            output.push_str(&segment.uri);
            output.push('\n');
        }

        if self.end_list {
            output.push_str("#EXT-X-ENDLIST\n");
        }

        output
    }
}

/// Plan keyframe-aligned packet segments for one track.
pub fn plan_keyframe_segments(
    packets: &[EncodedPacket],
    track_id: u32,
    target_duration_seconds: f64,
) -> Result<Vec<PacketSlice>> {
    if packets.is_empty() {
        return Err(Error::EmptyInput);
    }
    if target_duration_seconds <= 0.0 || !target_duration_seconds.is_finite() {
        return Err(Error::InvalidRange { start: 0, end: 0 });
    }

    let track_packets: Vec<(usize, &EncodedPacket)> = packets
        .iter()
        .enumerate()
        .filter(|(_, packet)| packet.track_id == track_id)
        .collect();
    if track_packets.is_empty() {
        return Err(Error::IncompatibleTrack {
            track_id,
            reason: "track is not present",
        });
    }

    let time_base = track_packets[0].1.time_base;
    let target_ticks = time_base.seconds_to_ticks(target_duration_seconds);
    let mut slices = Vec::new();
    let mut start_index = track_packets[0].0;
    let mut start_pts = track_packets[0].1.pts;

    for (index, packet) in track_packets.iter().skip(1).copied() {
        if packet.is_keyframe && packet.pts - start_pts >= target_ticks {
            slices.push(PacketSlice {
                start: start_index,
                end: index,
            });
            start_index = index;
            start_pts = packet.pts;
        }
    }

    if let Some((last_index, _)) = track_packets.last() {
        let end = last_index + 1;
        if end > start_index {
            slices.push(PacketSlice {
                start: start_index,
                end,
            });
        }
    }

    Ok(slices)
}

fn escape_attribute(value: &str) -> String {
    value.replace('"', "%22")
}

#[cfg(feature = "containers")]
mod object_store_impl {
    use super::{HlsPlaylist, HlsSegment, plan_keyframe_segments};
    use crate::{
        container::ContainerFormat,
        containers,
        error::{Error, Result},
        media::{MediaInfo, StreamInfo},
        object_store_io::{demux_object, write_object_bytes},
        packet::{EncodedPacket, PacketSlice},
    };
    use bytes::Bytes;
    use object_store::{ObjectStore, path::Path};
    use std::collections::BTreeSet;

    /// Segment container used by object-store HLS packaging.
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub enum HlsSegmentContainer {
        /// MPEG-TS media segments (`.ts`).
        MpegTs,
        /// Fragmented MP4 media segments (`.m4s`) with an init segment.
        Mp4,
    }

    impl HlsSegmentContainer {
        fn extension(self) -> &'static str {
            match self {
                Self::MpegTs => "ts",
                Self::Mp4 => "m4s",
            }
        }

        fn container_format(self) -> ContainerFormat {
            match self {
                Self::MpegTs => ContainerFormat::MpegTs,
                Self::Mp4 => ContainerFormat::Mp4,
            }
        }
    }

    /// Options for object-store VOD HLS packaging.
    #[derive(Clone, Debug, PartialEq)]
    pub struct ObjectHlsVodJob {
        /// Segment duration target used when splitting on keyframes.
        pub target_duration_seconds: f64,
        /// Track used for segment boundary planning. The first video track is used when unset,
        /// otherwise the first audio track.
        pub segment_track_id: Option<u32>,
        /// Container used for each segment object.
        pub segment_format: HlsSegmentContainer,
        /// Object-key prefix for segments. Relative prefixes are written next to the playlist.
        pub segment_prefix: Option<String>,
        /// Object-key or file name for the fMP4 init segment. Relative names are written next
        /// to the playlist.
        pub init_segment_name: Option<String>,
        /// URI prefix written into the playlist before segment file names.
        pub uri_prefix: Option<String>,
        /// Include packets from every stream that fall within each segment slice.
        pub copy_all_tracks: bool,
    }

    impl ObjectHlsVodJob {
        /// Create a VOD HLS packaging job with TS segments and six-second target duration.
        #[must_use]
        pub const fn new() -> Self {
            Self {
                target_duration_seconds: 6.0,
                segment_track_id: None,
                segment_format: HlsSegmentContainer::MpegTs,
                segment_prefix: None,
                init_segment_name: None,
                uri_prefix: None,
                copy_all_tracks: true,
            }
        }

        /// Set the target segment duration in seconds.
        #[must_use]
        pub const fn with_target_duration(mut self, seconds: f64) -> Self {
            self.target_duration_seconds = seconds;
            self
        }

        /// Set the track used for segment boundary planning.
        #[must_use]
        pub const fn with_segment_track(mut self, track_id: u32) -> Self {
            self.segment_track_id = Some(track_id);
            self
        }

        /// Select segment container.
        #[must_use]
        pub const fn with_segment_format(mut self, format: HlsSegmentContainer) -> Self {
            self.segment_format = format;
            self
        }

        /// Set the object key prefix used for segment objects.
        #[must_use]
        pub fn with_segment_prefix(mut self, prefix: impl Into<String>) -> Self {
            self.segment_prefix = Some(prefix.into());
            self
        }

        /// Set the object key or file name used for the fMP4 init segment.
        #[must_use]
        pub fn with_init_segment_name(mut self, name: impl Into<String>) -> Self {
            self.init_segment_name = Some(name.into());
            self
        }

        /// Set a URI prefix written into the playlist.
        #[must_use]
        pub fn with_uri_prefix(mut self, prefix: impl Into<String>) -> Self {
            self.uri_prefix = Some(prefix.into());
            self
        }

        /// Control whether non-boundary tracks are included in segments.
        #[must_use]
        pub const fn copy_all_tracks(mut self, copy_all_tracks: bool) -> Self {
            self.copy_all_tracks = copy_all_tracks;
            self
        }
    }

    impl Default for ObjectHlsVodJob {
        fn default() -> Self {
            Self::new()
        }
    }

    /// Report returned after object-store HLS packaging.
    #[derive(Clone, Debug, PartialEq)]
    pub struct ObjectHlsVodReport {
        /// Playlist object key.
        pub playlist: Path,
        /// Segment object keys written.
        pub segments: Vec<Path>,
        /// Optional fMP4 initialization segment object key.
        pub init_segment: Option<Path>,
        /// Segment container format.
        pub segment_format: ContainerFormat,
        /// Number of media segments written.
        pub segment_count: usize,
        /// Total bytes written across segments and playlist.
        pub bytes_written: u64,
        /// Serialized playlist model.
        pub playlist_model: HlsPlaylist,
    }

    /// Package already-demuxed packet data into VOD HLS objects in one store.
    pub async fn write_hls_vod_same_store(
        store: &dyn ObjectStore,
        playlist: &Path,
        media: &MediaInfo,
        packets: &[EncodedPacket],
        job: &ObjectHlsVodJob,
    ) -> Result<ObjectHlsVodReport> {
        write_hls_vod_between_stores(store, playlist, media, packets, job).await
    }

    /// Package already-demuxed packet data into VOD HLS objects.
    pub async fn write_hls_vod_between_stores(
        target_store: &dyn ObjectStore,
        playlist: &Path,
        media: &MediaInfo,
        packets: &[EncodedPacket],
        job: &ObjectHlsVodJob,
    ) -> Result<ObjectHlsVodReport> {
        let boundary_track = select_boundary_track(media, job.segment_track_id)?;
        let slices = plan_keyframe_segments(
            packets,
            boundary_track.track_id,
            job.target_duration_seconds,
        )?;
        let mut playlist_segments = Vec::with_capacity(slices.len());
        let mut segment_paths = Vec::with_capacity(slices.len());
        let mut segment_groups = Vec::with_capacity(slices.len());
        let mut bytes_written = 0_u64;

        for (index, slice) in slices.iter().copied().enumerate() {
            let segment_packets =
                segment_packets(packets, slice, boundary_track.track_id, job.copy_all_tracks);
            if segment_packets.is_empty() {
                continue;
            }
            let segment_path = segment_path(playlist, job, index);
            let duration = segment_duration_seconds(&segment_packets, boundary_track.track_id);
            playlist_segments.push(HlsSegment::new(duration, segment_uri(&segment_path, job)));
            segment_paths.push(segment_path);
            segment_groups.push(segment_packets);
        }

        if playlist_segments.is_empty() {
            return Err(Error::EmptyInput);
        }

        let mut init_segment = None::<Path>;
        match job.segment_format {
            HlsSegmentContainer::MpegTs => {
                for (segment_path, segment_packets) in segment_paths.iter().zip(&segment_groups) {
                    let segment_media = segment_media(media, segment_packets)?;
                    let segment_bytes =
                        containers::mux_mpeg_ts_bytes(&segment_media, segment_packets)?;
                    bytes_written += segment_bytes.len() as u64;
                    write_object_bytes(target_store, segment_path, segment_bytes).await?;
                }
            }
            HlsSegmentContainer::Mp4 => {
                let fmp4_media = segment_media_for_all(media, &segment_groups)?;
                let output = containers::mux_fragmented_mp4_segments(
                    &fmp4_media,
                    &segment_groups,
                    hls_fragment_duration_ms(job.target_duration_seconds)?,
                )?;
                let init_path = init_segment_path(playlist, job);
                bytes_written += output.init_segment.len() as u64;
                write_object_bytes(target_store, &init_path, output.init_segment).await?;
                for (segment_path, segment_bytes) in segment_paths.iter().zip(output.media_segments)
                {
                    bytes_written += segment_bytes.len() as u64;
                    write_object_bytes(target_store, segment_path, segment_bytes).await?;
                }
                init_segment = Some(init_path);
            }
        }

        let mut playlist_model = HlsPlaylist::vod(playlist_segments);
        if let Some(init_path) = &init_segment {
            playlist_model.map_uri = Some(segment_uri(init_path, job));
        }
        let playlist_bytes = Bytes::from(playlist_model.to_m3u8());
        bytes_written += playlist_bytes.len() as u64;
        write_object_bytes(target_store, playlist, playlist_bytes).await?;

        Ok(ObjectHlsVodReport {
            playlist: playlist.clone(),
            segments: segment_paths,
            init_segment,
            segment_format: job.segment_format.container_format(),
            segment_count: playlist_model.segments.len(),
            bytes_written,
            playlist_model,
        })
    }

    /// Demux an object-store media object and package it into VOD HLS in the same store.
    pub async fn package_object_hls_vod_same_store(
        store: &dyn ObjectStore,
        source: &Path,
        playlist: &Path,
        job: &ObjectHlsVodJob,
    ) -> Result<ObjectHlsVodReport> {
        package_object_hls_vod_between_stores(store, source, store, playlist, job).await
    }

    /// Demux an object-store media object and package it into VOD HLS.
    pub async fn package_object_hls_vod_between_stores(
        source_store: &dyn ObjectStore,
        source: &Path,
        target_store: &dyn ObjectStore,
        playlist: &Path,
        job: &ObjectHlsVodJob,
    ) -> Result<ObjectHlsVodReport> {
        let demuxed = demux_object(source_store, source).await?;
        write_hls_vod_between_stores(
            target_store,
            playlist,
            &demuxed.media,
            &demuxed.packets,
            job,
        )
        .await
    }

    fn select_boundary_track(media: &MediaInfo, requested: Option<u32>) -> Result<&StreamInfo> {
        if let Some(track_id) = requested {
            return media.stream(track_id).ok_or(Error::IncompatibleTrack {
                track_id,
                reason: "requested HLS segment track is missing",
            });
        }
        media
            .video_streams()
            .next()
            .or_else(|| media.audio_streams().next())
            .ok_or(Error::Unsupported {
                operation: "hls package",
                reason: "media has no video or audio track for segment planning",
            })
    }

    fn segment_packets(
        packets: &[EncodedPacket],
        slice: PacketSlice,
        boundary_track_id: u32,
        copy_all_tracks: bool,
    ) -> Vec<EncodedPacket> {
        let bounded = packets
            .get(slice.start..slice.end)
            .unwrap_or_default()
            .iter()
            .cloned();
        if copy_all_tracks {
            bounded.collect()
        } else {
            bounded
                .filter(|packet| packet.track_id == boundary_track_id)
                .collect()
        }
    }

    fn segment_media(media: &MediaInfo, packets: &[EncodedPacket]) -> Result<MediaInfo> {
        let track_ids = packets
            .iter()
            .map(|packet| packet.track_id)
            .collect::<BTreeSet<_>>();
        let mut out = MediaInfo {
            duration_seconds: packets
                .iter()
                .map(|packet| packet.time_base.ticks_to_seconds(packet.end_pts()))
                .max_by(f64::total_cmp),
            ..Default::default()
        };
        for track_id in track_ids {
            let stream = media.stream(track_id).ok_or(Error::IncompatibleTrack {
                track_id,
                reason: "segment packet references a stream missing from MediaInfo",
            })?;
            out.push_stream(stream.clone());
        }
        Ok(out)
    }

    fn segment_media_for_all(
        media: &MediaInfo,
        segment_groups: &[Vec<EncodedPacket>],
    ) -> Result<MediaInfo> {
        let packets = segment_groups
            .iter()
            .flat_map(|segment| segment.iter().cloned())
            .collect::<Vec<_>>();
        segment_media(media, &packets)
    }

    fn hls_fragment_duration_ms(target_duration_seconds: f64) -> Result<u32> {
        if target_duration_seconds <= 0.0 || !target_duration_seconds.is_finite() {
            return Err(Error::InvalidRange { start: 0, end: 0 });
        }
        Ok((target_duration_seconds * 1_000.0).ceil().max(1.0) as u32)
    }

    fn segment_duration_seconds(packets: &[EncodedPacket], track_id: u32) -> f64 {
        let mut track_packets = packets
            .iter()
            .filter(|packet| packet.track_id == track_id)
            .collect::<Vec<_>>();
        track_packets.sort_by_key(|packet| packet.pts);
        let Some(first) = track_packets.first() else {
            return 0.0;
        };
        let end = track_packets
            .iter()
            .map(|packet| packet.end_pts())
            .max()
            .unwrap_or(first.pts);
        first.time_base.ticks_to_seconds(end - first.pts)
    }

    fn segment_path(playlist: &Path, job: &ObjectHlsVodJob, index: usize) -> Path {
        let prefix = job
            .segment_prefix
            .clone()
            .unwrap_or_else(|| format!("{}segment-", playlist_directory(playlist)));
        let prefix = if prefix.contains('/') {
            prefix
        } else {
            format!("{}{}", playlist_directory(playlist), prefix)
        };
        Path::from(format!(
            "{prefix}{index:05}.{}",
            job.segment_format.extension()
        ))
    }

    fn init_segment_path(playlist: &Path, job: &ObjectHlsVodJob) -> Path {
        let name = job
            .init_segment_name
            .clone()
            .unwrap_or_else(|| "init.mp4".to_owned());
        if name.contains('/') {
            Path::from(name)
        } else {
            Path::from(format!("{}{}", playlist_directory(playlist), name))
        }
    }

    fn segment_uri(path: &Path, job: &ObjectHlsVodJob) -> String {
        let value = path.to_string();
        let file_name = value.rsplit('/').next().unwrap_or(value.as_str());
        match &job.uri_prefix {
            Some(prefix) => format!("{prefix}{file_name}"),
            None => file_name.to_owned(),
        }
    }

    fn playlist_directory(playlist: &Path) -> String {
        let value = playlist.to_string();
        value
            .rsplit_once('/')
            .map(|(directory, _)| format!("{directory}/"))
            .unwrap_or_default()
    }
}

#[cfg(feature = "containers")]
pub use object_store_impl::{
    HlsSegmentContainer, ObjectHlsVodJob, ObjectHlsVodReport,
    package_object_hls_vod_between_stores, package_object_hls_vod_same_store,
    write_hls_vod_between_stores, write_hls_vod_same_store,
};

#[cfg(test)]
mod tests {
    use super::{HlsPlaylist, HlsSegment, plan_keyframe_segments};
    use crate::{codec::CodecId, packet::EncodedPacket, time::TimeBase};

    #[test]
    fn writes_hls_playlist() {
        let playlist = HlsPlaylist::vod(vec![
            HlsSegment::new(2.0, "seg0.m4s"),
            HlsSegment::new(2.5, "seg1.m4s"),
        ]);

        let output = playlist.to_m3u8();

        assert!(output.contains("#EXT-X-TARGETDURATION:3"));
        assert!(output.contains("seg1.m4s"));
        assert!(output.ends_with("#EXT-X-ENDLIST\n"));
    }

    #[test]
    fn plans_segments_on_keyframes() {
        let tb = TimeBase::milliseconds();
        let packets: Vec<_> = (0..5)
            .map(|index| {
                EncodedPacket::new(1, CodecId::H264, index * 1_000, 1_000, tb, vec![0])
                    .with_keyframe(index % 2 == 0)
            })
            .collect();

        let segments = plan_keyframe_segments(&packets, 1, 2.0).unwrap();

        assert_eq!(segments.len(), 3);
        assert_eq!(segments[0].start, 0);
        assert_eq!(segments[0].end, 2);
        assert_eq!(segments[2].start, 4);
        assert_eq!(segments[2].end, 5);
    }
}
