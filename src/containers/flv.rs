use crate::{
    bitstream::{aac::aac_packet_to_raw, h264::h264_packet_to_length_prefixed},
    codec::{CodecId, MediaType},
    container::{ContainerDemuxer, ContainerFormat, ContainerMuxer, DemuxedMedia},
    error::{Error, Result},
    media::{MediaInfo, StreamInfo},
    packet::{EncodedPacket, validate_monotonic_by_track},
    time::TimeBase,
};
use bytes::{BufMut, Bytes, BytesMut};
use std::collections::BTreeMap;

const FLV_HEADER_LEN: usize = 9;
const TAG_AUDIO: u8 = 8;
const TAG_VIDEO: u8 = 9;
const FLV_CODEC_H264: u8 = 7;
const FLV_AUDIO_AAC: u8 = 10;
const AVC_SEQUENCE_HEADER: u8 = 0;
const AVC_NALU: u8 = 1;
const AAC_SEQUENCE_HEADER: u8 = 0;
const AAC_RAW: u8 = 1;

/// FLV demuxer for H.264/AAC packet-copy workflows.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub struct FlvDemuxer;

impl FlvDemuxer {
    /// Create an FLV demuxer.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl ContainerDemuxer for FlvDemuxer {
    fn container_format(&self) -> ContainerFormat {
        ContainerFormat::Flv
    }

    fn demux_bytes(&self, bytes: &Bytes) -> Result<DemuxedMedia> {
        demux_flv_bytes(bytes)
    }
}

/// FLV muxer for H.264 video and AAC audio packet-copy workflows.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub struct FlvMuxer;

impl FlvMuxer {
    /// Create an FLV muxer.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl ContainerMuxer for FlvMuxer {
    fn container_format(&self) -> ContainerFormat {
        ContainerFormat::Flv
    }

    fn supports_stream(&self, stream: &StreamInfo) -> bool {
        matches!(
            (&stream.media_type, &stream.codec),
            (MediaType::Video, CodecId::H264) | (MediaType::Audio, CodecId::Aac)
        )
    }

    fn mux_bytes(&self, media: &MediaInfo, packets: &[EncodedPacket]) -> Result<Bytes> {
        mux_flv_bytes(media, packets)
    }
}

/// Probe stream metadata from FLV bytes.
pub fn probe_flv_bytes(bytes: &Bytes) -> Result<MediaInfo> {
    demux_flv_bytes(bytes).map(|demuxed| demuxed.media)
}

/// Demux FLV H.264/AAC bytes into packet-copy streams.
pub fn demux_flv_bytes(bytes: &Bytes) -> Result<DemuxedMedia> {
    if bytes.len() < FLV_HEADER_LEN + 4 || &bytes[0..3] != b"FLV" {
        return Err(Error::Parse {
            format: "flv",
            message: "input is not an FLV object".to_owned(),
        });
    }
    let data_offset = read_u32_be(bytes, 5)? as usize;
    if data_offset < FLV_HEADER_LEN || data_offset + 4 > bytes.len() {
        return Err(Error::Parse {
            format: "flv",
            message: "FLV data offset is invalid".to_owned(),
        });
    }

    let mut streams = BTreeMap::<u32, StreamInfo>::new();
    let mut packets = Vec::new();
    let mut video_config = None::<Bytes>;
    let mut audio_config = None::<Bytes>;
    let mut offset = data_offset + 4;
    let time_base = TimeBase::milliseconds();

    while offset + 11 <= bytes.len() {
        let tag_type = bytes[offset];
        let data_size = read_u24(bytes, offset + 1)?;
        let timestamp = read_flv_timestamp(bytes, offset + 4)?;
        offset += 11;
        let data_size_usize = data_size as usize;
        if offset + data_size_usize > bytes.len() {
            return Err(Error::Parse {
                format: "flv",
                message: "FLV tag data exceeds object length".to_owned(),
            });
        }
        let payload = &bytes[offset..offset + data_size_usize];
        match tag_type {
            TAG_VIDEO => parse_video_tag(
                payload,
                timestamp,
                time_base,
                &mut streams,
                &mut packets,
                &mut video_config,
            )?,
            TAG_AUDIO => parse_audio_tag(
                payload,
                timestamp,
                time_base,
                &mut streams,
                &mut packets,
                &mut audio_config,
            )?,
            _ => {}
        }
        offset += data_size_usize;
        if offset + 4 > bytes.len() {
            break;
        }
        offset += 4;
    }

    let mut media = MediaInfo::default();
    for (_, stream) in streams {
        media.push_stream(stream);
    }
    update_packet_durations(&mut packets);
    media.duration_seconds = packets
        .iter()
        .map(|packet| {
            packet
                .time_base
                .ticks_to_seconds(packet.pts + packet.duration)
        })
        .max_by(f64::total_cmp);

    Ok(DemuxedMedia::new(ContainerFormat::Flv, media, packets))
}

/// Mux H.264/AAC packets into FLV bytes.
pub fn mux_flv_bytes(media: &MediaInfo, packets: &[EncodedPacket]) -> Result<Bytes> {
    if media.streams.is_empty() || packets.is_empty() {
        return Err(Error::EmptyInput);
    }
    let muxer = FlvMuxer::new();
    for stream in &media.streams {
        if !muxer.supports_stream(stream) {
            return Err(Error::Unsupported {
                operation: "flv mux",
                reason: "FLV muxing currently supports H.264 video and AAC audio",
            });
        }
    }
    validate_monotonic_by_track(packets)?;

    let has_audio = media
        .streams
        .iter()
        .any(|stream| stream.media_type == MediaType::Audio);
    let has_video = media
        .streams
        .iter()
        .any(|stream| stream.media_type == MediaType::Video);
    let mut out = BytesMut::new();
    out.extend_from_slice(b"FLV");
    out.put_u8(1);
    out.put_u8((if has_audio { 0x04 } else { 0 }) | (if has_video { 0x01 } else { 0 }));
    out.put_u32(FLV_HEADER_LEN as u32);
    out.put_u32(0);

    for stream in &media.streams {
        match (&stream.media_type, &stream.codec) {
            (MediaType::Video, CodecId::H264) => {
                let config = stream.codec_config.as_ref().ok_or(Error::Unsupported {
                    operation: "flv mux",
                    reason: "H.264 FLV muxing requires AVCDecoderConfigurationRecord",
                })?;
                write_flv_tag(&mut out, TAG_VIDEO, 0, &video_sequence_header(config)?)?;
            }
            (MediaType::Audio, CodecId::Aac) => {
                let config = stream.codec_config.as_ref().ok_or(Error::Unsupported {
                    operation: "flv mux",
                    reason: "AAC FLV muxing requires AudioSpecificConfig",
                })?;
                write_flv_tag(&mut out, TAG_AUDIO, 0, &audio_sequence_header(config))?;
            }
            _ => {}
        }
    }

    let mut ordered = packets.iter().collect::<Vec<_>>();
    ordered.sort_by(|left, right| {
        left.pts_seconds()
            .total_cmp(&right.pts_seconds())
            .then_with(|| left.track_id.cmp(&right.track_id))
    });
    for packet in ordered {
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
        let timestamp = flv_timestamp_ms(packet)?;
        match packet.codec {
            CodecId::H264 => {
                let config = stream.codec_config.as_ref().ok_or(Error::Unsupported {
                    operation: "flv mux",
                    reason: "H.264 FLV muxing requires AVCDecoderConfigurationRecord",
                })?;
                write_flv_tag(
                    &mut out,
                    TAG_VIDEO,
                    timestamp,
                    &video_nalu_packet(stream, packet, config)?,
                )?;
            }
            CodecId::Aac => {
                write_flv_tag(
                    &mut out,
                    TAG_AUDIO,
                    timestamp,
                    &audio_raw_packet(stream, packet)?,
                )?;
            }
            _ => {}
        }
    }

    Ok(out.freeze())
}

fn parse_video_tag(
    payload: &[u8],
    timestamp: u32,
    time_base: TimeBase,
    streams: &mut BTreeMap<u32, StreamInfo>,
    packets: &mut Vec<EncodedPacket>,
    video_config: &mut Option<Bytes>,
) -> Result<()> {
    if payload.len() < 5 {
        return Ok(());
    }
    let frame_type = payload[0] >> 4;
    let codec = payload[0] & 0x0f;
    if codec != FLV_CODEC_H264 {
        return Err(Error::Unsupported {
            operation: "flv demux",
            reason: "FLV video demuxing currently supports H.264 only",
        });
    }
    let packet_type = payload[1];
    let composition_time = read_i24(payload, 2)?;
    match packet_type {
        AVC_SEQUENCE_HEADER => {
            let config = Bytes::copy_from_slice(&payload[5..]);
            *video_config = Some(config.clone());
            let mut stream = StreamInfo::new(1, MediaType::Video, CodecId::H264, time_base);
            stream.codec_config = Some(config);
            streams.insert(1, stream);
        }
        AVC_NALU => {
            let pts = i64::from(timestamp) + i64::from(composition_time);
            let mut packet = EncodedPacket::new(
                1,
                CodecId::H264,
                pts,
                0,
                time_base,
                Bytes::copy_from_slice(&payload[5..]),
            )
            .with_dts(i64::from(timestamp))
            .with_keyframe(frame_type == 1);
            if let Some(config) = video_config.clone() {
                streams
                    .entry(1)
                    .or_insert_with(|| {
                        StreamInfo::new(1, MediaType::Video, CodecId::H264, time_base)
                    })
                    .codec_config = Some(config);
            }
            if packet.pts < 0 {
                packet.pts = i64::from(timestamp);
            }
            packets.push(packet);
        }
        _ => {}
    }
    Ok(())
}

fn parse_audio_tag(
    payload: &[u8],
    timestamp: u32,
    time_base: TimeBase,
    streams: &mut BTreeMap<u32, StreamInfo>,
    packets: &mut Vec<EncodedPacket>,
    audio_config: &mut Option<Bytes>,
) -> Result<()> {
    if payload.len() < 2 {
        return Ok(());
    }
    let sound_format = payload[0] >> 4;
    if sound_format != FLV_AUDIO_AAC {
        return Err(Error::Unsupported {
            operation: "flv demux",
            reason: "FLV audio demuxing currently supports AAC only",
        });
    }
    match payload[1] {
        AAC_SEQUENCE_HEADER => {
            let config = Bytes::copy_from_slice(&payload[2..]);
            let (sample_rate, channels) = parse_aac_config(&config).unwrap_or((44_100, 2));
            let mut stream = StreamInfo::new(2, MediaType::Audio, CodecId::Aac, time_base)
                .with_audio_format(sample_rate, channels);
            stream.codec_config = Some(config.clone());
            *audio_config = Some(config);
            streams.insert(2, stream);
        }
        AAC_RAW => {
            if let Some(config) = audio_config.clone() {
                let (sample_rate, channels) = parse_aac_config(&config).unwrap_or((44_100, 2));
                streams.entry(2).or_insert_with(|| {
                    let mut stream = StreamInfo::new(2, MediaType::Audio, CodecId::Aac, time_base)
                        .with_audio_format(sample_rate, channels);
                    stream.codec_config = Some(config);
                    stream
                });
            }
            packets.push(
                EncodedPacket::new(
                    2,
                    CodecId::Aac,
                    i64::from(timestamp),
                    0,
                    time_base,
                    Bytes::copy_from_slice(&payload[2..]),
                )
                .with_keyframe(true),
            );
        }
        _ => {}
    }
    Ok(())
}

fn update_packet_durations(packets: &mut [EncodedPacket]) {
    let mut by_track = BTreeMap::<u32, Vec<usize>>::new();
    for (index, packet) in packets.iter().enumerate() {
        by_track.entry(packet.track_id).or_default().push(index);
    }
    for indexes in by_track.values_mut() {
        indexes.sort_by_key(|index| packets[*index].decode_order_ts());
        for pair in indexes.windows(2) {
            let current = pair[0];
            let next = pair[1];
            let duration = packets[next].decode_order_ts() - packets[current].decode_order_ts();
            if duration > 0 {
                packets[current].duration = duration;
            }
        }
    }
}

fn video_sequence_header(config: &[u8]) -> Result<Bytes> {
    let mut out = BytesMut::new();
    out.put_u8(0x10 | FLV_CODEC_H264);
    out.put_u8(AVC_SEQUENCE_HEADER);
    write_i24(&mut out, 0)?;
    out.extend_from_slice(config);
    Ok(out.freeze())
}

fn audio_sequence_header(config: &[u8]) -> Bytes {
    let mut out = BytesMut::new();
    out.put_u8(0xaf);
    out.put_u8(AAC_SEQUENCE_HEADER);
    out.extend_from_slice(config);
    out.freeze()
}

fn video_nalu_packet(stream: &StreamInfo, packet: &EncodedPacket, config: &Bytes) -> Result<Bytes> {
    let mut out = BytesMut::new();
    out.put_u8((if packet.is_keyframe { 0x10 } else { 0x20 }) | FLV_CODEC_H264);
    out.put_u8(AVC_NALU);
    let dts = packet.dts.unwrap_or(packet.pts);
    let composition_ms = packet
        .time_base
        .rescale(packet.pts - dts, TimeBase::milliseconds());
    write_i24(&mut out, composition_ms)?;
    out.extend_from_slice(&h264_packet_to_length_prefixed(packet, config)?);
    let _ = stream;
    Ok(out.freeze())
}

fn audio_raw_packet(stream: &StreamInfo, packet: &EncodedPacket) -> Result<Bytes> {
    let mut out = BytesMut::new();
    out.put_u8(audio_flags(stream));
    out.put_u8(AAC_RAW);
    out.extend_from_slice(&aac_packet_to_raw(packet)?);
    Ok(out.freeze())
}

fn audio_flags(stream: &StreamInfo) -> u8 {
    let rate = match stream.sample_rate.unwrap_or(44_100) {
        0..=5_512 => 0,
        5_513..=11_025 => 1,
        11_026..=22_050 => 2,
        _ => 3,
    };
    let channels = u8::from(stream.channels.unwrap_or(2) > 1);
    (FLV_AUDIO_AAC << 4) | (rate << 2) | (1 << 1) | channels
}

fn flv_timestamp_ms(packet: &EncodedPacket) -> Result<u32> {
    let ts = packet
        .time_base
        .rescale(packet.decode_order_ts(), TimeBase::milliseconds());
    u32::try_from(ts).map_err(|_| Error::InvalidPacketTiming {
        reason: "FLV timestamp must fit unsigned 32-bit milliseconds",
    })
}

fn write_flv_tag(out: &mut BytesMut, tag_type: u8, timestamp: u32, payload: &[u8]) -> Result<()> {
    let size = u32::try_from(payload.len()).map_err(|_| Error::Unsupported {
        operation: "flv mux",
        reason: "FLV tag payload exceeds 24-bit size field",
    })?;
    if size > 0x00ff_ffff {
        return Err(Error::Unsupported {
            operation: "flv mux",
            reason: "FLV tag payload exceeds 24-bit size field",
        });
    }
    out.put_u8(tag_type);
    put_u24(out, size);
    put_u24(out, timestamp & 0x00ff_ffff);
    out.put_u8(((timestamp >> 24) & 0xff) as u8);
    put_u24(out, 0);
    out.extend_from_slice(payload);
    out.put_u32(11 + size);
    Ok(())
}

fn parse_aac_config(config: &[u8]) -> Option<(u32, u16)> {
    if config.len() < 2 {
        return None;
    }
    let object_type = config[0] >> 3;
    if object_type == 0 {
        return None;
    }
    let frequency_index = ((config[0] & 0x07) << 1) | (config[1] >> 7);
    let sample_rate = match frequency_index {
        0 => 96_000,
        1 => 88_200,
        2 => 64_000,
        3 => 48_000,
        4 => 44_100,
        5 => 32_000,
        6 => 24_000,
        7 => 22_050,
        8 => 16_000,
        9 => 12_000,
        10 => 11_025,
        11 => 8_000,
        12 => 7_350,
        _ => return None,
    };
    let channels = ((config[1] >> 3) & 0x0f) as u16;
    Some((sample_rate, channels.max(1)))
}

fn read_flv_timestamp(bytes: &Bytes, offset: usize) -> Result<u32> {
    let low = read_u24(bytes, offset)?;
    let ext = *bytes.get(offset + 3).ok_or(Error::Parse {
        format: "flv",
        message: "FLV timestamp is truncated".to_owned(),
    })?;
    Ok(low | (u32::from(ext) << 24))
}

fn read_u24(bytes: &Bytes, offset: usize) -> Result<u32> {
    if bytes.len().saturating_sub(offset) < 3 {
        return Err(Error::Parse {
            format: "flv",
            message: "FLV 24-bit field is truncated".to_owned(),
        });
    }
    Ok((u32::from(bytes[offset]) << 16)
        | (u32::from(bytes[offset + 1]) << 8)
        | u32::from(bytes[offset + 2]))
}

fn read_i24(bytes: &[u8], offset: usize) -> Result<i32> {
    if bytes.len().saturating_sub(offset) < 3 {
        return Err(Error::Parse {
            format: "flv",
            message: "FLV signed 24-bit field is truncated".to_owned(),
        });
    }
    let raw = ((i32::from(bytes[offset])) << 16)
        | ((i32::from(bytes[offset + 1])) << 8)
        | i32::from(bytes[offset + 2]);
    Ok(if raw & 0x0080_0000 != 0 {
        raw | !0x00ff_ffff
    } else {
        raw
    })
}

fn read_u32_be(bytes: &Bytes, offset: usize) -> Result<u32> {
    if bytes.len().saturating_sub(offset) < 4 {
        return Err(Error::Parse {
            format: "flv",
            message: "FLV 32-bit field is truncated".to_owned(),
        });
    }
    Ok(u32::from_be_bytes(
        bytes[offset..offset + 4].try_into().unwrap(),
    ))
}

fn put_u24(out: &mut BytesMut, value: u32) {
    out.put_u8(((value >> 16) & 0xff) as u8);
    out.put_u8(((value >> 8) & 0xff) as u8);
    out.put_u8((value & 0xff) as u8);
}

fn write_i24(out: &mut BytesMut, value: i64) -> Result<()> {
    if !(-0x80_0000..=0x7f_ffff).contains(&value) {
        return Err(Error::InvalidPacketTiming {
            reason: "FLV composition time must fit signed 24-bit milliseconds",
        });
    }
    let value = (value as i32) & 0x00ff_ffff;
    put_u24(out, value as u32);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn muxes_and_demuxes_h264_aac_flv() {
        let video_time_base = TimeBase::milliseconds();
        let audio_time_base = TimeBase::milliseconds();
        let mut media = MediaInfo::default();
        let mut video = StreamInfo::new(1, MediaType::Video, CodecId::H264, video_time_base);
        video.codec_config = Some(Bytes::from_static(
            b"\x01\x42\x00\x1e\xff\xe1\0\x02\x67\x42\x01\0\x02\x68\xce",
        ));
        media.push_stream(video);
        let mut audio = StreamInfo::new(2, MediaType::Audio, CodecId::Aac, audio_time_base)
            .with_audio_format(48_000, 2);
        audio.codec_config = Some(Bytes::from_static(&[0x11, 0x90]));
        media.push_stream(audio);
        let packets = vec![
            EncodedPacket::new(
                1,
                CodecId::H264,
                0,
                40,
                video_time_base,
                Bytes::from_static(b"\0\0\0\x01\x65\x88"),
            )
            .with_keyframe(true),
            EncodedPacket::new(
                2,
                CodecId::Aac,
                0,
                23,
                audio_time_base,
                Bytes::from_static(b"\x11\x22"),
            )
            .with_keyframe(true),
        ];

        let bytes = mux_flv_bytes(&media, &packets).unwrap();
        let demuxed = demux_flv_bytes(&bytes).unwrap();

        assert_eq!(&bytes[0..3], b"FLV");
        assert_eq!(demuxed.media.streams.len(), 2);
        assert_eq!(demuxed.packets.len(), 2);
        assert_eq!(demuxed.packets[0].codec, CodecId::H264);
        assert_eq!(&demuxed.packets[0].data[..], b"\0\0\0\x02\x65\x88");
        assert_eq!(demuxed.packets[1].codec, CodecId::Aac);
        assert_eq!(&demuxed.packets[1].data[..], b"\x11\x22");
    }
}
