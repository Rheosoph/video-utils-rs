use crate::{
    bitstream::{h264::h264_packet_to_length_prefixed, h265::h265_packet_to_length_prefixed},
    codec::{CodecId, MediaType},
    container::{ContainerFormat, ContainerMuxer},
    error::{Error, Result},
    media::{MediaInfo, StreamInfo},
    packet::{EncodedPacket, validate_monotonic_by_track},
};
use bytes::{Bytes, BytesMut};
use std::{cmp::Ordering, collections::BTreeMap};

const EBML_HEADER_ID: u64 = 0x1A45DFA3;
const SEGMENT_ID: u64 = 0x18538067;
const INFO_ID: u64 = 0x1549A966;
const TRACKS_ID: u64 = 0x1654AE6B;
const TRACK_ENTRY_ID: u64 = 0xAE;
const CLUSTER_ID: u64 = 0x1F43B675;
const SIMPLE_BLOCK_ID: u64 = 0xA3;
const TIMESTAMP_SCALE_NS: u64 = 1_000_000;
const CLUSTER_MAX_TIMECODE: i64 = i16::MAX as i64;

/// Matroska/WebM muxer backed by a small in-memory EBML writer.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct MatroskaMuxer {
    format: ContainerFormat,
}

impl MatroskaMuxer {
    /// Create a Matroska/WebM muxer.
    pub fn new(format: ContainerFormat) -> Result<Self> {
        if !matches!(format, ContainerFormat::Matroska | ContainerFormat::WebM) {
            return Err(Error::Unsupported {
                operation: "matroska mux",
                reason: "only Matroska and WebM containers are handled by MatroskaMuxer",
            });
        }

        Ok(Self { format })
    }
}

impl ContainerMuxer for MatroskaMuxer {
    fn container_format(&self) -> ContainerFormat {
        self.format
    }

    fn supports_stream(&self, stream: &StreamInfo) -> bool {
        if !self.format.supports_stream(stream) {
            return false;
        }
        matroska_codec_id(&stream.codec).is_some()
    }

    fn mux_bytes(&self, media: &MediaInfo, packets: &[EncodedPacket]) -> Result<Bytes> {
        mux_matroska_bytes(self.format, media, packets)
    }
}

/// Mux stream metadata and encoded packets into Matroska/WebM bytes.
pub fn mux_matroska_bytes(
    format: ContainerFormat,
    media: &MediaInfo,
    packets: &[EncodedPacket],
) -> Result<Bytes> {
    let muxer = MatroskaMuxer::new(format)?;
    validate_inputs(&muxer, media, packets)?;

    let track_context = build_track_context(media, packets, format)?;
    let mut body = BytesMut::new();
    write_element(&mut body, INFO_ID, &build_info(media)?);
    write_element(&mut body, TRACKS_ID, &build_tracks(media, &track_context)?);
    write_clusters(&mut body, media, packets, &track_context, format)?;

    let mut out = BytesMut::new();
    write_element(&mut out, EBML_HEADER_ID, &build_ebml_header(format));
    write_element(&mut out, SEGMENT_ID, &body);
    Ok(out.freeze())
}

#[derive(Clone, Debug)]
struct TrackMuxContext {
    codec_id: &'static str,
    track_type: u64,
    default_duration_ns: Option<u64>,
    codec_private: Option<Bytes>,
}

fn validate_inputs(
    muxer: &MatroskaMuxer,
    media: &MediaInfo,
    packets: &[EncodedPacket],
) -> Result<()> {
    if media.streams.is_empty() || packets.is_empty() {
        return Err(Error::EmptyInput);
    }
    validate_monotonic_by_track(packets)?;

    for stream in &media.streams {
        if stream.track_id == 0 {
            return Err(Error::Unsupported {
                operation: "matroska mux",
                reason: "Matroska track numbers must be non-zero",
            });
        }
        if !muxer.supports_stream(stream) {
            return Err(Error::Unsupported {
                operation: "matroska mux",
                reason: "stream is not supported by the target Matroska/WebM muxer",
            });
        }
        if !packets
            .iter()
            .any(|packet| packet.track_id == stream.track_id)
        {
            return Err(Error::IncompatibleTrack {
                track_id: stream.track_id,
                reason: "declared stream has no packets to mux",
            });
        }
    }

    for packet in packets {
        let stream = media
            .stream(packet.track_id)
            .ok_or(Error::IncompatibleTrack {
                track_id: packet.track_id,
                reason: "packet references a stream missing from MediaInfo",
            })?;
        if packet.codec != stream.codec {
            return Err(Error::CodecMismatch {
                expected: stream.codec.clone(),
                actual: packet.codec.clone(),
            });
        }
        if packet.time_base != stream.time_base {
            return Err(Error::TimeBaseMismatch {
                expected: stream.time_base,
                actual: packet.time_base,
            });
        }
        if packet.pts < 0 || packet.dts.is_some_and(|dts| dts < 0) {
            return Err(Error::InvalidPacketTiming {
                reason: "negative timestamps cannot be muxed into Matroska/WebM",
            });
        }
    }

    Ok(())
}

fn build_track_context(
    media: &MediaInfo,
    packets: &[EncodedPacket],
    format: ContainerFormat,
) -> Result<BTreeMap<u32, TrackMuxContext>> {
    let mut contexts = BTreeMap::new();
    for stream in &media.streams {
        let codec_id = matroska_codec_id(&stream.codec).ok_or(Error::Unsupported {
            operation: "matroska mux",
            reason: "codec has no Matroska/WebM codec id mapping",
        })?;
        let default_duration_ns = infer_default_duration_ns(stream, packets)?;
        let codec_private = codec_private_for_stream(stream, format)?;
        contexts.insert(
            stream.track_id,
            TrackMuxContext {
                codec_id,
                track_type: matroska_track_type(stream.media_type)?,
                default_duration_ns,
                codec_private,
            },
        );
    }
    Ok(contexts)
}

fn build_ebml_header(format: ContainerFormat) -> BytesMut {
    let mut out = BytesMut::new();
    write_uint_element(&mut out, 0x4286, 1);
    write_uint_element(&mut out, 0x42F7, 1);
    write_uint_element(&mut out, 0x42F2, 4);
    write_uint_element(&mut out, 0x42F3, 8);
    write_string_element(&mut out, 0x4282, doc_type(format));
    write_uint_element(&mut out, 0x4287, 4);
    write_uint_element(&mut out, 0x4285, 2);
    out
}

fn build_info(media: &MediaInfo) -> Result<BytesMut> {
    let mut out = BytesMut::new();
    write_uint_element(&mut out, 0x2AD7B1, TIMESTAMP_SCALE_NS);
    write_string_element(&mut out, 0x4D80, "video-utils-rs");
    write_string_element(&mut out, 0x5741, "video-utils-rs");
    if let Some(duration_seconds) = media.duration_seconds.filter(|value| value.is_finite()) {
        write_float_element(&mut out, 0x4489, duration_seconds * 1000.0);
    }
    Ok(out)
}

fn build_tracks(media: &MediaInfo, contexts: &BTreeMap<u32, TrackMuxContext>) -> Result<BytesMut> {
    let mut tracks = BytesMut::new();
    for stream in &media.streams {
        let context = contexts
            .get(&stream.track_id)
            .ok_or(Error::IncompatibleTrack {
                track_id: stream.track_id,
                reason: "missing Matroska track context",
            })?;
        let mut entry = BytesMut::new();
        write_uint_element(&mut entry, 0xD7, u64::from(stream.track_id));
        write_uint_element(&mut entry, 0x73C5, u64::from(stream.track_id));
        write_uint_element(&mut entry, 0x83, context.track_type);
        write_uint_element(&mut entry, 0x9C, 0);
        if let Some(default_duration_ns) = context.default_duration_ns {
            write_uint_element(&mut entry, 0x23E383, default_duration_ns);
        }
        write_string_element(&mut entry, 0x86, context.codec_id);
        if let Some(language) = &stream.language {
            write_string_element(&mut entry, 0x22B59C, language);
        }
        if let Some(name) = stream.tags.get("name") {
            write_string_element(&mut entry, 0x536E, name);
        }
        if let Some(codec_private) = &context.codec_private {
            write_binary_element(&mut entry, 0x63A2, codec_private);
        }

        match stream.media_type {
            MediaType::Video => {
                let mut video = BytesMut::new();
                write_uint_element(
                    &mut video,
                    0xB0,
                    u64::from(stream.width.ok_or(Error::Unsupported {
                        operation: "matroska mux",
                        reason: "video stream width is required for Matroska/WebM muxing",
                    })?),
                );
                write_uint_element(
                    &mut video,
                    0xBA,
                    u64::from(stream.height.ok_or(Error::Unsupported {
                        operation: "matroska mux",
                        reason: "video stream height is required for Matroska/WebM muxing",
                    })?),
                );
                write_element(&mut entry, 0xE0, &video);
            }
            MediaType::Audio => {
                let mut audio = BytesMut::new();
                write_float_element(
                    &mut audio,
                    0xB5,
                    stream.sample_rate.ok_or(Error::Unsupported {
                        operation: "matroska mux",
                        reason: "audio sample rate is required for Matroska/WebM muxing",
                    })? as f64,
                );
                write_uint_element(
                    &mut audio,
                    0x9F,
                    u64::from(stream.channels.ok_or(Error::Unsupported {
                        operation: "matroska mux",
                        reason: "audio channel count is required for Matroska/WebM muxing",
                    })?),
                );
                write_element(&mut entry, 0xE1, &audio);
            }
            MediaType::Subtitle => {}
            MediaType::Image | MediaType::Data => {
                return Err(Error::Unsupported {
                    operation: "matroska mux",
                    reason: "image and data tracks are not written by the Matroska/WebM muxer",
                });
            }
        }

        write_element(&mut tracks, TRACK_ENTRY_ID, &entry);
    }

    Ok(tracks)
}

fn write_clusters(
    out: &mut BytesMut,
    media: &MediaInfo,
    packets: &[EncodedPacket],
    contexts: &BTreeMap<u32, TrackMuxContext>,
    format: ContainerFormat,
) -> Result<()> {
    let mut ordered = packets.iter().collect::<Vec<_>>();
    ordered.sort_by(|left, right| packet_write_order(left, right, media));

    let mut cluster = BytesMut::new();
    let mut cluster_timecode = None::<i64>;
    for packet in ordered {
        let packet_timecode = packet_to_matroska_ticks(packet)?;
        if cluster_timecode.is_none()
            || packet_timecode - cluster_timecode.unwrap_or(0) > CLUSTER_MAX_TIMECODE
        {
            flush_cluster(out, &mut cluster, cluster_timecode);
            cluster_timecode = Some(packet_timecode);
            write_uint_element(&mut cluster, 0xE7, packet_timecode as u64);
        }

        let relative = packet_timecode - cluster_timecode.unwrap_or(packet_timecode);
        if !(i16::MIN as i64..=i16::MAX as i64).contains(&relative) {
            return Err(Error::InvalidPacketTiming {
                reason: "packet timestamp is outside Matroska cluster relative timecode range",
            });
        }
        let context = contexts
            .get(&packet.track_id)
            .ok_or(Error::IncompatibleTrack {
                track_id: packet.track_id,
                reason: "missing Matroska track context",
            })?;
        write_simple_block(&mut cluster, packet, context, relative as i16, format)?;
    }
    flush_cluster(out, &mut cluster, cluster_timecode);

    Ok(())
}

fn flush_cluster(out: &mut BytesMut, cluster: &mut BytesMut, cluster_timecode: Option<i64>) {
    if cluster_timecode.is_some() && !cluster.is_empty() {
        write_element(out, CLUSTER_ID, cluster);
        cluster.clear();
    }
}

fn write_simple_block(
    out: &mut BytesMut,
    packet: &EncodedPacket,
    context: &TrackMuxContext,
    relative_timecode: i16,
    format: ContainerFormat,
) -> Result<()> {
    let mut block = BytesMut::new();
    write_track_number_vint(&mut block, packet.track_id)?;
    block.extend_from_slice(&relative_timecode.to_be_bytes());
    let flags = if packet.is_keyframe { 0x80 } else { 0x00 };
    block.extend_from_slice(&[flags]);
    let payload = packet_payload_for_matroska(packet, context, format)?;
    block.extend_from_slice(&payload);
    write_element(out, SIMPLE_BLOCK_ID, &block);
    Ok(())
}

fn packet_payload_for_matroska(
    packet: &EncodedPacket,
    context: &TrackMuxContext,
    _format: ContainerFormat,
) -> Result<Bytes> {
    match packet.codec {
        CodecId::H264 => h264_packet_to_length_prefixed(
            packet,
            context.codec_private.as_ref().ok_or(Error::Unsupported {
                operation: "matroska mux",
                reason: "H.264 Matroska muxing requires AVC decoder config",
            })?,
        ),
        CodecId::H265 => h265_packet_to_length_prefixed(
            packet,
            context.codec_private.as_ref().ok_or(Error::Unsupported {
                operation: "matroska mux",
                reason: "H.265 Matroska muxing requires HEVC decoder config",
            })?,
        ),
        _ => Ok(packet.data.clone()),
    }
}

fn matroska_codec_id(codec: &CodecId) -> Option<&'static str> {
    match codec {
        CodecId::H264 => Some("V_MPEG4/ISO/AVC"),
        CodecId::H265 => Some("V_MPEGH/ISO/HEVC"),
        CodecId::AV1 => Some("V_AV1"),
        CodecId::VP8 => Some("V_VP8"),
        CodecId::VP9 => Some("V_VP9"),
        CodecId::Mpeg1Video => Some("V_MPEG1"),
        CodecId::Mpeg2Video => Some("V_MPEG2"),
        CodecId::Mpeg4Part2 => Some("V_MPEG4/ISO/ASP"),
        CodecId::ProRes => Some("V_PRORES"),
        CodecId::Theora => Some("V_THEORA"),
        CodecId::RawVideo => Some("V_UNCOMPRESSED"),
        CodecId::Aac => Some("A_AAC"),
        CodecId::Ac3 => Some("A_AC3"),
        CodecId::Eac3 => Some("A_EAC3"),
        CodecId::Alac => Some("A_ALAC"),
        CodecId::Opus => Some("A_OPUS"),
        CodecId::Flac => Some("A_FLAC"),
        CodecId::Mp1 => Some("A_MPEG/L1"),
        CodecId::Mp2 => Some("A_MPEG/L2"),
        CodecId::Mp3 => Some("A_MPEG/L3"),
        CodecId::Pcm => Some("A_PCM/INT/LIT"),
        CodecId::Vorbis => Some("A_VORBIS"),
        CodecId::Speex => Some("A_SPEEX"),
        CodecId::Dts => Some("A_DTS"),
        CodecId::WavPack => Some("A_WAVPACK4"),
        CodecId::Srt => Some("S_TEXT/UTF8"),
        CodecId::WebVtt => Some("S_TEXT/WEBVTT"),
        CodecId::Adpcm
        | CodecId::Dirac
        | CodecId::Wma
        | CodecId::Png
        | CodecId::Jpeg
        | CodecId::Gif
        | CodecId::WebP
        | CodecId::Avif
        | CodecId::Unknown(_) => None,
    }
}

fn matroska_track_type(media_type: MediaType) -> Result<u64> {
    match media_type {
        MediaType::Video => Ok(1),
        MediaType::Audio => Ok(2),
        MediaType::Subtitle => Ok(17),
        MediaType::Image | MediaType::Data => Err(Error::Unsupported {
            operation: "matroska mux",
            reason: "image and data tracks are not written by the Matroska/WebM muxer",
        }),
    }
}

fn codec_private_for_stream(
    stream: &StreamInfo,
    _format: ContainerFormat,
) -> Result<Option<Bytes>> {
    if let Some(codec_private) = &stream.codec_config {
        return Ok(Some(codec_private.clone()));
    }

    match stream.codec {
        CodecId::H264 => Err(Error::Unsupported {
            operation: "matroska mux",
            reason: "H.264 Matroska muxing requires AVC decoder config",
        }),
        CodecId::H265 => Err(Error::Unsupported {
            operation: "matroska mux",
            reason: "H.265 Matroska muxing requires HEVC decoder config",
        }),
        CodecId::Opus => Ok(Some(opus_head(stream)?)),
        CodecId::Vorbis => Err(Error::Unsupported {
            operation: "matroska mux",
            reason: "Vorbis Matroska/WebM muxing requires codec private header data",
        }),
        _ => Ok(None),
    }
}

fn opus_head(stream: &StreamInfo) -> Result<Bytes> {
    let channels = stream.channels.ok_or(Error::Unsupported {
        operation: "matroska mux",
        reason: "Opus muxing requires a channel count",
    })?;
    let sample_rate = stream.sample_rate.unwrap_or(48_000);
    let mut out = BytesMut::with_capacity(19);
    out.extend_from_slice(b"OpusHead");
    out.extend_from_slice(&[1, channels as u8]);
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&sample_rate.to_le_bytes());
    out.extend_from_slice(&0i16.to_le_bytes());
    out.extend_from_slice(&[0]);
    Ok(out.freeze())
}

fn infer_default_duration_ns(
    stream: &StreamInfo,
    packets: &[EncodedPacket],
) -> Result<Option<u64>> {
    let mut durations = packets
        .iter()
        .filter(|packet| packet.track_id == stream.track_id && packet.duration > 0)
        .map(|packet| packet.duration_seconds())
        .filter(|seconds| seconds.is_finite() && *seconds > 0.0)
        .collect::<Vec<_>>();
    durations.sort_by(f64::total_cmp);
    let Some(duration_seconds) = durations.first().copied() else {
        return Ok(None);
    };
    let ns = (duration_seconds * 1_000_000_000.0).round();
    if ns > u64::MAX as f64 {
        return Err(Error::InvalidPacketTiming {
            reason: "packet duration is too large for Matroska DefaultDuration",
        });
    }
    Ok(Some(ns as u64))
}

fn packet_to_matroska_ticks(packet: &EncodedPacket) -> Result<i64> {
    let seconds = packet.time_base.ticks_to_seconds(packet.decode_order_ts());
    let ticks = (seconds * 1000.0).round();
    if !ticks.is_finite() || ticks < 0.0 || ticks > i64::MAX as f64 {
        return Err(Error::InvalidPacketTiming {
            reason: "packet timestamp cannot be represented by Matroska timecode",
        });
    }
    Ok(ticks as i64)
}

fn packet_write_order(left: &EncodedPacket, right: &EncodedPacket, media: &MediaInfo) -> Ordering {
    let left_ts = left.time_base.ticks_to_seconds(left.decode_order_ts());
    let right_ts = right.time_base.ticks_to_seconds(right.decode_order_ts());
    left_ts
        .total_cmp(&right_ts)
        .then_with(|| media_priority(left, media).cmp(&media_priority(right, media)))
        .then_with(|| left.track_id.cmp(&right.track_id))
        .then_with(|| left.pts.cmp(&right.pts))
}

fn media_priority(packet: &EncodedPacket, media: &MediaInfo) -> u8 {
    match media
        .stream(packet.track_id)
        .map(|stream| stream.media_type)
    {
        Some(MediaType::Video) => 0,
        Some(MediaType::Audio) => 1,
        Some(MediaType::Subtitle) => 2,
        _ => 3,
    }
}

fn doc_type(format: ContainerFormat) -> &'static str {
    match format {
        ContainerFormat::WebM => "webm",
        ContainerFormat::Matroska => "matroska",
        _ => "matroska",
    }
}

fn write_element(out: &mut BytesMut, id: u64, payload: &[u8]) {
    write_id(out, id);
    write_size(out, payload.len() as u64);
    out.extend_from_slice(payload);
}

fn write_uint_element(out: &mut BytesMut, id: u64, value: u64) {
    let mut data = BytesMut::new();
    write_uint(&mut data, value);
    write_element(out, id, &data);
}

fn write_string_element(out: &mut BytesMut, id: u64, value: &str) {
    write_element(out, id, value.as_bytes());
}

fn write_binary_element(out: &mut BytesMut, id: u64, value: &[u8]) {
    write_element(out, id, value);
}

fn write_float_element(out: &mut BytesMut, id: u64, value: f64) {
    let mut data = BytesMut::new();
    data.extend_from_slice(&value.to_be_bytes());
    write_element(out, id, &data);
}

fn write_uint(out: &mut BytesMut, value: u64) {
    let bytes = value.to_be_bytes();
    let first_non_zero = bytes.iter().position(|byte| *byte != 0).unwrap_or(7);
    out.extend_from_slice(&bytes[first_non_zero..]);
}

fn write_id(out: &mut BytesMut, id: u64) {
    let bytes = id.to_be_bytes();
    let first_non_zero = bytes.iter().position(|byte| *byte != 0).unwrap_or(7);
    out.extend_from_slice(&bytes[first_non_zero..]);
}

fn write_size(out: &mut BytesMut, size: u64) {
    if size < 0x7f {
        out.extend_from_slice(&[(0x80 | size as u8)]);
    } else if size < 0x3fff {
        out.extend_from_slice(&[(0x40 | ((size >> 8) as u8)), size as u8]);
    } else if size < 0x1f_ffff {
        out.extend_from_slice(&[0x20 | ((size >> 16) as u8), (size >> 8) as u8, size as u8]);
    } else if size < 0x0fff_ffff {
        out.extend_from_slice(&[
            0x10 | ((size >> 24) as u8),
            (size >> 16) as u8,
            (size >> 8) as u8,
            size as u8,
        ]);
    } else {
        out.extend_from_slice(&[
            0x01,
            (size >> 48) as u8,
            (size >> 40) as u8,
            (size >> 32) as u8,
            (size >> 24) as u8,
            (size >> 16) as u8,
            (size >> 8) as u8,
            size as u8,
        ]);
    }
}

fn write_track_number_vint(out: &mut BytesMut, track_id: u32) -> Result<()> {
    if track_id == 0 {
        return Err(Error::Unsupported {
            operation: "matroska mux",
            reason: "Matroska track numbers must be non-zero",
        });
    }
    if track_id < 0x7f {
        out.extend_from_slice(&[0x80 | track_id as u8]);
    } else if track_id < 0x3fff {
        out.extend_from_slice(&[0x40 | ((track_id >> 8) as u8), (track_id & 0xff) as u8]);
    } else {
        return Err(Error::Unsupported {
            operation: "matroska mux",
            reason: "Matroska track numbers above 16382 are not supported",
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ebml_size_uses_shortest_vint() {
        let mut out = BytesMut::new();
        write_size(&mut out, 126);
        write_size(&mut out, 127);

        assert_eq!(&out[..], &[0xfe, 0x40, 0x7f]);
    }
}
