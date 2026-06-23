use crate::{
    bitstream::{
        aac::aac_packet_to_adts, h264::h264_packet_to_annex_b, h265::h265_packet_to_annex_b,
    },
    codec::{CodecId, MediaType},
    container::{ContainerDemuxer, ContainerFormat, ContainerMuxer, DemuxedMedia},
    error::{Error, Result},
    media::{MediaInfo, StreamInfo},
    packet::{EncodedPacket, validate_monotonic_by_track},
    time::TimeBase,
};
use bytes::{Bytes, BytesMut};
use std::collections::BTreeMap;

const TS_PACKET_SIZE: usize = 188;
const TS_PAYLOAD_SIZE: usize = 184;
const PAT_PID: u16 = 0x0000;
const PMT_PID: u16 = 0x0100;
const FIRST_STREAM_PID: u16 = 0x0101;
const CLOCK_TIME_BASE_DEN: i32 = 90_000;

/// MPEG transport stream demuxer.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub struct MpegTsDemuxer;

impl MpegTsDemuxer {
    /// Create an MPEG-TS demuxer.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl ContainerDemuxer for MpegTsDemuxer {
    fn container_format(&self) -> ContainerFormat {
        ContainerFormat::MpegTs
    }

    fn demux_bytes(&self, bytes: &Bytes) -> Result<DemuxedMedia> {
        demux_mpeg_ts_bytes(bytes)
    }
}

/// MPEG transport stream muxer.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub struct MpegTsMuxer;

impl MpegTsMuxer {
    /// Create an MPEG-TS muxer.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl ContainerMuxer for MpegTsMuxer {
    fn container_format(&self) -> ContainerFormat {
        ContainerFormat::MpegTs
    }

    fn supports_stream(&self, stream: &StreamInfo) -> bool {
        ContainerFormat::MpegTs.supports_stream(stream)
            && stream_type_for_codec(&stream.codec).is_some()
    }

    fn mux_bytes(&self, media: &MediaInfo, packets: &[EncodedPacket]) -> Result<Bytes> {
        mux_mpeg_ts_bytes(media, packets)
    }
}

/// Demux MPEG transport stream bytes into packet-copy PES payloads.
pub fn demux_mpeg_ts_bytes(bytes: &Bytes) -> Result<DemuxedMedia> {
    if bytes.is_empty() {
        return Err(Error::EmptyInput);
    }
    let packet_offsets = ts_packet_offsets(bytes)?;
    let clock = TimeBase::new(1, CLOCK_TIME_BASE_DEN)?;
    let mut pmt_pid = None::<u16>;
    let mut codecs_by_pid = BTreeMap::<u16, CodecId>::new();
    let mut pes_by_pid = BTreeMap::<u16, BytesMut>::new();
    let mut packets = Vec::<EncodedPacket>::new();

    for packet_offset in packet_offsets {
        let chunk = &bytes[packet_offset..packet_offset + TS_PACKET_SIZE];
        let payload_unit_start = chunk[1] & 0x40 != 0;
        let pid = (u16::from(chunk[1] & 0x1f) << 8) | u16::from(chunk[2]);
        let adaptation_control = (chunk[3] >> 4) & 0x03;
        let has_adaptation = matches!(adaptation_control, 2 | 3);
        let has_payload = matches!(adaptation_control, 1 | 3);
        let mut offset = 4usize;
        if has_adaptation {
            let adaptation_len = *chunk.get(offset).ok_or(Error::Parse {
                format: "mpeg-ts",
                message: "missing adaptation-field length".to_owned(),
            })? as usize;
            offset += 1 + adaptation_len;
            if offset > TS_PACKET_SIZE {
                return Err(Error::Parse {
                    format: "mpeg-ts",
                    message: "adaptation field exceeds packet length".to_owned(),
                });
            }
        }
        if !has_payload || offset >= TS_PACKET_SIZE {
            continue;
        }
        let payload = &chunk[offset..];

        if pid == PAT_PID && payload_unit_start {
            if let Some(section) = psi_section(payload)? {
                pmt_pid = Some(parse_pat(&section)?);
            }
            continue;
        }
        if Some(pid) == pmt_pid && payload_unit_start {
            if let Some(section) = psi_section(payload)? {
                codecs_by_pid = parse_pmt(&section)?;
            }
            continue;
        }

        let Some(codec) = codecs_by_pid.get(&pid).cloned() else {
            continue;
        };
        if payload_unit_start
            && let Some(existing) = pes_by_pid.remove(&pid)
            && !existing.is_empty()
        {
            packets.push(parse_pes_packet(
                pid,
                codec.clone(),
                existing.freeze(),
                clock,
            )?);
        }

        pes_by_pid
            .entry(pid)
            .or_default()
            .extend_from_slice(payload);
    }

    for (pid, data) in pes_by_pid {
        if data.is_empty() {
            continue;
        }
        if let Some(codec) = codecs_by_pid.get(&pid).cloned() {
            packets.push(parse_pes_packet(pid, codec, data.freeze(), clock)?);
        }
    }
    if packets.is_empty() {
        return Err(Error::Parse {
            format: "mpeg-ts",
            message: "no PES media packets found".to_owned(),
        });
    }

    infer_packet_durations(&mut packets);
    packets.sort_by(|left, right| {
        left.decode_order_ts()
            .cmp(&right.decode_order_ts())
            .then_with(|| left.track_id.cmp(&right.track_id))
    });

    let mut media = MediaInfo::default();
    for (pid, codec) in codecs_by_pid {
        if !packets
            .iter()
            .any(|packet| packet.track_id == u32::from(pid))
        {
            continue;
        }
        let Some(media_type) = codec.media_type() else {
            continue;
        };
        let mut stream = StreamInfo::new(u32::from(pid), media_type, codec.clone(), clock);
        if codec == CodecId::Aac
            && let Some(packet) = packets
                .iter()
                .find(|packet| packet.track_id == u32::from(pid))
            && let Some((sample_rate, channels)) = parse_adts_audio_format(&packet.data)
        {
            stream.sample_rate = Some(sample_rate);
            stream.channels = Some(channels);
        }
        stream.duration = stream_duration(&packets, u32::from(pid));
        media.push_stream(stream);
    }
    media.duration_seconds = packets
        .iter()
        .map(|packet| clock.ticks_to_seconds(packet.end_pts()))
        .max_by(f64::total_cmp);

    Ok(DemuxedMedia::new(ContainerFormat::MpegTs, media, packets))
}

/// Mux packet-copy streams into MPEG transport stream bytes.
pub fn mux_mpeg_ts_bytes(media: &MediaInfo, packets: &[EncodedPacket]) -> Result<Bytes> {
    let muxer = MpegTsMuxer::new();
    if media.streams.is_empty() || packets.is_empty() {
        return Err(Error::EmptyInput);
    }
    validate_monotonic_by_track(packets)?;
    for stream in &media.streams {
        if !muxer.supports_stream(stream) {
            return Err(Error::Unsupported {
                operation: "mpeg-ts mux",
                reason: "stream is not supported by the MPEG-TS muxer",
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

    let contexts = build_stream_contexts(media)?;
    let pcr_pid = pcr_pid_for_contexts(&contexts)?;
    let clock_time_base = TimeBase::new(1, CLOCK_TIME_BASE_DEN)?;
    let mut out = BytesMut::new();
    let mut continuity = BTreeMap::<u16, u8>::new();
    write_ts_payload(
        &mut out,
        PAT_PID,
        true,
        &pat_payload(PMT_PID),
        &mut continuity,
        None,
    );
    write_ts_payload(
        &mut out,
        PMT_PID,
        true,
        &pmt_payload(&contexts)?,
        &mut continuity,
        None,
    );

    let mut ordered = packets.iter().collect::<Vec<_>>();
    ordered.sort_by(|left, right| {
        let left_ts = left.time_base.ticks_to_seconds(left.decode_order_ts());
        let right_ts = right.time_base.ticks_to_seconds(right.decode_order_ts());
        left_ts
            .total_cmp(&right_ts)
            .then_with(|| left.track_id.cmp(&right.track_id))
            .then_with(|| left.pts.cmp(&right.pts))
    });

    for packet in ordered {
        let context = contexts
            .get(&packet.track_id)
            .ok_or(Error::IncompatibleTrack {
                track_id: packet.track_id,
                reason: "packet references a stream missing from MediaInfo",
            })?;
        if packet.codec != context.codec {
            return Err(Error::CodecMismatch {
                expected: context.codec.clone(),
                actual: packet.codec.clone(),
            });
        }
        let stream = media
            .stream(packet.track_id)
            .ok_or(Error::IncompatibleTrack {
                track_id: packet.track_id,
                reason: "packet references a stream missing from MediaInfo",
            })?;
        let payload = packet_payload_for_ts(packet, stream)?;
        let pts = packet.time_base.rescale(packet.pts, clock_time_base);
        let dts = packet
            .dts
            .map(|dts| packet.time_base.rescale(dts, clock_time_base));
        let pes = pes_packet(context.stream_id, pts, dts, &payload)?;
        let pcr = if context.pid == pcr_pid {
            Some(
                u64::try_from(dts.unwrap_or(pts)).map_err(|_| Error::InvalidPacketTiming {
                    reason: "MPEG-TS PCR timestamp cannot be negative",
                })?,
            )
        } else {
            None
        };
        write_ts_payload(&mut out, context.pid, true, &pes, &mut continuity, pcr);
    }

    Ok(out.freeze())
}

#[derive(Clone, Debug)]
struct TsStreamContext {
    pid: u16,
    stream_id: u8,
    stream_type: u8,
    codec: CodecId,
}

fn build_stream_contexts(media: &MediaInfo) -> Result<BTreeMap<u32, TsStreamContext>> {
    let mut contexts = BTreeMap::new();
    let mut next_pid = FIRST_STREAM_PID;
    let mut video_index = 0u8;
    let mut audio_index = 0u8;
    for stream in &media.streams {
        let stream_type = stream_type_for_codec(&stream.codec).ok_or(Error::Unsupported {
            operation: "mpeg-ts mux",
            reason: "codec has no MPEG-TS stream-type mapping",
        })?;
        let stream_id = match stream.media_type {
            MediaType::Video => {
                let id = 0xe0 | (video_index & 0x0f);
                video_index = video_index.saturating_add(1);
                id
            }
            MediaType::Audio => {
                let id = 0xc0 | (audio_index & 0x1f);
                audio_index = audio_index.saturating_add(1);
                id
            }
            MediaType::Image | MediaType::Subtitle | MediaType::Data => {
                return Err(Error::Unsupported {
                    operation: "mpeg-ts mux",
                    reason: "MPEG-TS muxing supports audio/video elementary streams only",
                });
            }
        };
        contexts.insert(
            stream.track_id,
            TsStreamContext {
                pid: next_pid,
                stream_id,
                stream_type,
                codec: stream.codec.clone(),
            },
        );
        next_pid = next_pid.checked_add(1).ok_or(Error::Unsupported {
            operation: "mpeg-ts mux",
            reason: "too many MPEG-TS streams",
        })?;
    }
    Ok(contexts)
}

fn packet_payload_for_ts(packet: &EncodedPacket, stream: &StreamInfo) -> Result<Bytes> {
    match packet.codec {
        CodecId::H264 => h264_packet_to_annex_b(packet, stream.codec_config.as_ref()),
        CodecId::H265 => h265_packet_to_annex_b(packet, stream.codec_config.as_ref()),
        CodecId::Aac => aac_packet_to_adts(packet, stream.codec_config.as_ref()),
        CodecId::Mpeg2Video | CodecId::Ac3 | CodecId::Eac3 | CodecId::Mp2 | CodecId::Mp3 => {
            Ok(packet.data.clone())
        }
        _ => Err(Error::Unsupported {
            operation: "mpeg-ts mux",
            reason: "codec is not supported by the MPEG-TS muxer",
        }),
    }
}

fn stream_type_for_codec(codec: &CodecId) -> Option<u8> {
    match codec {
        CodecId::Mpeg2Video => Some(0x02),
        CodecId::Mp2 | CodecId::Mp3 => Some(0x04),
        CodecId::Aac => Some(0x0f),
        CodecId::H264 => Some(0x1b),
        CodecId::H265 => Some(0x24),
        CodecId::Ac3 => Some(0x81),
        CodecId::Eac3 => Some(0x87),
        _ => None,
    }
}

fn codec_for_stream_type(stream_type: u8) -> Option<CodecId> {
    match stream_type {
        0x02 => Some(CodecId::Mpeg2Video),
        0x03 => Some(CodecId::Mp2),
        0x04 => Some(CodecId::Mp3),
        0x0f => Some(CodecId::Aac),
        0x1b => Some(CodecId::H264),
        0x24 => Some(CodecId::H265),
        0x81 => Some(CodecId::Ac3),
        0x87 => Some(CodecId::Eac3),
        _ => None,
    }
}

fn pat_payload(pmt_pid: u16) -> Bytes {
    let mut section = BytesMut::new();
    section.extend_from_slice(&[0x00, 0xb0, 0x0d]);
    section.extend_from_slice(&[0x00, 0x01, 0xc1, 0x00, 0x00]);
    section.extend_from_slice(&[
        0x00,
        0x01,
        0xe0 | ((pmt_pid >> 8) as u8 & 0x1f),
        pmt_pid as u8,
    ]);
    append_crc32(&mut section);

    let mut payload = BytesMut::with_capacity(section.len() + 1);
    payload.extend_from_slice(&[0x00]);
    payload.extend_from_slice(&section);
    payload.freeze()
}

fn pmt_payload(contexts: &BTreeMap<u32, TsStreamContext>) -> Result<Bytes> {
    let pcr_pid = pcr_pid_for_contexts(contexts)?;
    let entries_len = contexts.len().checked_mul(5).ok_or(Error::Unsupported {
        operation: "mpeg-ts mux",
        reason: "too many MPEG-TS streams",
    })?;
    let section_len = 9 + entries_len + 4;
    if section_len > 0x03ff {
        return Err(Error::Unsupported {
            operation: "mpeg-ts mux",
            reason: "PMT section is too large",
        });
    }

    let mut section = BytesMut::new();
    section.extend_from_slice(&[
        0x02,
        0xb0 | (((section_len >> 8) as u8) & 0x0f),
        section_len as u8,
        0x00,
        0x01,
        0xc1,
        0x00,
        0x00,
        0xe0 | ((pcr_pid >> 8) as u8 & 0x1f),
        pcr_pid as u8,
        0xf0,
        0x00,
    ]);
    for context in contexts.values() {
        section.extend_from_slice(&[
            context.stream_type,
            0xe0 | ((context.pid >> 8) as u8 & 0x1f),
            context.pid as u8,
            0xf0,
            0x00,
        ]);
    }
    append_crc32(&mut section);

    let mut payload = BytesMut::with_capacity(section.len() + 1);
    payload.extend_from_slice(&[0x00]);
    payload.extend_from_slice(&section);
    Ok(payload.freeze())
}

fn pcr_pid_for_contexts(contexts: &BTreeMap<u32, TsStreamContext>) -> Result<u16> {
    contexts
        .values()
        .find(|context| context.codec.media_type() == Some(MediaType::Video))
        .or_else(|| contexts.values().next())
        .map(|context| context.pid)
        .ok_or(Error::EmptyInput)
}

fn append_crc32(section: &mut BytesMut) {
    let crc = mpeg_crc32(section);
    section.extend_from_slice(&crc.to_be_bytes());
}

fn mpeg_crc32(data: &[u8]) -> u32 {
    let mut crc = 0xffff_ffffu32;
    for byte in data {
        crc ^= u32::from(*byte) << 24;
        for _ in 0..8 {
            if crc & 0x8000_0000 != 0 {
                crc = (crc << 1) ^ 0x04c1_1db7;
            } else {
                crc <<= 1;
            }
        }
    }
    crc
}

fn pes_packet(stream_id: u8, pts: i64, dts: Option<i64>, payload: &[u8]) -> Result<Bytes> {
    if pts < 0 || dts.is_some_and(|dts| dts < 0) {
        return Err(Error::InvalidPacketTiming {
            reason: "MPEG-TS PES timestamps cannot be negative",
        });
    }

    let write_dts = dts.is_some_and(|dts| dts != pts);
    let header_data_len = if write_dts { 10 } else { 5 };
    let pes_packet_length = payload.len() + 3 + header_data_len;
    let length_field = if pes_packet_length <= u16::MAX as usize {
        pes_packet_length as u16
    } else {
        0
    };

    let mut out = BytesMut::with_capacity(payload.len() + 19);
    out.extend_from_slice(&[0x00, 0x00, 0x01, stream_id]);
    out.extend_from_slice(&length_field.to_be_bytes());
    out.extend_from_slice(&[
        0x80,
        if write_dts { 0xc0 } else { 0x80 },
        header_data_len as u8,
    ]);
    if write_dts {
        write_pts(&mut out, 0x03, pts as u64);
        write_pts(&mut out, 0x01, dts.unwrap() as u64);
    } else {
        write_pts(&mut out, 0x02, pts as u64);
    }
    out.extend_from_slice(payload);
    Ok(out.freeze())
}

fn write_pts(out: &mut BytesMut, prefix: u8, pts: u64) {
    let value = pts & ((1_u64 << 33) - 1);
    out.extend_from_slice(&[
        (prefix << 4) | (((value >> 30) as u8 & 0x07) << 1) | 0x01,
        (value >> 22) as u8,
        (((value >> 15) as u8 & 0x7f) << 1) | 0x01,
        (value >> 7) as u8,
        ((value as u8 & 0x7f) << 1) | 0x01,
    ]);
}

fn write_ts_payload(
    out: &mut BytesMut,
    pid: u16,
    payload_unit_start: bool,
    payload: &[u8],
    continuity: &mut BTreeMap<u16, u8>,
    first_packet_pcr: Option<u64>,
) {
    let mut offset = 0usize;
    let mut first = true;
    while offset < payload.len() {
        let include_pcr = first && first_packet_pcr.is_some();
        let pcr_overhead = if include_pcr { 8 } else { 0 };
        let max_payload_len = TS_PAYLOAD_SIZE - pcr_overhead;
        let remaining = payload.len() - offset;
        let payload_len = remaining.min(max_payload_len);
        let stuffing = max_payload_len - payload_len;
        let counter = continuity.entry(pid).or_insert(0);
        let continuity_counter = *counter;
        *counter = (*counter + 1) & 0x0f;

        out.extend_from_slice(&[
            0x47,
            ((if payload_unit_start && first {
                0x40
            } else {
                0x00
            }) | ((pid >> 8) as u8 & 0x1f)),
            pid as u8,
            if stuffing == 0 && !include_pcr {
                0x10 | continuity_counter
            } else {
                0x30 | continuity_counter
            },
        ]);
        if include_pcr {
            let adaptation_len = 7 + stuffing;
            out.extend_from_slice(&[adaptation_len as u8, 0x10]);
            write_pcr(out, first_packet_pcr.unwrap());
            out.extend(std::iter::repeat_n(0xff, stuffing));
        } else if stuffing != 0 {
            let adaptation_len = stuffing - 1;
            out.extend_from_slice(&[adaptation_len as u8]);
            if adaptation_len > 0 {
                out.extend_from_slice(&[0x00]);
                out.extend(std::iter::repeat_n(0xff, adaptation_len - 1));
            }
        }
        out.extend_from_slice(&payload[offset..offset + payload_len]);
        offset += payload_len;
        first = false;
    }
}

fn write_pcr(out: &mut BytesMut, pcr_base: u64) {
    let value = pcr_base & ((1_u64 << 33) - 1);
    out.extend_from_slice(&[
        (value >> 25) as u8,
        (value >> 17) as u8,
        (value >> 9) as u8,
        (value >> 1) as u8,
        (((value & 0x01) as u8) << 7) | 0x7e,
        0x00,
    ]);
}

fn ts_packet_offsets(bytes: &[u8]) -> Result<Vec<usize>> {
    let mut offsets = Vec::new();
    let mut cursor = 0usize;
    while let Some(offset) = find_next_ts_packet(bytes, cursor) {
        offsets.push(offset);
        cursor = offset + TS_PACKET_SIZE;
    }
    if offsets.is_empty() {
        return Err(Error::Parse {
            format: "mpeg-ts",
            message: "no complete transport stream packets found".to_owned(),
        });
    }
    Ok(offsets)
}

fn find_next_ts_packet(bytes: &[u8], from: usize) -> Option<usize> {
    let mut offset = from;
    while offset + TS_PACKET_SIZE <= bytes.len() {
        if bytes[offset] == 0x47 && plausible_sync_at(bytes, offset) {
            return Some(offset);
        }
        offset += 1;
    }
    None
}

fn plausible_sync_at(bytes: &[u8], offset: usize) -> bool {
    if offset + TS_PACKET_SIZE > bytes.len() || bytes[offset] != 0x47 {
        return false;
    }
    let next = offset + TS_PACKET_SIZE;
    next + TS_PACKET_SIZE > bytes.len() || bytes[next] == 0x47
}

fn psi_section(payload: &[u8]) -> Result<Option<Bytes>> {
    if payload.is_empty() {
        return Ok(None);
    }
    let pointer = payload[0] as usize;
    let start = 1 + pointer;
    if start >= payload.len() {
        return Ok(None);
    }
    if start + 3 > payload.len() {
        return Err(Error::Parse {
            format: "mpeg-ts",
            message: "PSI section is too short".to_owned(),
        });
    }
    let section_len =
        ((usize::from(payload[start + 1] & 0x0f)) << 8) | usize::from(payload[start + 2]);
    let end = start + 3 + section_len;
    if end > payload.len() {
        return Err(Error::Parse {
            format: "mpeg-ts",
            message: "PSI section length exceeds packet payload".to_owned(),
        });
    }
    Ok(Some(Bytes::copy_from_slice(&payload[start..end])))
}

fn parse_pat(section: &[u8]) -> Result<u16> {
    if section.len() < 16 || section[0] != 0x00 {
        return Err(Error::Parse {
            format: "mpeg-ts",
            message: "invalid PAT section".to_owned(),
        });
    }
    Ok((u16::from(section[10] & 0x1f) << 8) | u16::from(section[11]))
}

fn parse_pmt(section: &[u8]) -> Result<BTreeMap<u16, CodecId>> {
    if section.len() < 16 || section[0] != 0x02 {
        return Err(Error::Parse {
            format: "mpeg-ts",
            message: "invalid PMT section".to_owned(),
        });
    }
    let section_len = ((usize::from(section[1] & 0x0f)) << 8) | usize::from(section[2]);
    let section_end = 3 + section_len;
    if section_end > section.len() || section_end < 4 {
        return Err(Error::Parse {
            format: "mpeg-ts",
            message: "invalid PMT section length".to_owned(),
        });
    }
    let entries_end = section_end - 4;
    let program_info_len = ((usize::from(section[10] & 0x0f)) << 8) | usize::from(section[11]);
    let mut offset = 12 + program_info_len;
    let mut out = BTreeMap::new();
    while offset + 5 <= entries_end {
        let stream_type = section[offset];
        let pid = (u16::from(section[offset + 1] & 0x1f) << 8) | u16::from(section[offset + 2]);
        let es_info_len =
            ((usize::from(section[offset + 3] & 0x0f)) << 8) | usize::from(section[offset + 4]);
        if let Some(codec) = codec_for_stream_type(stream_type) {
            out.insert(pid, codec);
        }
        offset += 5 + es_info_len;
    }
    Ok(out)
}

fn parse_pes_packet(
    pid: u16,
    codec: CodecId,
    data: Bytes,
    time_base: TimeBase,
) -> Result<EncodedPacket> {
    if data.len() < 9 || &data[0..3] != b"\0\0\x01" {
        return Err(Error::Parse {
            format: "mpeg-ts",
            message: "invalid PES packet header".to_owned(),
        });
    }
    let flags = data[7];
    let header_data_len = data[8] as usize;
    let payload_offset = 9 + header_data_len;
    if payload_offset > data.len() {
        return Err(Error::Parse {
            format: "mpeg-ts",
            message: "PES optional header exceeds packet length".to_owned(),
        });
    }
    let pts_dts_flags = (flags >> 6) & 0x03;
    let pts = if pts_dts_flags & 0x02 != 0 {
        parse_pts(&data[9..14])? as i64
    } else {
        0
    };
    let dts = if pts_dts_flags == 0x03 {
        Some(parse_pts(&data[14..19])? as i64)
    } else {
        None
    };

    Ok(EncodedPacket::new(
        u32::from(pid),
        codec,
        pts,
        0,
        time_base,
        Bytes::copy_from_slice(&data[payload_offset..]),
    )
    .with_dts(dts.unwrap_or(pts))
    .with_keyframe(true))
}

fn parse_pts(bytes: &[u8]) -> Result<u64> {
    if bytes.len() < 5 {
        return Err(Error::Parse {
            format: "mpeg-ts",
            message: "PES timestamp is too short".to_owned(),
        });
    }
    Ok((u64::from((bytes[0] >> 1) & 0x07) << 30)
        | (u64::from(bytes[1]) << 22)
        | (u64::from((bytes[2] >> 1) & 0x7f) << 15)
        | (u64::from(bytes[3]) << 7)
        | u64::from((bytes[4] >> 1) & 0x7f))
}

fn infer_packet_durations(packets: &mut [EncodedPacket]) {
    let mut by_track = BTreeMap::<u32, Vec<usize>>::new();
    for (index, packet) in packets.iter().enumerate() {
        by_track.entry(packet.track_id).or_default().push(index);
    }
    for indices in by_track.values() {
        for window in indices.windows(2) {
            let current = window[0];
            let next = window[1];
            let duration = packets[next].pts - packets[current].pts;
            if duration > 0 {
                packets[current].duration = duration;
            }
        }
        if let Some(last) = indices.last().copied() {
            let fallback = indices
                .iter()
                .rev()
                .skip(1)
                .find_map(|index| {
                    (packets[*index].duration > 0).then_some(packets[*index].duration)
                })
                .unwrap_or(0);
            packets[last].duration = fallback;
        }
    }
}

fn stream_duration(packets: &[EncodedPacket], track_id: u32) -> Option<i64> {
    packets
        .iter()
        .filter(|packet| packet.track_id == track_id)
        .map(EncodedPacket::end_pts)
        .max()
}

fn parse_adts_audio_format(data: &[u8]) -> Option<(u32, u16)> {
    if data.len() < 7 || data[0] != 0xff || data[1] & 0xf0 != 0xf0 {
        return None;
    }
    let frequency_index = (data[2] >> 2) & 0x0f;
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
    let channel_config = ((data[2] & 0x01) << 2) | ((data[3] >> 6) & 0x03);
    if !(1..=7).contains(&channel_config) {
        return None;
    }
    Some((sample_rate, u16::from(channel_config)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mux_demuxes_h264_transport_stream() {
        let time_base = TimeBase::new(1, 90_000).unwrap();
        let mut media = MediaInfo::default();
        media.push_stream(
            StreamInfo::new(1, MediaType::Video, CodecId::H264, time_base).with_dimensions(16, 16),
        );
        let packets = vec![
            EncodedPacket::new(
                1,
                CodecId::H264,
                0,
                3_000,
                time_base,
                Bytes::from_static(b"\0\0\0\x01\x65\x88"),
            )
            .with_keyframe(true),
            EncodedPacket::new(
                1,
                CodecId::H264,
                3_000,
                3_000,
                time_base,
                Bytes::from_static(b"\0\0\0\x01\x41\x99"),
            ),
        ];

        let bytes = mux_mpeg_ts_bytes(&media, &packets).unwrap();
        let demuxed = demux_mpeg_ts_bytes(&bytes).unwrap();

        assert_eq!(demuxed.format, ContainerFormat::MpegTs);
        assert_eq!(demuxed.media.streams[0].codec, CodecId::H264);
        assert_eq!(demuxed.packets.len(), 2);
        assert!(demuxed.packets[0].data.starts_with(b"\0\0\0\x01"));
    }
}
