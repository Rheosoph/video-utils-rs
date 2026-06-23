use crate::{
    codec::{CodecId, MediaType, VideoDecoder, VideoEncoder},
    container::ContainerFormat,
    containers,
    error::{Error, Result},
    media::{MediaInfo, StreamInfo},
    object_store_io::{
        demux_object, detect_object_container_format, read_object_bytes, write_object_bytes,
    },
    packet::EncodedPacket,
    subtitle::{
        SubtitleFormat, SubtitleStyle, burn_subtitles_onto_frame, parse_subtitles,
        subtitle_codec_for_format, subtitle_events_to_packets, subtitle_format_for_codec,
        subtitle_packets_to_events, write_srt, write_webvtt,
    },
    time::TimeBase,
};
use bytes::Bytes;
use object_store::{ObjectStore, path::Path};

/// Options for adding a subtitle sidecar as a container track.
#[derive(Clone, Debug, PartialEq)]
pub struct ObjectSubtitleTrackJob {
    /// Track id assigned to the subtitle stream.
    pub track_id: u32,
    /// Sidecar format to parse.
    pub format: SubtitleFormat,
    /// Optional language tag written into the stream metadata.
    pub language: Option<String>,
    /// Subtitle packet time base.
    pub time_base: TimeBase,
}

impl ObjectSubtitleTrackJob {
    /// Create a subtitle track job using a millisecond packet time base.
    #[must_use]
    pub const fn new(track_id: u32, format: SubtitleFormat) -> Self {
        Self {
            track_id,
            format,
            language: None,
            time_base: TimeBase::milliseconds(),
        }
    }

    /// Set subtitle stream language.
    #[must_use]
    pub fn with_language(mut self, language: impl Into<String>) -> Self {
        self.language = Some(language.into());
        self
    }

    /// Set subtitle packet time base.
    #[must_use]
    pub const fn with_time_base(mut self, time_base: TimeBase) -> Self {
        self.time_base = time_base;
        self
    }
}

/// Report returned after adding a subtitle track.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ObjectSubtitleTrackReport {
    /// Source media object key.
    pub source: Path,
    /// Subtitle sidecar object key.
    pub sidecar: Path,
    /// Target media object key.
    pub target: Path,
    /// Source container format.
    pub source_format: ContainerFormat,
    /// Target container format.
    pub target_format: ContainerFormat,
    /// Subtitle track id.
    pub subtitle_track_id: u32,
    /// Subtitle cues parsed from the sidecar.
    pub event_count: usize,
    /// Subtitle packets muxed.
    pub subtitle_packets: usize,
    /// Bytes written to the target media object.
    pub bytes_written: u64,
}

/// Report returned after extracting a subtitle track to a sidecar object.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ObjectSubtitleExtractReport {
    /// Source media object key.
    pub source: Path,
    /// Target sidecar object key.
    pub target: Path,
    /// Source container format.
    pub source_format: ContainerFormat,
    /// Extracted subtitle track id.
    pub subtitle_track_id: u32,
    /// Extracted events.
    pub event_count: usize,
    /// Bytes written to the sidecar object.
    pub bytes_written: u64,
}

/// Options for burning a subtitle sidecar into decoded video frames.
#[derive(Clone, Debug, PartialEq)]
pub struct ObjectSubtitleBurnInJob {
    /// Sidecar format to parse.
    pub format: SubtitleFormat,
    /// Rendering style.
    pub style: SubtitleStyle,
    /// Specific video track to transform. The first video track is used when unset.
    pub video_track_id: Option<u32>,
    /// Preserve non-video packets in the output container when supported.
    pub preserve_non_video: bool,
    /// Codec-private data produced by the output video encoder when needed.
    pub output_video_codec_config: Option<Bytes>,
}

impl ObjectSubtitleBurnInJob {
    /// Create a burn-in job with default subtitle styling.
    #[must_use]
    pub const fn new(format: SubtitleFormat) -> Self {
        Self {
            format,
            style: SubtitleStyle {
                text_color: [255, 255, 255, 255],
                outline_color: [0, 0, 0, 255],
                box_color: [0, 0, 0, 160],
                margin_bottom: 24,
                padding: 8,
                scale: 2,
                line_gap: 4,
            },
            video_track_id: None,
            preserve_non_video: true,
            output_video_codec_config: None,
        }
    }

    /// Select a video track.
    #[must_use]
    pub const fn with_video_track(mut self, track_id: u32) -> Self {
        self.video_track_id = Some(track_id);
        self
    }

    /// Set burn-in style.
    #[must_use]
    pub const fn with_style(mut self, style: SubtitleStyle) -> Self {
        self.style = style;
        self
    }

    /// Control whether non-video packets are copied.
    #[must_use]
    pub const fn preserve_non_video(mut self, preserve: bool) -> Self {
        self.preserve_non_video = preserve;
        self
    }

    /// Attach output video codec-private data for muxers that require it.
    #[must_use]
    pub fn with_output_video_codec_config(mut self, codec_config: impl Into<Bytes>) -> Self {
        self.output_video_codec_config = Some(codec_config.into());
        self
    }
}

/// Report returned after burning subtitles into video frames.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ObjectSubtitleBurnInReport {
    /// Source media object key.
    pub source: Path,
    /// Subtitle sidecar object key.
    pub sidecar: Path,
    /// Target media object key.
    pub target: Path,
    /// Source container format.
    pub source_format: ContainerFormat,
    /// Target container format.
    pub target_format: ContainerFormat,
    /// Transformed video track id.
    pub video_track_id: u32,
    /// Parsed subtitle events.
    pub event_count: usize,
    /// Decoded frames processed.
    pub decoded_frames: usize,
    /// Encoded video packets produced.
    pub encoded_video_packets: usize,
    /// Non-video packets copied.
    pub copied_packets: usize,
    /// Bytes written to the target object.
    pub bytes_written: u64,
}

/// Object-store locations used by cross-store subtitle burn-in.
#[derive(Clone, Copy)]
pub struct ObjectSubtitleBurnInObjects<'a> {
    /// Source media object store.
    pub source_store: &'a dyn ObjectStore,
    /// Source media object key.
    pub source: &'a Path,
    /// Subtitle sidecar object store.
    pub sidecar_store: &'a dyn ObjectStore,
    /// Subtitle sidecar object key.
    pub sidecar: &'a Path,
    /// Target media object store.
    pub target_store: &'a dyn ObjectStore,
    /// Target media object key.
    pub target: &'a Path,
}

impl<'a> ObjectSubtitleBurnInObjects<'a> {
    /// Create a cross-store subtitle burn-in object set.
    #[must_use]
    pub const fn new(
        source_store: &'a dyn ObjectStore,
        source: &'a Path,
        sidecar_store: &'a dyn ObjectStore,
        sidecar: &'a Path,
        target_store: &'a dyn ObjectStore,
        target: &'a Path,
    ) -> Self {
        Self {
            source_store,
            source,
            sidecar_store,
            sidecar,
            target_store,
            target,
        }
    }
}

/// Add a subtitle sidecar as a Matroska subtitle track inside one object store.
pub async fn add_subtitle_sidecar_to_object_same_store(
    store: &dyn ObjectStore,
    source: &Path,
    sidecar: &Path,
    target: &Path,
    job: &ObjectSubtitleTrackJob,
) -> Result<ObjectSubtitleTrackReport> {
    add_subtitle_sidecar_to_object_between_stores(store, source, store, sidecar, store, target, job)
        .await
}

/// Add a subtitle sidecar as a Matroska subtitle track.
pub async fn add_subtitle_sidecar_to_object_between_stores(
    source_store: &dyn ObjectStore,
    source: &Path,
    sidecar_store: &dyn ObjectStore,
    sidecar: &Path,
    target_store: &dyn ObjectStore,
    target: &Path,
    job: &ObjectSubtitleTrackJob,
) -> Result<ObjectSubtitleTrackReport> {
    let source_format = detect_object_container_format(source_store, source).await?;
    let target_format = target_format_from_path(target)?;
    if target_format != ContainerFormat::Matroska {
        return Err(Error::Unsupported {
            operation: "subtitle mux",
            reason: "subtitle track muxing currently writes Matroska targets",
        });
    }

    let sidecar_text = read_utf8_object(sidecar_store, sidecar, job.format).await?;
    let events = parse_subtitles(job.format, &sidecar_text)?;
    let subtitle_codec = subtitle_codec_for_format(job.format);
    let subtitle_packets =
        subtitle_events_to_packets(job.track_id, subtitle_codec.clone(), job.time_base, &events)?;

    let mut demuxed = demux_object(source_store, source).await?;
    if demuxed.media.stream(job.track_id).is_some() {
        return Err(Error::IncompatibleTrack {
            track_id: job.track_id,
            reason: "target subtitle track id already exists in source media",
        });
    }

    let mut subtitle_stream = StreamInfo::new(
        job.track_id,
        MediaType::Subtitle,
        subtitle_codec,
        job.time_base,
    );
    subtitle_stream.language = job.language.clone();
    subtitle_stream.duration = subtitle_packets.iter().map(EncodedPacket::end_pts).max();
    demuxed.media.push_stream(subtitle_stream);
    demuxed.packets.extend(subtitle_packets.clone());
    sort_packets(&mut demuxed.packets);

    let bytes = mux_container_bytes(target_format, &demuxed.media, &demuxed.packets)?;
    let bytes_written = bytes.len() as u64;
    write_object_bytes(target_store, target, bytes).await?;

    Ok(ObjectSubtitleTrackReport {
        source: source.clone(),
        sidecar: sidecar.clone(),
        target: target.clone(),
        source_format,
        target_format,
        subtitle_track_id: job.track_id,
        event_count: events.len(),
        subtitle_packets: subtitle_packets.len(),
        bytes_written,
    })
}

/// Extract a subtitle track to a sidecar object in one object store.
pub async fn extract_subtitle_track_to_sidecar_same_store(
    store: &dyn ObjectStore,
    source: &Path,
    target: &Path,
    track_id: Option<u32>,
    format: SubtitleFormat,
) -> Result<ObjectSubtitleExtractReport> {
    extract_subtitle_track_to_sidecar_between_stores(store, source, store, target, track_id, format)
        .await
}

/// Extract a subtitle track to a sidecar object.
pub async fn extract_subtitle_track_to_sidecar_between_stores(
    source_store: &dyn ObjectStore,
    source: &Path,
    target_store: &dyn ObjectStore,
    target: &Path,
    track_id: Option<u32>,
    format: SubtitleFormat,
) -> Result<ObjectSubtitleExtractReport> {
    let source_format = detect_object_container_format(source_store, source).await?;
    let demuxed = demux_object(source_store, source).await?;
    let stream = select_subtitle_stream(&demuxed.media.streams, track_id)?;
    let output_codec = subtitle_codec_for_format(format);
    if subtitle_format_for_codec(&stream.codec).is_none() {
        return Err(Error::Unsupported {
            operation: "subtitle extract",
            reason: "selected subtitle codec is not supported for sidecar extraction",
        });
    }
    let packets = demuxed
        .packets
        .iter()
        .filter(|packet| packet.track_id == stream.track_id)
        .cloned()
        .map(|mut packet| {
            packet.codec = output_codec.clone();
            packet
        })
        .collect::<Vec<_>>();
    let events = subtitle_packets_to_events(&output_codec, &packets)?;
    let text = write_sidecar(format, &events);
    let bytes_written = text.len() as u64;
    write_object_bytes(target_store, target, Bytes::from(text)).await?;

    Ok(ObjectSubtitleExtractReport {
        source: source.clone(),
        target: target.clone(),
        source_format,
        subtitle_track_id: stream.track_id,
        event_count: events.len(),
        bytes_written,
    })
}

/// Burn a subtitle sidecar into video frames inside one object store.
pub async fn burn_subtitle_sidecar_into_object_same_store(
    store: &dyn ObjectStore,
    source: &Path,
    sidecar: &Path,
    target: &Path,
    job: &ObjectSubtitleBurnInJob,
    decoder: &mut dyn VideoDecoder,
    encoder: &mut dyn VideoEncoder,
) -> Result<ObjectSubtitleBurnInReport> {
    burn_subtitle_sidecar_into_object_between_stores(
        ObjectSubtitleBurnInObjects::new(store, source, store, sidecar, store, target),
        job,
        decoder,
        encoder,
    )
    .await
}

/// Burn a subtitle sidecar into video frames using caller-supplied codecs.
pub async fn burn_subtitle_sidecar_into_object_between_stores(
    objects: ObjectSubtitleBurnInObjects<'_>,
    job: &ObjectSubtitleBurnInJob,
    decoder: &mut dyn VideoDecoder,
    encoder: &mut dyn VideoEncoder,
) -> Result<ObjectSubtitleBurnInReport> {
    let source_format =
        detect_object_container_format(objects.source_store, objects.source).await?;
    let target_format = target_format_from_path(objects.target)?;
    let sidecar_text = read_utf8_object(objects.sidecar_store, objects.sidecar, job.format).await?;
    let events = parse_subtitles(job.format, &sidecar_text)?;
    let demuxed = demux_object(objects.source_store, objects.source).await?;
    let video_track_id = select_video_track(&demuxed.media.streams, job.video_track_id)?;
    let input_stream = demuxed
        .media
        .stream(video_track_id)
        .ok_or(Error::IncompatibleTrack {
            track_id: video_track_id,
            reason: "selected video track is missing from source media",
        })?;
    if decoder.codec_id() != input_stream.codec {
        return Err(Error::CodecMismatch {
            expected: input_stream.codec.clone(),
            actual: decoder.codec_id(),
        });
    }
    if encoder.codec_id().media_type() != Some(MediaType::Video) {
        return Err(Error::Unsupported {
            operation: "subtitle burn-in",
            reason: "encoder must produce a video codec",
        });
    }

    let mut output_media = demuxed.media.clone();
    let mut output_packets = Vec::<EncodedPacket>::new();
    let mut decoded_frames = 0usize;
    let mut encoded_video_packets = 0usize;
    let mut copied_packets = 0usize;
    let output_codec = encoder.codec_id();
    let mut output_dimensions = None::<(u32, u32)>;
    let mut output_time_base = None;

    for packet in &demuxed.packets {
        if packet.track_id != video_track_id {
            if job.preserve_non_video {
                output_packets.push(packet.clone());
                copied_packets += 1;
            }
            continue;
        }

        let frames = decoder.decode_packet(packet)?;
        for mut frame in frames {
            decoded_frames += 1;
            let time_ms = packet
                .time_base
                .rescale(packet.pts, TimeBase::milliseconds());
            burn_subtitles_onto_frame(&mut frame, &events, time_ms, &job.style)?;
            output_dimensions = Some((frame.width, frame.height));
            let encoded = encoder.encode_frame(&frame, packet.pts)?;
            if let Some(packet) = encoded.first() {
                output_time_base = Some(packet.time_base);
            }
            encoded_video_packets += encoded.len();
            output_packets.extend(encoded);
        }
    }

    for mut frame in decoder.flush()? {
        decoded_frames += 1;
        burn_subtitles_onto_frame(&mut frame, &events, 0, &job.style)?;
        output_dimensions = Some((frame.width, frame.height));
        let encoded = encoder.encode_frame(&frame, 0)?;
        if let Some(packet) = encoded.first() {
            output_time_base = Some(packet.time_base);
        }
        encoded_video_packets += encoded.len();
        output_packets.extend(encoded);
    }
    let finished = encoder.finish()?;
    if let Some(packet) = finished.first() {
        output_time_base = Some(packet.time_base);
    }
    encoded_video_packets += finished.len();
    output_packets.extend(finished);

    if !job.preserve_non_video {
        output_media
            .streams
            .retain(|stream| stream.track_id == video_track_id);
    }
    update_video_stream(
        output_media
            .streams
            .iter_mut()
            .find(|stream| stream.track_id == video_track_id)
            .ok_or(Error::IncompatibleTrack {
                track_id: video_track_id,
                reason: "selected video track is missing from output media",
            })?,
        output_codec,
        output_dimensions,
        output_time_base,
        job.output_video_codec_config.clone(),
    );
    sort_packets(&mut output_packets);

    let bytes = mux_container_bytes(target_format, &output_media, &output_packets)?;
    let bytes_written = bytes.len() as u64;
    write_object_bytes(objects.target_store, objects.target, bytes).await?;

    Ok(ObjectSubtitleBurnInReport {
        source: objects.source.clone(),
        sidecar: objects.sidecar.clone(),
        target: objects.target.clone(),
        source_format,
        target_format,
        video_track_id,
        event_count: events.len(),
        decoded_frames,
        encoded_video_packets,
        copied_packets,
        bytes_written,
    })
}

async fn read_utf8_object(
    store: &dyn ObjectStore,
    location: &Path,
    format: SubtitleFormat,
) -> Result<String> {
    let bytes = read_object_bytes(store, location).await?;
    String::from_utf8(bytes.to_vec()).map_err(|err| Error::Parse {
        format: match format {
            SubtitleFormat::Srt => "srt",
            SubtitleFormat::WebVtt => "webvtt",
        },
        message: format!("subtitle sidecar is not valid UTF-8: {err}"),
    })
}

fn write_sidecar(format: SubtitleFormat, events: &[crate::subtitle::SubtitleEvent]) -> String {
    match format {
        SubtitleFormat::Srt => write_srt(events),
        SubtitleFormat::WebVtt => write_webvtt(events),
    }
}

fn select_subtitle_stream(streams: &[StreamInfo], requested: Option<u32>) -> Result<&StreamInfo> {
    if let Some(track_id) = requested {
        let stream = streams
            .iter()
            .find(|stream| stream.track_id == track_id)
            .ok_or(Error::IncompatibleTrack {
                track_id,
                reason: "requested subtitle track is missing",
            })?;
        if stream.media_type != MediaType::Subtitle {
            return Err(Error::IncompatibleTrack {
                track_id,
                reason: "requested track is not subtitle",
            });
        }
        return Ok(stream);
    }

    streams
        .iter()
        .find(|stream| stream.media_type == MediaType::Subtitle)
        .ok_or(Error::Unsupported {
            operation: "subtitle extract",
            reason: "source media has no subtitle track",
        })
}

fn select_video_track(streams: &[StreamInfo], requested: Option<u32>) -> Result<u32> {
    if let Some(track_id) = requested {
        let stream = streams
            .iter()
            .find(|stream| stream.track_id == track_id)
            .ok_or(Error::IncompatibleTrack {
                track_id,
                reason: "requested video track is missing",
            })?;
        if stream.media_type != MediaType::Video {
            return Err(Error::IncompatibleTrack {
                track_id,
                reason: "requested track is not video",
            });
        }
        return Ok(track_id);
    }

    streams
        .iter()
        .find(|stream| stream.media_type == MediaType::Video)
        .map(|stream| stream.track_id)
        .ok_or(Error::Unsupported {
            operation: "subtitle burn-in",
            reason: "source media has no video track",
        })
}

fn update_video_stream(
    stream: &mut StreamInfo,
    codec: CodecId,
    dimensions: Option<(u32, u32)>,
    time_base: Option<TimeBase>,
    codec_config: Option<Bytes>,
) {
    let codec_changed = stream.codec != codec;
    stream.codec = codec;
    if let Some(time_base) = time_base {
        stream.time_base = time_base;
    }
    if let Some((width, height)) = dimensions {
        stream.width = Some(width);
        stream.height = Some(height);
    }
    if let Some(codec_config) = codec_config {
        stream.codec_config = Some(codec_config);
    } else if codec_changed {
        stream.codec_config = None;
    }
}

fn sort_packets(packets: &mut [EncodedPacket]) {
    packets.sort_by(|left, right| {
        let left_ts = left.time_base.ticks_to_seconds(left.decode_order_ts());
        let right_ts = right.time_base.ticks_to_seconds(right.decode_order_ts());
        left_ts
            .total_cmp(&right_ts)
            .then_with(|| left.track_id.cmp(&right.track_id))
            .then_with(|| left.pts.cmp(&right.pts))
    });
}

fn target_format_from_path(target: &Path) -> Result<ContainerFormat> {
    ContainerFormat::from_path(target).ok_or(Error::Unsupported {
        operation: "container detection",
        reason: "target object key extension is not a recognized media container",
    })
}

fn mux_container_bytes(
    format: ContainerFormat,
    media: &MediaInfo,
    packets: &[EncodedPacket],
) -> Result<Bytes> {
    match format {
        ContainerFormat::Mp4 | ContainerFormat::QuickTime => {
            containers::mux_iso_bmff_bytes(format, media, packets)
        }
        ContainerFormat::Matroska | ContainerFormat::WebM => {
            containers::mux_matroska_bytes(format, media, packets)
        }
        ContainerFormat::MpegTs => containers::mux_mpeg_ts_bytes(media, packets),
        ContainerFormat::RawElementary => containers::mux_elementary_bytes(media, packets),
        ContainerFormat::Ogg => containers::mux_ogg_bytes(media, packets),
        ContainerFormat::Wav => containers::mux_wav_bytes(media, packets),
        ContainerFormat::MpegPs
        | ContainerFormat::Avi
        | ContainerFormat::Flv
        | ContainerFormat::Aiff => Err(Error::Unsupported {
            operation: "subtitle object",
            reason: "no mux adapter is wired for this target container format yet",
        }),
    }
}
