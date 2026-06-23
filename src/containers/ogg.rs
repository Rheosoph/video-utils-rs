use crate::{
    codec::{CodecId, MediaType},
    container::{ContainerDemuxer, ContainerFormat, ContainerMuxer, DemuxedMedia},
    error::{Error, Result},
    media::{MediaInfo, StreamInfo},
    packet::{EncodedPacket, validate_monotonic_by_track},
    time::TimeBase,
};
use bytes::{BufMut, Bytes, BytesMut};
use std::collections::BTreeMap;

const OGG_CAPTURE: &[u8; 4] = b"OggS";
const OPUS_SAMPLE_RATE: u32 = 48_000;
const DEFAULT_OPUS_PACKET_DURATION: i64 = 960;
const DEFAULT_VORBIS_PACKET_DURATION: i64 = 1024;

/// Ogg demuxer for packet-copy audio streams.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub struct OggDemuxer;

impl OggDemuxer {
    /// Create an Ogg demuxer.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl ContainerDemuxer for OggDemuxer {
    fn container_format(&self) -> ContainerFormat {
        ContainerFormat::Ogg
    }

    fn demux_bytes(&self, bytes: &Bytes) -> Result<DemuxedMedia> {
        demux_ogg_bytes(bytes)
    }
}

/// Ogg muxer for packet-copy Opus, Vorbis, and FLAC streams.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub struct OggMuxer;

impl OggMuxer {
    /// Create an Ogg muxer.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl ContainerMuxer for OggMuxer {
    fn container_format(&self) -> ContainerFormat {
        ContainerFormat::Ogg
    }

    fn supports_stream(&self, stream: &StreamInfo) -> bool {
        stream.media_type == MediaType::Audio
            && matches!(
                stream.codec,
                CodecId::Opus | CodecId::Vorbis | CodecId::Flac
            )
    }

    fn mux_bytes(&self, media: &MediaInfo, packets: &[EncodedPacket]) -> Result<Bytes> {
        mux_ogg_bytes(media, packets)
    }
}

/// Probe stream metadata from Ogg bytes.
pub fn probe_ogg_bytes(bytes: &Bytes) -> Result<MediaInfo> {
    demux_ogg_bytes(bytes).map(|demuxed| demuxed.media)
}

/// Demux Ogg bytes into packet-copy audio streams.
pub fn demux_ogg_bytes(bytes: &Bytes) -> Result<DemuxedMedia> {
    let pages = parse_pages(bytes)?;
    let records = collect_packet_records(&pages);
    if records.is_empty() {
        return Err(Error::EmptyInput);
    }

    let mut groups = BTreeMap::<u32, Vec<OggPacketRecord>>::new();
    for record in records {
        groups.entry(record.serial).or_default().push(record);
    }

    let mut media = MediaInfo::default();
    let mut packets = Vec::new();
    let mut next_track_id = 1_u32;

    for (serial, records) in groups {
        let Some(first) = records.first() else {
            continue;
        };
        let parsed = parse_stream_headers(&records, first)?;
        let track_id = next_track_id;
        next_track_id += 1;
        let mut stream = StreamInfo::new(
            track_id,
            MediaType::Audio,
            parsed.codec.clone(),
            TimeBase::new(1, parsed.sample_rate as i32)?,
        )
        .with_audio_format(parsed.sample_rate, parsed.channels);
        stream.codec_config = parsed.codec_config.clone();
        stream
            .tags
            .insert("ogg_serial".to_owned(), serial.to_string());
        stream
            .tags
            .insert("ogg_codec".to_owned(), parsed.codec.as_str().to_owned());

        let mut pts = 0_i64;
        let default_duration = default_packet_duration(&parsed.codec);
        for record in records.iter().skip(parsed.header_count) {
            let duration = record
                .granule_position
                .filter(|granule| *granule > pts)
                .map(|granule| granule - pts)
                .unwrap_or(default_duration);
            packets.push(
                EncodedPacket::new(
                    track_id,
                    parsed.codec.clone(),
                    pts,
                    duration,
                    stream.time_base,
                    record.data.clone(),
                )
                .with_keyframe(true),
            );
            pts = pts.saturating_add(duration);
        }
        stream.duration = Some(pts);
        if pts > 0 {
            media.duration_seconds = Some(
                media
                    .duration_seconds
                    .unwrap_or_default()
                    .max(pts as f64 / parsed.sample_rate as f64),
            );
        }
        media.push_stream(stream);
    }

    if media.streams.is_empty() {
        return Err(Error::Unsupported {
            operation: "ogg demux",
            reason: "no supported Opus, Vorbis, or FLAC stream was found",
        });
    }

    validate_monotonic_by_track(&packets)?;
    Ok(DemuxedMedia::new(ContainerFormat::Ogg, media, packets))
}

/// Mux packet-copy Opus, Vorbis, or FLAC packets into Ogg bytes.
pub fn mux_ogg_bytes(media: &MediaInfo, packets: &[EncodedPacket]) -> Result<Bytes> {
    let stream = select_ogg_stream(media)?;
    let muxer = OggMuxer::new();
    if !muxer.supports_stream(stream) {
        return Err(Error::Unsupported {
            operation: "ogg mux",
            reason: "Ogg muxing currently supports one Opus, Vorbis, or FLAC audio stream",
        });
    }
    validate_monotonic_by_track(packets)?;

    let mut stream_packets = packets
        .iter()
        .filter(|packet| packet.track_id == stream.track_id)
        .collect::<Vec<_>>();
    if stream_packets.is_empty() {
        return Err(Error::EmptyInput);
    }
    stream_packets.sort_by_key(|packet| packet.decode_order_ts());

    let serial = stream
        .tags
        .get("ogg_serial")
        .and_then(|value| value.parse::<u32>().ok())
        .unwrap_or_else(|| 0x7655_0000_u32.wrapping_add(stream.track_id));
    let mut sequence = 0_u32;
    let mut out = BytesMut::new();

    let headers = ogg_header_packets(stream)?;
    for (index, header) in headers.iter().enumerate() {
        let header_type = if index == 0 { 0x02 } else { 0x00 };
        write_packet_page(&mut out, serial, &mut sequence, header_type, 0, header)?;
    }

    for (index, packet) in stream_packets.iter().enumerate() {
        if packet.codec != stream.codec {
            return Err(Error::CodecMismatch {
                expected: stream.codec.clone(),
                actual: packet.codec.clone(),
            });
        }
        let header_type = if index + 1 == stream_packets.len() {
            0x04
        } else {
            0x00
        };
        let granule = packet.end_pts().max(packet.pts);
        write_packet_page(
            &mut out,
            serial,
            &mut sequence,
            header_type,
            granule as u64,
            &packet.data,
        )?;
    }

    Ok(out.freeze())
}

fn select_ogg_stream(media: &MediaInfo) -> Result<&StreamInfo> {
    let audio = media.audio_streams().collect::<Vec<_>>();
    if audio.len() != 1 {
        return Err(Error::Unsupported {
            operation: "ogg mux",
            reason: "Ogg muxing currently requires exactly one audio stream",
        });
    }
    if media
        .streams
        .iter()
        .any(|stream| stream.media_type != MediaType::Audio)
    {
        return Err(Error::Unsupported {
            operation: "ogg mux",
            reason: "Ogg muxing cannot preserve non-audio streams",
        });
    }
    Ok(audio[0])
}

fn ogg_header_packets(stream: &StreamInfo) -> Result<Vec<Bytes>> {
    match stream.codec {
        CodecId::Opus => {
            let head = stream
                .codec_config
                .as_ref()
                .filter(|config| config.starts_with(b"OpusHead"))
                .cloned()
                .unwrap_or_else(|| build_opus_head(stream));
            Ok(vec![head, build_opus_tags()])
        }
        CodecId::Vorbis => {
            let config = stream.codec_config.as_ref().ok_or(Error::Unsupported {
                operation: "ogg mux",
                reason: "Vorbis Ogg muxing requires three Xiph-laced header packets in codec_config",
            })?;
            let headers = xiph_laced_headers(config)?;
            if headers.len() != 3 || !headers[0].starts_with(b"\x01vorbis") {
                return Err(Error::Unsupported {
                    operation: "ogg mux",
                    reason: "Vorbis codec_config must contain identification, comment, and setup headers",
                });
            }
            Ok(headers)
        }
        CodecId::Flac => {
            let config = stream.codec_config.as_ref().ok_or(Error::Unsupported {
                operation: "ogg mux",
                reason: "FLAC Ogg muxing requires an Ogg FLAC identification packet in codec_config",
            })?;
            Ok(vec![config.clone()])
        }
        _ => Err(Error::Unsupported {
            operation: "ogg mux",
            reason: "unsupported Ogg codec",
        }),
    }
}

fn parse_stream_headers(
    records: &[OggPacketRecord],
    first: &OggPacketRecord,
) -> Result<ParsedOggStream> {
    if first.data.starts_with(b"OpusHead") {
        let (sample_rate, channels) = parse_opus_head(&first.data)?;
        let header_count = if records
            .get(1)
            .is_some_and(|record| record.data.starts_with(b"OpusTags"))
        {
            2
        } else {
            1
        };
        return Ok(ParsedOggStream {
            codec: CodecId::Opus,
            sample_rate,
            channels,
            codec_config: Some(first.data.clone()),
            header_count,
        });
    }

    if first.data.starts_with(b"\x01vorbis") {
        let (sample_rate, channels) = parse_vorbis_ident(&first.data)?;
        let headers = records
            .iter()
            .take(3)
            .map(|record| record.data.clone())
            .collect::<Vec<_>>();
        if headers.len() != 3 {
            return Err(Error::Parse {
                format: "ogg",
                message: "Vorbis stream is missing required header packets".to_owned(),
            });
        }
        return Ok(ParsedOggStream {
            codec: CodecId::Vorbis,
            sample_rate,
            channels,
            codec_config: Some(xiph_lace_headers(&headers)?),
            header_count: 3,
        });
    }

    if first.data.starts_with(b"\x7fFLAC") || first.data.starts_with(b"fLaC") {
        let (sample_rate, channels) = parse_flac_header(&first.data).unwrap_or((48_000, 2));
        return Ok(ParsedOggStream {
            codec: CodecId::Flac,
            sample_rate,
            channels,
            codec_config: Some(first.data.clone()),
            header_count: 1,
        });
    }

    Err(Error::Unsupported {
        operation: "ogg demux",
        reason: "unsupported Ogg stream codec",
    })
}

fn parse_opus_head(data: &[u8]) -> Result<(u32, u16)> {
    if data.len() < 19 {
        return Err(Error::Parse {
            format: "ogg",
            message: "OpusHead packet is too short".to_owned(),
        });
    }
    let channels = u16::from(data[9]);
    let declared_rate = u32::from_le_bytes([data[12], data[13], data[14], data[15]]);
    Ok((declared_rate.max(OPUS_SAMPLE_RATE), channels.max(1)))
}

fn parse_vorbis_ident(data: &[u8]) -> Result<(u32, u16)> {
    if data.len() < 30 || !data.starts_with(b"\x01vorbis") {
        return Err(Error::Parse {
            format: "ogg",
            message: "Vorbis identification packet is invalid".to_owned(),
        });
    }
    let channels = u16::from(data[11]).max(1);
    let sample_rate = u32::from_le_bytes([data[12], data[13], data[14], data[15]]).max(1);
    Ok((sample_rate, channels))
}

fn parse_flac_header(data: &[u8]) -> Option<(u32, u16)> {
    let streaminfo = if data.starts_with(b"fLaC") {
        data.windows(4)
            .position(|window| window == [0x00, 0x00, 0x00, 0x22])
            .and_then(|offset| data.get(offset + 4..offset + 38))
    } else {
        data.windows(4)
            .position(|window| window == [0x00, 0x00, 0x00, 0x22])
            .and_then(|offset| data.get(offset + 4..offset + 38))
    }?;
    if streaminfo.len() < 18 {
        return None;
    }
    let packed = u64::from_be_bytes([
        0,
        0,
        0,
        streaminfo[10],
        streaminfo[11],
        streaminfo[12],
        streaminfo[13],
        streaminfo[14],
    ]);
    let sample_rate = ((packed >> 44) & 0x0f_ffff) as u32;
    let channels = (((packed >> 41) & 0x07) + 1) as u16;
    Some((sample_rate.max(1), channels.max(1)))
}

fn build_opus_head(stream: &StreamInfo) -> Bytes {
    let channels = stream.channels.unwrap_or(2).min(u8::MAX as u16) as u8;
    let sample_rate = stream.sample_rate.unwrap_or(OPUS_SAMPLE_RATE);
    let mut out = BytesMut::with_capacity(19);
    out.extend_from_slice(b"OpusHead");
    out.put_u8(1);
    out.put_u8(channels);
    out.put_u16_le(0);
    out.put_u32_le(sample_rate);
    out.put_i16_le(0);
    out.put_u8(0);
    out.freeze()
}

fn build_opus_tags() -> Bytes {
    let vendor = b"video-utils-rs";
    let mut out = BytesMut::new();
    out.extend_from_slice(b"OpusTags");
    out.put_u32_le(vendor.len() as u32);
    out.extend_from_slice(vendor);
    out.put_u32_le(0);
    out.freeze()
}

fn xiph_laced_headers(config: &[u8]) -> Result<Vec<Bytes>> {
    let Some((&count_minus_one, mut rest)) = config.split_first() else {
        return Err(Error::Parse {
            format: "xiph",
            message: "empty Xiph header block".to_owned(),
        });
    };
    let header_count = usize::from(count_minus_one) + 1;
    if header_count < 2 {
        return Err(Error::Parse {
            format: "xiph",
            message: "Xiph header block must contain at least two headers".to_owned(),
        });
    }

    let mut sizes = Vec::with_capacity(header_count - 1);
    for _ in 0..header_count - 1 {
        let mut size = 0_usize;
        loop {
            let Some((&byte, tail)) = rest.split_first() else {
                return Err(Error::Parse {
                    format: "xiph",
                    message: "Xiph header block ended inside a lacing size".to_owned(),
                });
            };
            rest = tail;
            size += usize::from(byte);
            if byte != 255 {
                break;
            }
        }
        sizes.push(size);
    }

    let mut headers = Vec::with_capacity(header_count);
    for size in sizes {
        if rest.len() < size {
            return Err(Error::Parse {
                format: "xiph",
                message: "Xiph header size exceeds remaining bytes".to_owned(),
            });
        }
        headers.push(Bytes::copy_from_slice(&rest[..size]));
        rest = &rest[size..];
    }
    headers.push(Bytes::copy_from_slice(rest));
    Ok(headers)
}

fn xiph_lace_headers(headers: &[Bytes]) -> Result<Bytes> {
    if headers.len() < 2 || headers.len() > u8::MAX as usize + 1 {
        return Err(Error::Unsupported {
            operation: "xiph headers",
            reason: "Xiph lacing requires two to 256 headers",
        });
    }
    let mut out = BytesMut::new();
    out.put_u8((headers.len() - 1) as u8);
    for header in &headers[..headers.len() - 1] {
        let mut len = header.len();
        while len >= 255 {
            out.put_u8(255);
            len -= 255;
        }
        out.put_u8(len as u8);
    }
    for header in headers {
        out.extend_from_slice(header);
    }
    Ok(out.freeze())
}

fn default_packet_duration(codec: &CodecId) -> i64 {
    match codec {
        CodecId::Opus => DEFAULT_OPUS_PACKET_DURATION,
        CodecId::Vorbis => DEFAULT_VORBIS_PACKET_DURATION,
        _ => 0,
    }
}

fn parse_pages(bytes: &Bytes) -> Result<Vec<OggPage>> {
    let mut pages = Vec::new();
    let mut offset = 0_usize;
    while offset < bytes.len() {
        if bytes.len().saturating_sub(offset) < 27 {
            return Err(Error::Parse {
                format: "ogg",
                message: "trailing partial Ogg page header".to_owned(),
            });
        }
        if &bytes[offset..offset + 4] != OGG_CAPTURE {
            return Err(Error::Parse {
                format: "ogg",
                message: "missing Ogg capture pattern".to_owned(),
            });
        }
        let version = bytes[offset + 4];
        if version != 0 {
            return Err(Error::Parse {
                format: "ogg",
                message: "unsupported Ogg bitstream version".to_owned(),
            });
        }
        let granule_raw = read_u64_le(bytes, offset + 6)?;
        let serial = read_u32_le(bytes, offset + 14)?;
        let segments_len = usize::from(bytes[offset + 26]);
        let lacing_start = offset + 27;
        let payload_start = lacing_start + segments_len;
        if payload_start > bytes.len() {
            return Err(Error::Parse {
                format: "ogg",
                message: "Ogg page lacing table exceeds object length".to_owned(),
            });
        }
        let lacing = bytes[lacing_start..payload_start].to_vec();
        let payload_len = lacing
            .iter()
            .map(|value| usize::from(*value))
            .sum::<usize>();
        let payload_end = payload_start + payload_len;
        if payload_end > bytes.len() {
            return Err(Error::Parse {
                format: "ogg",
                message: "Ogg page payload exceeds object length".to_owned(),
            });
        }
        pages.push(OggPage {
            granule_position: if granule_raw == u64::MAX {
                None
            } else {
                Some(granule_raw as i64)
            },
            serial,
            lacing,
            payload: Bytes::copy_from_slice(&bytes[payload_start..payload_end]),
        });
        offset = payload_end;
    }

    Ok(pages)
}

fn collect_packet_records(pages: &[OggPage]) -> Vec<OggPacketRecord> {
    let mut buffers = BTreeMap::<u32, BytesMut>::new();
    let mut records = Vec::new();

    for page in pages {
        let mut payload_offset = 0_usize;
        for (segment_index, segment_len) in page.lacing.iter().copied().enumerate() {
            let segment_len = usize::from(segment_len);
            let segment = &page.payload[payload_offset..payload_offset + segment_len];
            payload_offset += segment_len;

            let buffer = buffers.entry(page.serial).or_default();
            buffer.extend_from_slice(segment);
            if segment_len < 255 {
                let data = buffer.split().freeze();
                let is_last_segment = segment_index + 1 == page.lacing.len();
                records.push(OggPacketRecord {
                    serial: page.serial,
                    granule_position: page.granule_position.filter(|_| is_last_segment),
                    data,
                });
            }
        }
    }

    records
}

fn write_packet_page(
    out: &mut BytesMut,
    serial: u32,
    sequence: &mut u32,
    header_type: u8,
    granule_position: u64,
    packet: &[u8],
) -> Result<()> {
    let lacing = packet_lacing(packet.len())?;
    let payload_len = packet.len();
    let mut page = BytesMut::with_capacity(27 + lacing.len() + payload_len);
    page.extend_from_slice(OGG_CAPTURE);
    page.put_u8(0);
    page.put_u8(header_type);
    page.put_u64_le(granule_position);
    page.put_u32_le(serial);
    page.put_u32_le(*sequence);
    page.put_u32_le(0);
    page.put_u8(lacing.len() as u8);
    page.extend_from_slice(&lacing);
    page.extend_from_slice(packet);

    let checksum = ogg_crc(&page);
    page[22..26].copy_from_slice(&checksum.to_le_bytes());
    out.extend_from_slice(&page);
    *sequence = sequence.wrapping_add(1);
    Ok(())
}

fn packet_lacing(len: usize) -> Result<Vec<u8>> {
    if len > 65_025 {
        return Err(Error::Unsupported {
            operation: "ogg mux",
            reason: "single Ogg packet pages currently support up to 65,025 bytes",
        });
    }
    let mut remaining = len;
    let mut lacing = Vec::new();
    while remaining >= 255 {
        lacing.push(255);
        remaining -= 255;
    }
    lacing.push(remaining as u8);
    if lacing.len() > 255 {
        return Err(Error::Unsupported {
            operation: "ogg mux",
            reason: "single Ogg packet pages currently support at most 255 lacing entries",
        });
    }
    Ok(lacing)
}

fn ogg_crc(bytes: &[u8]) -> u32 {
    let mut crc = 0_u32;
    for byte in bytes {
        crc ^= u32::from(*byte) << 24;
        for _ in 0..8 {
            crc = if crc & 0x8000_0000 != 0 {
                (crc << 1) ^ 0x04c1_1db7
            } else {
                crc << 1
            };
        }
    }
    crc
}

fn read_u32_le(bytes: &Bytes, offset: usize) -> Result<u32> {
    bytes
        .get(offset..offset + 4)
        .map(|bytes| u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
        .ok_or_else(|| Error::Parse {
            format: "ogg",
            message: "unexpected end of Ogg page".to_owned(),
        })
}

fn read_u64_le(bytes: &Bytes, offset: usize) -> Result<u64> {
    bytes
        .get(offset..offset + 8)
        .map(|bytes| {
            u64::from_le_bytes([
                bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
            ])
        })
        .ok_or_else(|| Error::Parse {
            format: "ogg",
            message: "unexpected end of Ogg page".to_owned(),
        })
}

#[derive(Clone, Debug)]
struct OggPage {
    granule_position: Option<i64>,
    serial: u32,
    lacing: Vec<u8>,
    payload: Bytes,
}

#[derive(Clone, Debug)]
struct OggPacketRecord {
    serial: u32,
    granule_position: Option<i64>,
    data: Bytes,
}

#[derive(Clone, Debug)]
struct ParsedOggStream {
    codec: CodecId,
    sample_rate: u32,
    channels: u16,
    codec_config: Option<Bytes>,
    header_count: usize,
}

#[cfg(test)]
mod tests {
    use super::{demux_ogg_bytes, mux_ogg_bytes, xiph_lace_headers};
    use crate::{CodecId, EncodedPacket, MediaInfo, MediaType, StreamInfo, TimeBase};
    use bytes::Bytes;

    #[test]
    fn ogg_mux_demux_round_trips_opus_packets() {
        let time_base = TimeBase::new(1, 48_000).unwrap();
        let mut media = MediaInfo::default();
        media.push_stream(
            StreamInfo::new(1, MediaType::Audio, CodecId::Opus, time_base)
                .with_audio_format(48_000, 2),
        );
        let packets = vec![
            packet(CodecId::Opus, 0, 960, b"opus-a"),
            packet(CodecId::Opus, 960, 960, b"opus-b"),
        ];

        let bytes = mux_ogg_bytes(&media, &packets).unwrap();
        assert_eq!(&bytes[..4], b"OggS");
        let demuxed = demux_ogg_bytes(&bytes).unwrap();

        assert_eq!(demuxed.media.streams[0].codec, CodecId::Opus);
        assert_eq!(demuxed.media.streams[0].channels, Some(2));
        assert_eq!(demuxed.packets.len(), 2);
        assert_eq!(demuxed.packets[0].data, Bytes::from_static(b"opus-a"));
    }

    #[test]
    fn ogg_mux_demux_round_trips_vorbis_with_xiph_headers() {
        let time_base = TimeBase::new(1, 44_100).unwrap();
        let mut ident = vec![0; 30];
        ident[..7].copy_from_slice(b"\x01vorbis");
        ident[11] = 2;
        ident[12..16].copy_from_slice(&44_100_u32.to_le_bytes());
        let headers = vec![
            Bytes::from(ident),
            Bytes::from_static(b"\x03vorbis-comment"),
            Bytes::from_static(b"\x05vorbis-setup"),
        ];
        let mut stream = StreamInfo::new(1, MediaType::Audio, CodecId::Vorbis, time_base)
            .with_audio_format(44_100, 2);
        stream.codec_config = Some(xiph_lace_headers(&headers).unwrap());
        let mut media = MediaInfo::default();
        media.push_stream(stream);

        let bytes =
            mux_ogg_bytes(&media, &[packet(CodecId::Vorbis, 0, 1024, b"vorbis-a")]).unwrap();
        let demuxed = demux_ogg_bytes(&bytes).unwrap();

        assert_eq!(demuxed.media.streams[0].codec, CodecId::Vorbis);
        assert_eq!(demuxed.media.streams[0].sample_rate, Some(44_100));
        assert_eq!(demuxed.packets[0].data, Bytes::from_static(b"vorbis-a"));
    }

    #[test]
    fn ogg_mux_demux_round_trips_flac_with_config() {
        let time_base = TimeBase::new(1, 48_000).unwrap();
        let mut stream = StreamInfo::new(1, MediaType::Audio, CodecId::Flac, time_base)
            .with_audio_format(48_000, 2);
        stream.codec_config = Some(Bytes::from_static(b"\x7fFLAC\x01\0\0\0\0fLaC"));
        let mut media = MediaInfo::default();
        media.push_stream(stream);

        let bytes = mux_ogg_bytes(&media, &[packet(CodecId::Flac, 0, 0, b"flac-frame")]).unwrap();
        let demuxed = demux_ogg_bytes(&bytes).unwrap();

        assert_eq!(demuxed.media.streams[0].codec, CodecId::Flac);
        assert_eq!(demuxed.packets[0].data, Bytes::from_static(b"flac-frame"));
    }

    fn packet(codec: CodecId, pts: i64, duration: i64, data: &'static [u8]) -> EncodedPacket {
        EncodedPacket::new(
            1,
            codec,
            pts,
            duration,
            TimeBase::new(1, 48_000).unwrap(),
            Bytes::from_static(data),
        )
        .with_keyframe(true)
    }
}
