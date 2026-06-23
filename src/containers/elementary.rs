use crate::{
    bitstream::{
        aac::aac_packet_to_adts,
        h264::{AnnexBNalIter, h264_packet_to_annex_b},
        h265::h265_packet_to_annex_b,
    },
    codec::{CodecId, MediaType},
    container::{ContainerDemuxer, ContainerFormat, ContainerMuxer, DemuxedMedia},
    error::{Error, Result},
    media::{MediaInfo, StreamInfo},
    packet::{EncodedPacket, validate_monotonic_by_track},
    time::TimeBase,
};
use bytes::{Bytes, BytesMut};
use object_store::path::Path;

const ANNEX_B_START_CODE: &[u8] = &[0, 0, 0, 1];
const VIDEO_TIME_BASE_DEN: i32 = 90_000;
const DEFAULT_VIDEO_FRAME_DURATION: i64 = 3_000;

/// Raw elementary stream demuxer.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct ElementaryDemuxer {
    codec: CodecId,
}

impl ElementaryDemuxer {
    /// Create a raw elementary demuxer for one codec.
    #[must_use]
    pub fn new(codec: CodecId) -> Self {
        Self { codec }
    }
}

impl ContainerDemuxer for ElementaryDemuxer {
    fn container_format(&self) -> ContainerFormat {
        ContainerFormat::RawElementary
    }

    fn demux_bytes(&self, bytes: &Bytes) -> Result<DemuxedMedia> {
        demux_elementary_bytes(self.codec.clone(), bytes)
    }
}

/// Raw elementary stream muxer.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub struct ElementaryMuxer;

impl ElementaryMuxer {
    /// Create a raw elementary muxer.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl ContainerMuxer for ElementaryMuxer {
    fn container_format(&self) -> ContainerFormat {
        ContainerFormat::RawElementary
    }

    fn supports_stream(&self, stream: &StreamInfo) -> bool {
        stream.codec.media_type() == Some(stream.media_type)
            && !matches!(stream.codec, CodecId::Unknown(_))
    }

    fn mux_bytes(&self, media: &MediaInfo, packets: &[EncodedPacket]) -> Result<Bytes> {
        mux_elementary_bytes(media, packets)
    }
}

/// Infer a raw elementary codec from an object key extension.
#[must_use]
pub fn detect_elementary_codec_from_path(path: &Path) -> Option<CodecId> {
    path.extension()
        .and_then(detect_elementary_codec_from_extension)
}

/// Infer a raw elementary codec from an extension.
#[must_use]
pub fn detect_elementary_codec_from_extension(extension: &str) -> Option<CodecId> {
    match extension
        .trim_start_matches('.')
        .to_ascii_lowercase()
        .as_str()
    {
        "h264" | "avc" => Some(CodecId::H264),
        "h265" | "hevc" => Some(CodecId::H265),
        "av1" | "ivf" => Some(CodecId::AV1),
        "aac" => Some(CodecId::Aac),
        "ac3" => Some(CodecId::Ac3),
        "eac3" => Some(CodecId::Eac3),
        "mp1" => Some(CodecId::Mp1),
        "mp2" => Some(CodecId::Mp2),
        "mp3" => Some(CodecId::Mp3),
        "flac" => Some(CodecId::Flac),
        _ => None,
    }
}

/// Demux a raw elementary object after inferring its codec from the key.
pub fn demux_elementary_bytes_from_path(path: &Path, bytes: &Bytes) -> Result<DemuxedMedia> {
    let codec = detect_elementary_codec_from_path(path).ok_or(Error::Unsupported {
        operation: "elementary demux",
        reason: "raw elementary demuxing requires a recognized codec extension",
    })?;
    demux_elementary_bytes(codec, bytes)
}

/// Demux raw elementary bytes into one packet stream.
pub fn demux_elementary_bytes(codec: CodecId, bytes: &Bytes) -> Result<DemuxedMedia> {
    if bytes.is_empty() {
        return Err(Error::EmptyInput);
    }

    match codec {
        CodecId::H264 => demux_annex_b_video(CodecId::H264, bytes),
        CodecId::H265 => demux_annex_b_video(CodecId::H265, bytes),
        CodecId::Aac => demux_adts_aac(bytes),
        CodecId::AV1
        | CodecId::Ac3
        | CodecId::Eac3
        | CodecId::Mp1
        | CodecId::Mp2
        | CodecId::Mp3
        | CodecId::Flac => demux_single_packet(codec, bytes),
        _ => Err(Error::Unsupported {
            operation: "elementary demux",
            reason: "this codec does not have a raw elementary adapter",
        }),
    }
}

/// Mux one packet stream into raw elementary bytes.
pub fn mux_elementary_bytes(media: &MediaInfo, packets: &[EncodedPacket]) -> Result<Bytes> {
    let muxer = ElementaryMuxer::new();
    if media.streams.len() != 1 {
        return Err(Error::Unsupported {
            operation: "elementary mux",
            reason: "raw elementary output can carry exactly one stream",
        });
    }
    let stream = &media.streams[0];
    if !muxer.supports_stream(stream) {
        return Err(Error::Unsupported {
            operation: "elementary mux",
            reason: "stream is not supported by the raw elementary muxer",
        });
    }
    validate_monotonic_by_track(packets)?;

    let mut ordered = packets
        .iter()
        .filter(|packet| packet.track_id == stream.track_id)
        .collect::<Vec<_>>();
    if ordered.is_empty() {
        return Err(Error::EmptyInput);
    }
    ordered.sort_by_key(|packet| packet.decode_order_ts());

    let mut out = BytesMut::new();
    for packet in ordered {
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

        let payload = match stream.codec {
            CodecId::H264 => h264_packet_to_annex_b(packet, stream.codec_config.as_ref())?,
            CodecId::H265 => h265_packet_to_annex_b(packet, stream.codec_config.as_ref())?,
            CodecId::Aac => aac_packet_to_adts(packet, stream.codec_config.as_ref())?,
            _ => packet.data.clone(),
        };
        out.extend_from_slice(&payload);
    }

    Ok(out.freeze())
}

fn demux_annex_b_video(codec: CodecId, bytes: &Bytes) -> Result<DemuxedMedia> {
    let access_units = split_annex_b_access_units(codec.clone(), bytes)?;
    let time_base = TimeBase::new(1, VIDEO_TIME_BASE_DEN)?;
    let duration = DEFAULT_VIDEO_FRAME_DURATION;
    let mut packets = Vec::with_capacity(access_units.len());
    for (index, unit) in access_units.into_iter().enumerate() {
        packets.push(
            EncodedPacket::new(
                1,
                codec.clone(),
                index as i64 * duration,
                duration,
                time_base,
                unit.data,
            )
            .with_keyframe(unit.is_keyframe),
        );
    }

    let mut stream = StreamInfo::new(1, MediaType::Video, codec, time_base);
    stream.duration = Some(packets.len() as i64 * duration);
    let mut media = MediaInfo {
        duration_seconds: Some(time_base.ticks_to_seconds(stream.duration.unwrap_or(0))),
        ..Default::default()
    };
    media.push_stream(stream);

    Ok(DemuxedMedia::new(
        ContainerFormat::RawElementary,
        media,
        packets,
    ))
}

fn demux_adts_aac(bytes: &Bytes) -> Result<DemuxedMedia> {
    let frames = parse_adts_frames(bytes)?;
    if frames.is_empty() {
        return Err(Error::Parse {
            format: "aac",
            message: "no ADTS AAC frames found".to_owned(),
        });
    }

    let sample_rate = frames[0].sample_rate;
    let channels = frames[0].channels;
    let time_base = TimeBase::new(1, sample_rate as i32)?;
    let mut packets = Vec::with_capacity(frames.len());
    let mut pts = 0_i64;
    for frame in frames {
        packets.push(
            EncodedPacket::new(1, CodecId::Aac, pts, 1024, time_base, frame.data)
                .with_keyframe(true),
        );
        pts += 1024;
    }

    let mut stream = StreamInfo::new(1, MediaType::Audio, CodecId::Aac, time_base)
        .with_audio_format(sample_rate, channels);
    stream.duration = Some(pts);
    stream.codec_config = Some(aac_audio_specific_config(
        frames_audio_object_type(bytes)?,
        sample_rate_to_frequency_index(sample_rate)?,
        channels,
    ));

    let mut media = MediaInfo {
        duration_seconds: Some(time_base.ticks_to_seconds(pts)),
        ..Default::default()
    };
    media.push_stream(stream);
    Ok(DemuxedMedia::new(
        ContainerFormat::RawElementary,
        media,
        packets,
    ))
}

fn demux_single_packet(codec: CodecId, bytes: &Bytes) -> Result<DemuxedMedia> {
    let media_type = codec.media_type().ok_or(Error::Unsupported {
        operation: "elementary demux",
        reason: "codec media type is unknown",
    })?;
    let time_base = TimeBase::new(1, VIDEO_TIME_BASE_DEN)?;
    let packet =
        EncodedPacket::new(1, codec.clone(), 0, 0, time_base, bytes.clone()).with_keyframe(true);
    let mut media = MediaInfo::default();
    media.push_stream(StreamInfo::new(1, media_type, codec, time_base));
    Ok(DemuxedMedia::new(
        ContainerFormat::RawElementary,
        media,
        vec![packet],
    ))
}

#[derive(Clone, Debug)]
struct AccessUnit {
    data: Bytes,
    is_keyframe: bool,
}

fn split_annex_b_access_units(codec: CodecId, bytes: &Bytes) -> Result<Vec<AccessUnit>> {
    let mut units = Vec::new();
    let mut current = BytesMut::new();
    let mut current_has_vcl = false;
    let mut current_is_keyframe = false;

    for nal in AnnexBNalIter::new(bytes) {
        if nal.is_empty() {
            continue;
        }
        let is_vcl = match codec {
            CodecId::H264 => h264_is_vcl(nal),
            CodecId::H265 => h265_is_vcl(nal),
            _ => false,
        };
        if is_vcl && current_has_vcl && !current.is_empty() {
            units.push(AccessUnit {
                data: current.split().freeze(),
                is_keyframe: current_is_keyframe,
            });
            current_has_vcl = false;
            current_is_keyframe = false;
        }

        current.extend_from_slice(ANNEX_B_START_CODE);
        current.extend_from_slice(nal);
        current_has_vcl |= is_vcl;
        current_is_keyframe |= match codec {
            CodecId::H264 => h264_is_keyframe_nal(nal),
            CodecId::H265 => h265_is_keyframe_nal(nal),
            _ => false,
        };
    }

    if !current.is_empty() {
        units.push(AccessUnit {
            data: current.freeze(),
            is_keyframe: current_is_keyframe || units.is_empty(),
        });
    }

    if units.is_empty() {
        return Err(Error::Parse {
            format: match codec {
                CodecId::H264 => "h264",
                CodecId::H265 => "h265",
                _ => "elementary",
            },
            message: "no Annex-B NAL units found".to_owned(),
        });
    }

    Ok(units)
}

fn h264_nal_type(nal: &[u8]) -> Option<u8> {
    nal.first().map(|byte| byte & 0x1f)
}

fn h264_is_vcl(nal: &[u8]) -> bool {
    h264_nal_type(nal).is_some_and(|nal_type| matches!(nal_type, 1..=5))
}

fn h264_is_keyframe_nal(nal: &[u8]) -> bool {
    h264_nal_type(nal).is_some_and(|nal_type| nal_type == 5)
}

fn h265_nal_type(nal: &[u8]) -> Option<u8> {
    (nal.len() >= 2).then(|| (nal[0] >> 1) & 0x3f)
}

fn h265_is_vcl(nal: &[u8]) -> bool {
    h265_nal_type(nal).is_some_and(|nal_type| nal_type <= 31)
}

fn h265_is_keyframe_nal(nal: &[u8]) -> bool {
    h265_nal_type(nal).is_some_and(|nal_type| matches!(nal_type, 16..=21))
}

#[derive(Clone, Debug)]
struct AdtsFrame {
    data: Bytes,
    sample_rate: u32,
    channels: u16,
}

fn parse_adts_frames(bytes: &Bytes) -> Result<Vec<AdtsFrame>> {
    let mut offset = 0usize;
    let mut frames = Vec::new();
    while offset + 7 <= bytes.len() {
        if bytes[offset] != 0xff || bytes[offset + 1] & 0xf0 != 0xf0 {
            return Err(Error::Parse {
                format: "aac",
                message: "expected ADTS sync word".to_owned(),
            });
        }
        let protection_absent = bytes[offset + 1] & 0x01 != 0;
        let frequency_index = (bytes[offset + 2] >> 2) & 0x0f;
        let sample_rate = frequency_index_to_sample_rate(frequency_index)?;
        let channel_config = ((bytes[offset + 2] & 0x01) << 2) | ((bytes[offset + 3] >> 6) & 0x03);
        let channels = channel_config_to_channels(channel_config)?;
        let frame_len = (((bytes[offset + 3] & 0x03) as usize) << 11)
            | ((bytes[offset + 4] as usize) << 3)
            | (((bytes[offset + 5] & 0xe0) as usize) >> 5);
        let header_len = if protection_absent { 7 } else { 9 };
        if frame_len < header_len || offset + frame_len > bytes.len() {
            return Err(Error::Parse {
                format: "aac",
                message: "ADTS frame length exceeds input".to_owned(),
            });
        }
        frames.push(AdtsFrame {
            data: Bytes::copy_from_slice(&bytes[offset..offset + frame_len]),
            sample_rate,
            channels,
        });
        offset += frame_len;
    }

    if offset != bytes.len() {
        return Err(Error::Parse {
            format: "aac",
            message: "ADTS input ended with a partial frame".to_owned(),
        });
    }

    Ok(frames)
}

fn frames_audio_object_type(bytes: &Bytes) -> Result<u8> {
    if bytes.len() < 3 {
        return Err(Error::Parse {
            format: "aac",
            message: "ADTS header is too short".to_owned(),
        });
    }
    Ok(((bytes[2] >> 6) & 0x03) + 1)
}

fn frequency_index_to_sample_rate(index: u8) -> Result<u32> {
    match index {
        0 => Ok(96_000),
        1 => Ok(88_200),
        2 => Ok(64_000),
        3 => Ok(48_000),
        4 => Ok(44_100),
        5 => Ok(32_000),
        6 => Ok(24_000),
        7 => Ok(22_050),
        8 => Ok(16_000),
        9 => Ok(12_000),
        10 => Ok(11_025),
        11 => Ok(8_000),
        12 => Ok(7_350),
        _ => Err(Error::Parse {
            format: "aac",
            message: "unsupported ADTS sample-rate index".to_owned(),
        }),
    }
}

fn sample_rate_to_frequency_index(sample_rate: u32) -> Result<u8> {
    match sample_rate {
        96_000 => Ok(0),
        88_200 => Ok(1),
        64_000 => Ok(2),
        48_000 => Ok(3),
        44_100 => Ok(4),
        32_000 => Ok(5),
        24_000 => Ok(6),
        22_050 => Ok(7),
        16_000 => Ok(8),
        12_000 => Ok(9),
        11_025 => Ok(10),
        8_000 => Ok(11),
        7_350 => Ok(12),
        _ => Err(Error::Unsupported {
            operation: "aac config",
            reason: "sample rate has no ADTS frequency-index mapping",
        }),
    }
}

fn channel_config_to_channels(channel_config: u8) -> Result<u16> {
    match channel_config {
        1..=7 => Ok(u16::from(channel_config)),
        _ => Err(Error::Parse {
            format: "aac",
            message: "unsupported ADTS channel configuration".to_owned(),
        }),
    }
}

fn aac_audio_specific_config(audio_object_type: u8, frequency_index: u8, channels: u16) -> Bytes {
    let channel_config = (channels as u8).min(7);
    Bytes::from(vec![
        (audio_object_type << 3) | (frequency_index >> 1),
        ((frequency_index & 0x01) << 7) | (channel_config << 3),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn demuxes_h264_annex_b_access_units() {
        let bytes = Bytes::from_static(
            b"\0\0\0\x01\x67\x42\0\0\0\x01\x68\xce\0\0\0\x01\x65\x88\0\0\0\x01\x41\x99",
        );

        let demuxed = demux_elementary_bytes(CodecId::H264, &bytes).unwrap();

        assert_eq!(demuxed.packets.len(), 2);
        assert!(demuxed.packets[0].is_keyframe);
        assert_eq!(demuxed.packets[1].pts, DEFAULT_VIDEO_FRAME_DURATION);
    }

    #[test]
    fn muxes_single_elementary_track() {
        let time_base = TimeBase::new(1, VIDEO_TIME_BASE_DEN).unwrap();
        let mut media = MediaInfo::default();
        media.push_stream(StreamInfo::new(
            1,
            MediaType::Audio,
            CodecId::Mp3,
            time_base,
        ));
        let packets = vec![
            EncodedPacket::new(1, CodecId::Mp3, 0, 1, time_base, Bytes::from_static(b"a")),
            EncodedPacket::new(1, CodecId::Mp3, 1, 1, time_base, Bytes::from_static(b"b")),
        ];

        let out = mux_elementary_bytes(&media, &packets).unwrap();

        assert_eq!(out, Bytes::from_static(b"ab"));
    }
}
