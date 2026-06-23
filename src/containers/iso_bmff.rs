use crate::{
    codec::{CodecId, MediaType},
    container::{ContainerDemuxer, ContainerFormat, DemuxedMedia},
    error::{Error, Result},
    media::{MediaInfo, StreamInfo},
    packet::{EncodedPacket, validate_monotonic_by_track},
    time::TimeBase,
};
use bytes::Bytes;
use re_mp4::{Mp4, StsdBoxContent, Track, TrackKind};

/// MP4/MOV demuxer backed by `re_mp4`.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct IsoBmffDemuxer {
    format: ContainerFormat,
}

impl IsoBmffDemuxer {
    /// Create an MP4/MOV demuxer for the given ISO BMFF family format.
    pub fn new(format: ContainerFormat) -> Result<Self> {
        if !matches!(format, ContainerFormat::Mp4 | ContainerFormat::QuickTime) {
            return Err(Error::Unsupported {
                operation: "iso-bmff demux",
                reason: "only MP4 and QuickTime containers are handled by IsoBmffDemuxer",
            });
        }

        Ok(Self { format })
    }
}

impl ContainerDemuxer for IsoBmffDemuxer {
    fn container_format(&self) -> ContainerFormat {
        self.format
    }

    fn demux_bytes(&self, bytes: &Bytes) -> Result<DemuxedMedia> {
        demux_iso_bmff_bytes(self.format, bytes)
    }
}

/// Probe stream metadata from MP4/MOV bytes.
pub fn probe_iso_bmff_bytes(format: ContainerFormat, bytes: &Bytes) -> Result<MediaInfo> {
    demux_iso_bmff_bytes(format, bytes).map(|demuxed| demuxed.media)
}

/// Demux MP4/MOV bytes into stream metadata and encoded packets.
pub fn demux_iso_bmff_bytes(format: ContainerFormat, bytes: &Bytes) -> Result<DemuxedMedia> {
    let demuxer = IsoBmffDemuxer::new(format)?;
    let mp4 = Mp4::read_bytes(bytes).map_err(|err| parse_error(format, err))?;

    let mut media = MediaInfo::default();
    let mut packets = Vec::new();

    for track in mp4.tracks().values() {
        let stream = stream_info_for_track(&mp4, track)?;
        let codec = stream.codec.clone();
        let time_base = stream.time_base;
        let track_id = stream.track_id;
        media.push_stream(stream);

        for sample in &track.samples {
            let range = sample.byte_range();
            let Some(data) = bytes.get(range.clone()) else {
                return Err(Error::Parse {
                    format: format.as_str(),
                    message: format!(
                        "sample range {}..{} is outside object body length {}",
                        range.start,
                        range.end,
                        bytes.len()
                    ),
                });
            };

            let duration = u64_to_i64(sample.duration, format, "sample duration")?;
            let mut packet = EncodedPacket::new(
                track_id,
                codec.clone(),
                sample.composition_timestamp,
                duration,
                time_base,
                Bytes::copy_from_slice(data),
            )
            .with_keyframe(sample.is_sync);

            packet.dts = Some(sample.decode_timestamp);
            packets.push(packet);
        }
    }

    sort_packets_by_decode_time(&mut packets);
    validate_monotonic_by_track(&packets)?;
    media.duration_seconds = infer_duration_seconds(&media);

    Ok(DemuxedMedia::new(
        demuxer.container_format(),
        media,
        packets,
    ))
}

fn stream_info_for_track(mp4: &Mp4, track: &Track) -> Result<StreamInfo> {
    let format = ContainerFormat::Mp4;
    let (media_type, codec) = codec_and_media_type(mp4, track);
    let timescale = u64_to_i32(track.timescale, format, "track timescale")?;
    let time_base = TimeBase::new(1, timescale)?;
    let mut stream = StreamInfo::new(track.track_id, media_type, codec, time_base);
    stream.duration = Some(u64_to_i64(track.duration, format, "track duration")?);

    match media_type {
        MediaType::Video => {
            if track.width > 0 && track.height > 0 {
                stream = stream.with_dimensions(u32::from(track.width), u32::from(track.height));
            }
        }
        MediaType::Audio => {
            if let StsdBoxContent::Mp4a(mp4a) = &track.trak(mp4).mdia.minf.stbl.stsd.contents {
                stream =
                    stream.with_audio_format(u32::from(mp4a.samplerate.value()), mp4a.channelcount);
            }
        }
        _ => {}
    }

    if let Some(config) = track.raw_codec_config(mp4) {
        stream.codec_config = Some(Bytes::from(config));
    }
    if let Some(codec_string) = track.codec_string(mp4) {
        stream.tags.insert("codec_string".to_owned(), codec_string);
    }

    Ok(stream)
}

fn codec_and_media_type(mp4: &Mp4, track: &Track) -> (MediaType, CodecId) {
    let stsd = &track.trak(mp4).mdia.minf.stbl.stsd.contents;
    match stsd {
        StsdBoxContent::Av01(_) => (MediaType::Video, CodecId::AV1),
        StsdBoxContent::Avc1(_) => (MediaType::Video, CodecId::H264),
        StsdBoxContent::Hev1(_) | StsdBoxContent::Hvc1(_) => (MediaType::Video, CodecId::H265),
        StsdBoxContent::Vp08(_) => (MediaType::Video, CodecId::VP8),
        StsdBoxContent::Vp09(_) => (MediaType::Video, CodecId::VP9),
        StsdBoxContent::Mp4a(_) => (MediaType::Audio, CodecId::Aac),
        StsdBoxContent::Tx3g(_) => (MediaType::Subtitle, CodecId::Unknown("tx3g".to_owned())),
        StsdBoxContent::Unknown(fourcc) => {
            let media_type = match track.kind {
                Some(TrackKind::Video) => MediaType::Video,
                Some(TrackKind::Audio) => MediaType::Audio,
                Some(TrackKind::Subtitle) => MediaType::Subtitle,
                None => MediaType::Data,
            };
            (media_type, CodecId::Unknown(fourcc.to_string()))
        }
    }
}

fn sort_packets_by_decode_time(packets: &mut [EncodedPacket]) {
    packets.sort_by(|left, right| {
        let left_ts = left.time_base.ticks_to_seconds(left.decode_order_ts());
        let right_ts = right.time_base.ticks_to_seconds(right.decode_order_ts());
        left_ts
            .total_cmp(&right_ts)
            .then_with(|| left.track_id.cmp(&right.track_id))
            .then_with(|| left.pts.cmp(&right.pts))
    });
}

fn infer_duration_seconds(media: &MediaInfo) -> Option<f64> {
    media
        .streams
        .iter()
        .filter_map(|stream| stream.duration_seconds())
        .max_by(f64::total_cmp)
}

fn parse_error(format: ContainerFormat, err: re_mp4::Error) -> Error {
    Error::Parse {
        format: format.as_str(),
        message: err.to_string(),
    }
}

fn u64_to_i32(value: u64, format: ContainerFormat, field: &'static str) -> Result<i32> {
    value.try_into().map_err(|_| Error::Parse {
        format: format.as_str(),
        message: format!("{field} is too large for TimeBase"),
    })
}

fn u64_to_i64(value: u64, format: ContainerFormat, field: &'static str) -> Result<i64> {
    value.try_into().map_err(|_| Error::Parse {
        format: format.as_str(),
        message: format!("{field} is too large for packet timing"),
    })
}
