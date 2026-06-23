use crate::{
    codec::{CodecId, MediaType},
    container::{ContainerDemuxer, ContainerFormat, ContainerMuxer, DemuxedMedia},
    containers::wav::{PcmEncoding, PcmSampleFormat, pcm_encoding_from_stream, set_pcm_tags},
    error::{Error, Result},
    media::{MediaInfo, StreamInfo},
    packet::{EncodedPacket, validate_monotonic_by_track},
    time::TimeBase,
};
use bytes::{BufMut, Bytes, BytesMut};

const FORM_HEADER_LEN: usize = 12;

/// AIFF/AIFC demuxer for uncompressed PCM audio.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub struct AiffDemuxer;

impl AiffDemuxer {
    /// Create an AIFF demuxer.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl ContainerDemuxer for AiffDemuxer {
    fn container_format(&self) -> ContainerFormat {
        ContainerFormat::Aiff
    }

    fn demux_bytes(&self, bytes: &Bytes) -> Result<DemuxedMedia> {
        demux_aiff_bytes(bytes)
    }
}

/// AIFF muxer for uncompressed PCM audio.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub struct AiffMuxer;

impl AiffMuxer {
    /// Create an AIFF muxer.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl ContainerMuxer for AiffMuxer {
    fn container_format(&self) -> ContainerFormat {
        ContainerFormat::Aiff
    }

    fn supports_stream(&self, stream: &StreamInfo) -> bool {
        stream.media_type == MediaType::Audio && stream.codec == CodecId::Pcm
    }

    fn mux_bytes(&self, media: &MediaInfo, packets: &[EncodedPacket]) -> Result<Bytes> {
        mux_aiff_bytes(media, packets)
    }
}

/// Probe stream metadata from AIFF/AIFC bytes.
pub fn probe_aiff_bytes(bytes: &Bytes) -> Result<MediaInfo> {
    demux_aiff_bytes(bytes).map(|demuxed| demuxed.media)
}

/// Demux AIFF/AIFC uncompressed PCM bytes into one PCM stream and packet.
pub fn demux_aiff_bytes(bytes: &Bytes) -> Result<DemuxedMedia> {
    let parsed = parse_aiff(bytes)?;
    let block_align = parsed
        .channels
        .checked_mul(bytes_per_sample(parsed.encoding)?)
        .ok_or(Error::Unsupported {
            operation: "aiff demux",
            reason: "AIFF block alignment is too large",
        })?;
    let duration = parsed.data.len() / usize::from(block_align);
    let time_base = TimeBase::new(1, parsed.sample_rate as i32)?;

    let mut stream = StreamInfo::new(1, MediaType::Audio, CodecId::Pcm, time_base)
        .with_audio_format(parsed.sample_rate, parsed.channels);
    stream.duration = Some(duration as i64);
    set_pcm_tags(&mut stream, parsed.encoding, block_align);

    let data = aiff_pcm_to_internal(&parsed.data, parsed.encoding)?;
    let packet = EncodedPacket::new(1, CodecId::Pcm, 0, duration as i64, time_base, data)
        .with_keyframe(true);

    let mut media = MediaInfo {
        duration_seconds: Some(duration as f64 / parsed.sample_rate as f64),
        ..Default::default()
    };
    media.push_stream(stream);

    Ok(DemuxedMedia::new(
        ContainerFormat::Aiff,
        media,
        vec![packet],
    ))
}

/// Mux one PCM audio stream into AIFF bytes.
pub fn mux_aiff_bytes(media: &MediaInfo, packets: &[EncodedPacket]) -> Result<Bytes> {
    let stream = select_aiff_stream(media)?;
    let encoding = pcm_encoding_from_stream(stream)?;
    validate_aiff_pcm_encoding(encoding)?;
    let channels = stream.channels.ok_or(Error::Unsupported {
        operation: "aiff mux",
        reason: "audio channel count is required for AIFF muxing",
    })?;
    let sample_rate = stream.sample_rate.ok_or(Error::Unsupported {
        operation: "aiff mux",
        reason: "audio sample rate is required for AIFF muxing",
    })?;
    let block_align =
        channels
            .checked_mul(bytes_per_sample(encoding)?)
            .ok_or(Error::Unsupported {
                operation: "aiff mux",
                reason: "AIFF block alignment is too large",
            })?;
    validate_monotonic_by_track(packets)?;

    let mut ordered = packets
        .iter()
        .filter(|packet| packet.track_id == stream.track_id)
        .collect::<Vec<_>>();
    if ordered.is_empty() {
        return Err(Error::EmptyInput);
    }
    ordered.sort_by_key(|packet| packet.decode_order_ts());

    let mut internal_data = BytesMut::new();
    for packet in ordered {
        if packet.codec != CodecId::Pcm {
            return Err(Error::CodecMismatch {
                expected: CodecId::Pcm,
                actual: packet.codec.clone(),
            });
        }
        if !packet.data.len().is_multiple_of(usize::from(block_align)) {
            return Err(Error::InvalidAudioBuffer {
                reason: "PCM packet byte length is not aligned to the stream block size",
            });
        }
        internal_data.extend_from_slice(&packet.data);
    }
    let sample_frames = internal_data.len() / usize::from(block_align);
    let aiff_data = internal_pcm_to_aiff(&internal_data, encoding)?;
    write_aiff_bytes(sample_rate, channels, encoding, sample_frames, &aiff_data)
}

#[derive(Clone, Debug)]
struct ParsedAiff {
    sample_rate: u32,
    channels: u16,
    encoding: PcmEncoding,
    data: Bytes,
}

fn parse_aiff(bytes: &Bytes) -> Result<ParsedAiff> {
    if bytes.len() < FORM_HEADER_LEN || &bytes[0..4] != b"FORM" {
        return Err(Error::Parse {
            format: "aiff",
            message: "input is not an AIFF FORM object".to_owned(),
        });
    }
    let form_type = &bytes[8..12];
    if form_type != b"AIFF" && form_type != b"AIFC" {
        return Err(Error::Parse {
            format: "aiff",
            message: "FORM object is not AIFF or AIFC".to_owned(),
        });
    }

    let mut offset = FORM_HEADER_LEN;
    let mut comm = None::<CommChunk>;
    let mut data = None::<Bytes>;
    while offset + 8 <= bytes.len() {
        let id = &bytes[offset..offset + 4];
        let size = read_u32_be(bytes, offset + 4)? as usize;
        offset += 8;
        if offset + size > bytes.len() {
            return Err(Error::Parse {
                format: "aiff",
                message: "AIFF chunk length exceeds object length".to_owned(),
            });
        }
        let chunk = &bytes[offset..offset + size];
        match id {
            b"COMM" => comm = Some(parse_comm_chunk(form_type, chunk)?),
            b"SSND" => data = Some(parse_ssnd_chunk(chunk)?),
            _ => {}
        }
        offset += size + (size % 2);
    }

    let comm = comm.ok_or(Error::Parse {
        format: "aiff",
        message: "missing COMM chunk".to_owned(),
    })?;
    let data = data.ok_or(Error::Parse {
        format: "aiff",
        message: "missing SSND chunk".to_owned(),
    })?;
    validate_aiff_pcm_encoding(comm.encoding)?;
    Ok(ParsedAiff {
        sample_rate: comm.sample_rate,
        channels: comm.channels,
        encoding: comm.encoding,
        data,
    })
}

#[derive(Clone, Copy, Debug)]
struct CommChunk {
    sample_rate: u32,
    channels: u16,
    encoding: PcmEncoding,
}

fn parse_comm_chunk(form_type: &[u8], chunk: &[u8]) -> Result<CommChunk> {
    if chunk.len() < 18 {
        return Err(Error::Parse {
            format: "aiff",
            message: "COMM chunk is too short".to_owned(),
        });
    }
    if form_type == b"AIFC" {
        if chunk.len() < 22 {
            return Err(Error::Parse {
                format: "aiff",
                message: "AIFC COMM chunk is missing compression type".to_owned(),
            });
        }
        let compression = &chunk[18..22];
        if compression != b"NONE" {
            return Err(Error::Unsupported {
                operation: "aiff demux",
                reason: "only uncompressed AIFC/AIFF PCM is supported",
            });
        }
    }
    Ok(CommChunk {
        channels: read_u16_be_slice(chunk, 0)?,
        sample_rate: decode_extended_sample_rate(&chunk[8..18])?,
        encoding: PcmEncoding::new(PcmSampleFormat::Integer, read_u16_be_slice(chunk, 6)?),
    })
}

fn parse_ssnd_chunk(chunk: &[u8]) -> Result<Bytes> {
    if chunk.len() < 8 {
        return Err(Error::Parse {
            format: "aiff",
            message: "SSND chunk is too short".to_owned(),
        });
    }
    let offset = read_u32_be_slice(chunk, 0)? as usize;
    let data_start = 8usize.checked_add(offset).ok_or(Error::Parse {
        format: "aiff",
        message: "SSND offset overflows".to_owned(),
    })?;
    if data_start > chunk.len() {
        return Err(Error::Parse {
            format: "aiff",
            message: "SSND offset exceeds chunk length".to_owned(),
        });
    }
    Ok(Bytes::copy_from_slice(&chunk[data_start..]))
}

fn write_aiff_bytes(
    sample_rate: u32,
    channels: u16,
    encoding: PcmEncoding,
    sample_frames: usize,
    data: &[u8],
) -> Result<Bytes> {
    let comm_len = 18_u32;
    let ssnd_payload_len = u32::try_from(8usize + data.len()).map_err(|_| Error::Unsupported {
        operation: "aiff mux",
        reason: "AIFF sound data is too large",
    })?;
    let form_len = 4_u32
        .checked_add(8 + comm_len)
        .and_then(|value| value.checked_add(8 + ssnd_payload_len + (ssnd_payload_len % 2)))
        .ok_or(Error::Unsupported {
            operation: "aiff mux",
            reason: "AIFF object is too large",
        })?;
    let sample_frames = u32::try_from(sample_frames).map_err(|_| Error::Unsupported {
        operation: "aiff mux",
        reason: "AIFF sample frame count exceeds 32-bit COMM field",
    })?;

    let mut out = BytesMut::with_capacity(form_len as usize + 8);
    out.extend_from_slice(b"FORM");
    out.put_u32(form_len);
    out.extend_from_slice(b"AIFF");
    out.extend_from_slice(b"COMM");
    out.put_u32(comm_len);
    out.put_u16(channels);
    out.put_u32(sample_frames);
    out.put_u16(encoding.bits_per_sample);
    out.extend_from_slice(&encode_extended_sample_rate(sample_rate));
    out.extend_from_slice(b"SSND");
    out.put_u32(ssnd_payload_len);
    out.put_u32(0);
    out.put_u32(0);
    out.extend_from_slice(data);
    if ssnd_payload_len % 2 == 1 {
        out.put_u8(0);
    }
    Ok(out.freeze())
}

fn select_aiff_stream(media: &MediaInfo) -> Result<&StreamInfo> {
    let audio = media.audio_streams().collect::<Vec<_>>();
    if audio.len() != 1 {
        return Err(Error::Unsupported {
            operation: "aiff mux",
            reason: "AIFF muxing requires exactly one audio stream",
        });
    }
    if media
        .streams
        .iter()
        .any(|stream| stream.media_type != MediaType::Audio)
    {
        return Err(Error::Unsupported {
            operation: "aiff mux",
            reason: "AIFF muxing cannot preserve non-audio streams",
        });
    }
    Ok(audio[0])
}

fn validate_aiff_pcm_encoding(encoding: PcmEncoding) -> Result<()> {
    match (encoding.sample_format, encoding.bits_per_sample) {
        (PcmSampleFormat::Integer, 8 | 16 | 24 | 32) => Ok(()),
        (PcmSampleFormat::Float, _) => Err(Error::Unsupported {
            operation: "aiff pcm",
            reason: "AIFF float PCM is not currently supported",
        }),
        (PcmSampleFormat::Integer, _) => Err(Error::Unsupported {
            operation: "aiff pcm",
            reason: "AIFF integer PCM currently supports 8, 16, 24, or 32 bits per sample",
        }),
    }
}

fn aiff_pcm_to_internal(data: &[u8], encoding: PcmEncoding) -> Result<Bytes> {
    swap_pcm_endian(data, encoding)
}

fn internal_pcm_to_aiff(data: &[u8], encoding: PcmEncoding) -> Result<Bytes> {
    swap_pcm_endian(data, encoding)
}

fn swap_pcm_endian(data: &[u8], encoding: PcmEncoding) -> Result<Bytes> {
    validate_aiff_pcm_encoding(encoding)?;
    let width = usize::from(bytes_per_sample(encoding)?);
    if width == 1 {
        return Ok(Bytes::copy_from_slice(data));
    }
    if !data.len().is_multiple_of(width) {
        return Err(Error::InvalidAudioBuffer {
            reason: "PCM data has a trailing partial sample",
        });
    }
    let mut out = BytesMut::with_capacity(data.len());
    for sample in data.chunks_exact(width) {
        for byte in sample.iter().rev() {
            out.put_u8(*byte);
        }
    }
    Ok(out.freeze())
}

fn bytes_per_sample(encoding: PcmEncoding) -> Result<u16> {
    validate_aiff_pcm_encoding(encoding)?;
    Ok(encoding.bits_per_sample / 8)
}

fn decode_extended_sample_rate(bytes: &[u8]) -> Result<u32> {
    if bytes.len() != 10 {
        return Err(Error::Parse {
            format: "aiff",
            message: "sample-rate extended float is not 10 bytes".to_owned(),
        });
    }
    let sign = bytes[0] & 0x80 != 0;
    let exponent = (u16::from(bytes[0] & 0x7f) << 8) | u16::from(bytes[1]);
    let mantissa = u64::from_be_bytes(bytes[2..10].try_into().unwrap());
    if sign || exponent == 0 || mantissa == 0 {
        return Err(Error::Unsupported {
            operation: "aiff demux",
            reason: "AIFF sample rate must be positive",
        });
    }
    let exp = i32::from(exponent) - 16_383 - 63;
    let rate = if exp >= 0 {
        (mantissa as f64) * 2f64.powi(exp)
    } else {
        (mantissa as f64) / 2f64.powi(-exp)
    };
    if !rate.is_finite() || rate <= 0.0 || rate > u32::MAX as f64 {
        return Err(Error::Unsupported {
            operation: "aiff demux",
            reason: "AIFF sample rate is outside u32 range",
        });
    }
    Ok(rate.round() as u32)
}

fn encode_extended_sample_rate(sample_rate: u32) -> [u8; 10] {
    let value = sample_rate as f64;
    let exponent = value.log2().floor() as i32;
    let biased_exponent = (exponent + 16_383) as u16;
    let normalized = value / 2f64.powi(exponent);
    let mantissa = (normalized * 2f64.powi(63)).round() as u64;
    let mut out = [0u8; 10];
    out[0..2].copy_from_slice(&biased_exponent.to_be_bytes());
    out[2..10].copy_from_slice(&mantissa.to_be_bytes());
    out
}

fn read_u16_be_slice(bytes: &[u8], offset: usize) -> Result<u16> {
    if bytes.len().saturating_sub(offset) < 2 {
        return Err(Error::Parse {
            format: "aiff",
            message: "unexpected end of AIFF chunk".to_owned(),
        });
    }
    Ok(u16::from_be_bytes(
        bytes[offset..offset + 2].try_into().unwrap(),
    ))
}

fn read_u32_be_slice(bytes: &[u8], offset: usize) -> Result<u32> {
    if bytes.len().saturating_sub(offset) < 4 {
        return Err(Error::Parse {
            format: "aiff",
            message: "unexpected end of AIFF chunk".to_owned(),
        });
    }
    Ok(u32::from_be_bytes(
        bytes[offset..offset + 4].try_into().unwrap(),
    ))
}

fn read_u32_be(bytes: &Bytes, offset: usize) -> Result<u32> {
    read_u32_be_slice(bytes, offset)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn muxes_and_demuxes_aiff_pcm() {
        let time_base = TimeBase::new(1, 44_100).unwrap();
        let mut media = MediaInfo::default();
        let mut stream = StreamInfo::new(1, MediaType::Audio, CodecId::Pcm, time_base)
            .with_audio_format(44_100, 1);
        set_pcm_tags(&mut stream, PcmEncoding::signed_16(), 2);
        media.push_stream(stream);
        let packet = EncodedPacket::new(
            1,
            CodecId::Pcm,
            0,
            2,
            time_base,
            Bytes::from_static(&[0x01, 0x02, 0x03, 0x04]),
        )
        .with_keyframe(true);

        let bytes = mux_aiff_bytes(&media, &[packet]).unwrap();
        let demuxed = demux_aiff_bytes(&bytes).unwrap();

        assert_eq!(&bytes[0..4], b"FORM");
        assert_eq!(&bytes[8..12], b"AIFF");
        assert_eq!(demuxed.media.streams[0].sample_rate, Some(44_100));
        assert_eq!(demuxed.media.streams[0].channels, Some(1));
        assert_eq!(&demuxed.packets[0].data[..], &[0x01, 0x02, 0x03, 0x04]);
    }

    #[test]
    fn round_trips_extended_sample_rate() {
        let encoded = encode_extended_sample_rate(48_000);
        assert_eq!(decode_extended_sample_rate(&encoded).unwrap(), 48_000);
    }
}
