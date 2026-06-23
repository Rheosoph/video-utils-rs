use crate::{
    audio::AudioFrame,
    codec::{CodecId, MediaType},
    container::{ContainerDemuxer, ContainerFormat, ContainerMuxer, DemuxedMedia},
    error::{Error, Result},
    media::{MediaInfo, StreamInfo},
    packet::{EncodedPacket, validate_monotonic_by_track},
    time::TimeBase,
};
use bytes::{BufMut, Bytes, BytesMut};

const RIFF_HEADER_LEN: usize = 12;
const FORMAT_PCM_INTEGER: u16 = 0x0001;
const FORMAT_IEEE_FLOAT: u16 = 0x0003;

/// PCM sample representation carried by WAV packets.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PcmSampleFormat {
    /// Signed little-endian integer PCM.
    Integer,
    /// IEEE floating-point PCM.
    Float,
}

/// PCM encoding metadata used by WAV muxing and audio transforms.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PcmEncoding {
    /// Sample representation.
    pub sample_format: PcmSampleFormat,
    /// Bits per sample.
    pub bits_per_sample: u16,
}

impl PcmEncoding {
    /// Create a PCM encoding descriptor.
    #[must_use]
    pub const fn new(sample_format: PcmSampleFormat, bits_per_sample: u16) -> Self {
        Self {
            sample_format,
            bits_per_sample,
        }
    }

    /// Default signed 16-bit PCM.
    #[must_use]
    pub const fn signed_16() -> Self {
        Self::new(PcmSampleFormat::Integer, 16)
    }

    /// 32-bit floating-point PCM.
    #[must_use]
    pub const fn float_32() -> Self {
        Self::new(PcmSampleFormat::Float, 32)
    }

    fn format_tag(self) -> u16 {
        match self.sample_format {
            PcmSampleFormat::Integer => FORMAT_PCM_INTEGER,
            PcmSampleFormat::Float => FORMAT_IEEE_FLOAT,
        }
    }
}

/// WAV demuxer backed by a small RIFF/WAVE parser.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub struct WavDemuxer;

impl WavDemuxer {
    /// Create a WAV demuxer.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl ContainerDemuxer for WavDemuxer {
    fn container_format(&self) -> ContainerFormat {
        ContainerFormat::Wav
    }

    fn demux_bytes(&self, bytes: &Bytes) -> Result<DemuxedMedia> {
        demux_wav_bytes(bytes)
    }
}

/// WAV muxer backed by a small RIFF/WAVE writer.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub struct WavMuxer;

impl WavMuxer {
    /// Create a WAV muxer.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl ContainerMuxer for WavMuxer {
    fn container_format(&self) -> ContainerFormat {
        ContainerFormat::Wav
    }

    fn supports_stream(&self, stream: &StreamInfo) -> bool {
        stream.media_type == MediaType::Audio && stream.codec == CodecId::Pcm
    }

    fn mux_bytes(&self, media: &MediaInfo, packets: &[EncodedPacket]) -> Result<Bytes> {
        mux_wav_bytes(media, packets)
    }
}

/// Probe stream metadata from WAV bytes.
pub fn probe_wav_bytes(bytes: &Bytes) -> Result<MediaInfo> {
    demux_wav_bytes(bytes).map(|demuxed| demuxed.media)
}

/// Demux WAV bytes into one PCM stream and one raw PCM packet.
pub fn demux_wav_bytes(bytes: &Bytes) -> Result<DemuxedMedia> {
    let parsed = parse_wav(bytes)?;
    let block_align = parsed.block_align.max(1);
    let duration = parsed.data.len() / usize::from(block_align);
    let time_base = TimeBase::new(1, parsed.sample_rate as i32)?;

    let mut stream = StreamInfo::new(1, MediaType::Audio, CodecId::Pcm, time_base)
        .with_audio_format(parsed.sample_rate, parsed.channels);
    stream.duration = Some(duration as i64);
    set_pcm_tags(&mut stream, parsed.encoding, block_align);

    let packet = EncodedPacket::new(1, CodecId::Pcm, 0, duration as i64, time_base, parsed.data)
        .with_keyframe(true);

    let mut media = MediaInfo {
        duration_seconds: Some(duration as f64 / parsed.sample_rate as f64),
        ..Default::default()
    };
    media.push_stream(stream);

    Ok(DemuxedMedia::new(ContainerFormat::Wav, media, vec![packet]))
}

/// Mux raw PCM packets into WAV bytes.
pub fn mux_wav_bytes(media: &MediaInfo, packets: &[EncodedPacket]) -> Result<Bytes> {
    let muxer = WavMuxer::new();
    let stream = select_wav_stream(media)?;
    if !muxer.supports_stream(stream) {
        return Err(Error::Unsupported {
            operation: "wav mux",
            reason: "WAV muxing currently supports one PCM audio stream",
        });
    }
    validate_monotonic_by_track(packets)?;

    let encoding = pcm_encoding_from_stream(stream)?;
    validate_pcm_encoding(encoding)?;
    let channels = stream.channels.ok_or(Error::Unsupported {
        operation: "wav mux",
        reason: "audio channel count is required for WAV muxing",
    })?;
    let sample_rate = stream.sample_rate.ok_or(Error::Unsupported {
        operation: "wav mux",
        reason: "audio sample rate is required for WAV muxing",
    })?;
    let bytes_per_sample = bytes_per_sample(encoding)?;
    let block_align = channels
        .checked_mul(bytes_per_sample)
        .ok_or(Error::Unsupported {
            operation: "wav mux",
            reason: "WAV block alignment is too large",
        })?;

    let mut ordered = packets
        .iter()
        .filter(|packet| packet.track_id == stream.track_id)
        .collect::<Vec<_>>();
    if ordered.is_empty() {
        return Err(Error::EmptyInput);
    }
    ordered.sort_by_key(|packet| packet.decode_order_ts());

    let mut data = BytesMut::new();
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
        data.extend_from_slice(&packet.data);
    }

    write_wav_bytes(sample_rate, channels, encoding, block_align, &data)
}

/// Read PCM encoding metadata from stream tags.
pub fn pcm_encoding_from_stream(stream: &StreamInfo) -> Result<PcmEncoding> {
    let bits_per_sample = match stream.tags.get("bits_per_sample") {
        Some(value) => value.parse::<u16>().map_err(|err| Error::Parse {
            format: "wav",
            message: format!("invalid bits_per_sample tag: {err}"),
        })?,
        None => 16,
    };
    let sample_format = match stream.tags.get("sample_format").map(String::as_str) {
        Some("float") | Some("ieee-float") => PcmSampleFormat::Float,
        Some("integer") | Some("int") | None => PcmSampleFormat::Integer,
        Some(_) => {
            return Err(Error::Unsupported {
                operation: "pcm decode",
                reason: "unsupported PCM sample_format tag",
            });
        }
    };

    Ok(PcmEncoding::new(sample_format, bits_per_sample))
}

/// Attach PCM encoding tags to a stream.
pub fn set_pcm_tags(stream: &mut StreamInfo, encoding: PcmEncoding, block_align: u16) {
    stream.tags.insert(
        "sample_format".to_owned(),
        match encoding.sample_format {
            PcmSampleFormat::Integer => "integer",
            PcmSampleFormat::Float => "float",
        }
        .to_owned(),
    );
    stream.tags.insert(
        "bits_per_sample".to_owned(),
        encoding.bits_per_sample.to_string(),
    );
    stream
        .tags
        .insert("block_align".to_owned(), block_align.to_string());
}

/// Decode one raw PCM packet into an `AudioFrame`.
pub fn decode_pcm_packet(stream: &StreamInfo, packet: &EncodedPacket) -> Result<AudioFrame> {
    if packet.codec != CodecId::Pcm {
        return Err(Error::CodecMismatch {
            expected: CodecId::Pcm,
            actual: packet.codec.clone(),
        });
    }
    let encoding = pcm_encoding_from_stream(stream)?;
    let channels = stream.channels.ok_or(Error::InvalidAudioBuffer {
        reason: "PCM stream is missing channel count",
    })?;
    let sample_rate = stream.sample_rate.ok_or(Error::InvalidAudioBuffer {
        reason: "PCM stream is missing sample rate",
    })?;
    let samples = pcm_bytes_to_f32(&packet.data, encoding)?;
    AudioFrame::new(sample_rate, channels, packet.pts, samples)
}

/// Encode an `AudioFrame` into a raw PCM packet.
pub fn encode_pcm_packet(
    frame: &AudioFrame,
    track_id: u32,
    pts: i64,
    encoding: PcmEncoding,
) -> Result<EncodedPacket> {
    let time_base = TimeBase::new(1, frame.sample_rate as i32)?;
    let data = f32_to_pcm_bytes(&frame.samples_f32_interleaved, encoding)?;
    Ok(EncodedPacket::new(
        track_id,
        CodecId::Pcm,
        pts,
        frame.sample_frames() as i64,
        time_base,
        data,
    )
    .with_keyframe(true))
}

fn select_wav_stream(media: &MediaInfo) -> Result<&StreamInfo> {
    let audio = media.audio_streams().collect::<Vec<_>>();
    if audio.len() != 1 {
        return Err(Error::Unsupported {
            operation: "wav mux",
            reason: "WAV muxing requires exactly one audio stream",
        });
    }
    if media
        .streams
        .iter()
        .any(|stream| stream.media_type != MediaType::Audio)
    {
        return Err(Error::Unsupported {
            operation: "wav mux",
            reason: "WAV muxing cannot preserve non-audio streams",
        });
    }
    Ok(audio[0])
}

fn parse_wav(bytes: &Bytes) -> Result<ParsedWav> {
    if bytes.len() < RIFF_HEADER_LEN || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err(Error::Parse {
            format: "wav",
            message: "input is not a RIFF/WAVE object".to_owned(),
        });
    }

    let mut offset = RIFF_HEADER_LEN;
    let mut fmt = None::<FmtChunk>;
    let mut data = None::<Bytes>;
    while offset + 8 <= bytes.len() {
        let id = &bytes[offset..offset + 4];
        let size = read_u32_le(bytes, offset + 4)? as usize;
        offset += 8;
        if offset + size > bytes.len() {
            return Err(Error::Parse {
                format: "wav",
                message: "RIFF chunk length exceeds object length".to_owned(),
            });
        }
        let chunk = &bytes[offset..offset + size];
        match id {
            b"fmt " => fmt = Some(parse_fmt_chunk(chunk)?),
            b"data" => data = Some(Bytes::copy_from_slice(chunk)),
            _ => {}
        }
        offset += size + (size % 2);
    }

    let fmt = fmt.ok_or(Error::Parse {
        format: "wav",
        message: "missing fmt chunk".to_owned(),
    })?;
    let data = data.ok_or(Error::Parse {
        format: "wav",
        message: "missing data chunk".to_owned(),
    })?;
    let sample_format = match fmt.audio_format {
        FORMAT_PCM_INTEGER => PcmSampleFormat::Integer,
        FORMAT_IEEE_FLOAT => PcmSampleFormat::Float,
        _ => {
            return Err(Error::Unsupported {
                operation: "wav demux",
                reason: "only integer PCM and IEEE float WAV are supported",
            });
        }
    };
    let encoding = PcmEncoding::new(sample_format, fmt.bits_per_sample);
    validate_pcm_encoding(encoding)?;

    Ok(ParsedWav {
        sample_rate: fmt.sample_rate,
        channels: fmt.channels,
        block_align: fmt.block_align,
        encoding,
        data,
    })
}

fn parse_fmt_chunk(chunk: &[u8]) -> Result<FmtChunk> {
    if chunk.len() < 16 {
        return Err(Error::Parse {
            format: "wav",
            message: "fmt chunk is too short".to_owned(),
        });
    }
    Ok(FmtChunk {
        audio_format: read_u16_le_slice(chunk, 0)?,
        channels: read_u16_le_slice(chunk, 2)?,
        sample_rate: read_u32_le_slice(chunk, 4)?,
        block_align: read_u16_le_slice(chunk, 12)?,
        bits_per_sample: read_u16_le_slice(chunk, 14)?,
    })
}

fn write_wav_bytes(
    sample_rate: u32,
    channels: u16,
    encoding: PcmEncoding,
    block_align: u16,
    data: &[u8],
) -> Result<Bytes> {
    let fmt_len = 16_u32;
    let data_len = u32::try_from(data.len()).map_err(|_| Error::Unsupported {
        operation: "wav mux",
        reason: "WAV data is too large for RIFF",
    })?;
    let riff_len = 4_u32
        .checked_add(8 + fmt_len)
        .and_then(|value| value.checked_add(8 + data_len + (data_len % 2)))
        .ok_or(Error::Unsupported {
            operation: "wav mux",
            reason: "WAV object is too large for RIFF",
        })?;

    let byte_rate = sample_rate
        .checked_mul(u32::from(block_align))
        .ok_or(Error::Unsupported {
            operation: "wav mux",
            reason: "WAV byte rate is too large",
        })?;

    let mut out = BytesMut::with_capacity(riff_len as usize + 8);
    out.extend_from_slice(b"RIFF");
    out.put_u32_le(riff_len);
    out.extend_from_slice(b"WAVE");
    out.extend_from_slice(b"fmt ");
    out.put_u32_le(fmt_len);
    out.put_u16_le(encoding.format_tag());
    out.put_u16_le(channels);
    out.put_u32_le(sample_rate);
    out.put_u32_le(byte_rate);
    out.put_u16_le(block_align);
    out.put_u16_le(encoding.bits_per_sample);
    out.extend_from_slice(b"data");
    out.put_u32_le(data_len);
    out.extend_from_slice(data);
    if data_len % 2 == 1 {
        out.put_u8(0);
    }

    Ok(out.freeze())
}

fn pcm_bytes_to_f32(data: &[u8], encoding: PcmEncoding) -> Result<Vec<f32>> {
    validate_pcm_encoding(encoding)?;
    match (encoding.sample_format, encoding.bits_per_sample) {
        (PcmSampleFormat::Integer, 8) => Ok(data
            .iter()
            .map(|value| (*value as i16 - 128) as f32 / 128.0)
            .collect()),
        (PcmSampleFormat::Integer, 16) => chunks_exact(data, 2, "16-bit PCM").map(|chunks| {
            chunks
                .map(|chunk| i16::from_le_bytes([chunk[0], chunk[1]]) as f32 / i16::MAX as f32)
                .collect()
        }),
        (PcmSampleFormat::Integer, 24) => chunks_exact(data, 3, "24-bit PCM").map(|chunks| {
            chunks
                .map(|chunk| {
                    let raw = i32::from_le_bytes([
                        chunk[0],
                        chunk[1],
                        chunk[2],
                        if chunk[2] & 0x80 == 0 { 0x00 } else { 0xff },
                    ]);
                    (raw as f32 / signed_max_for_bits(24)).clamp(-1.0, 1.0)
                })
                .collect()
        }),
        (PcmSampleFormat::Integer, 32) => chunks_exact(data, 4, "32-bit PCM").map(|chunks| {
            chunks
                .map(|chunk| {
                    i32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]) as f32
                        / i32::MAX as f32
                })
                .collect()
        }),
        (PcmSampleFormat::Float, 32) => chunks_exact(data, 4, "32-bit float PCM").map(|chunks| {
            chunks
                .map(|chunk| {
                    f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]).clamp(-1.0, 1.0)
                })
                .collect()
        }),
        _ => Err(Error::Unsupported {
            operation: "pcm decode",
            reason: "unsupported PCM encoding",
        }),
    }
}

fn f32_to_pcm_bytes(samples: &[f32], encoding: PcmEncoding) -> Result<Bytes> {
    validate_pcm_encoding(encoding)?;
    let mut out = BytesMut::new();
    match (encoding.sample_format, encoding.bits_per_sample) {
        (PcmSampleFormat::Integer, 8) => {
            for sample in samples {
                let unsigned = ((*sample).clamp(-1.0, 1.0) * 127.0 + 128.0).round();
                out.put_u8((unsigned as i16).clamp(0, u8::MAX as i16) as u8);
            }
        }
        (PcmSampleFormat::Integer, 16) => {
            for sample in samples {
                let value = integer_sample(*sample, i16::MAX as f32);
                out.extend_from_slice(&(value as i16).to_le_bytes());
            }
        }
        (PcmSampleFormat::Integer, 24) => {
            for sample in samples {
                let value = integer_sample(*sample, signed_max_for_bits(24));
                let bytes = (value as i32).to_le_bytes();
                out.extend_from_slice(&bytes[..3]);
            }
        }
        (PcmSampleFormat::Integer, 32) => {
            for sample in samples {
                let value = integer_sample(*sample, i32::MAX as f32);
                out.extend_from_slice(&(value as i32).to_le_bytes());
            }
        }
        (PcmSampleFormat::Float, 32) => {
            for sample in samples {
                out.extend_from_slice(&sample.clamp(-1.0, 1.0).to_le_bytes());
            }
        }
        _ => {
            return Err(Error::Unsupported {
                operation: "pcm encode",
                reason: "unsupported PCM encoding",
            });
        }
    }
    Ok(out.freeze())
}

fn validate_pcm_encoding(encoding: PcmEncoding) -> Result<()> {
    match (encoding.sample_format, encoding.bits_per_sample) {
        (PcmSampleFormat::Integer, 8 | 16 | 24 | 32) | (PcmSampleFormat::Float, 32) => Ok(()),
        (PcmSampleFormat::Float, _) => Err(Error::Unsupported {
            operation: "pcm encode",
            reason: "float PCM currently requires 32 bits per sample",
        }),
        (PcmSampleFormat::Integer, _) => Err(Error::Unsupported {
            operation: "pcm encode",
            reason: "integer PCM currently supports 8, 16, 24, or 32 bits per sample",
        }),
    }
}

fn bytes_per_sample(encoding: PcmEncoding) -> Result<u16> {
    validate_pcm_encoding(encoding)?;
    Ok(encoding.bits_per_sample / 8)
}

fn chunks_exact<'a>(
    data: &'a [u8],
    width: usize,
    label: &'static str,
) -> Result<std::slice::ChunksExact<'a, u8>> {
    if !data.len().is_multiple_of(width) {
        return Err(Error::InvalidAudioBuffer {
            reason: match label {
                "16-bit PCM" => "16-bit PCM data has a trailing partial sample",
                "24-bit PCM" => "24-bit PCM data has a trailing partial sample",
                "32-bit PCM" => "32-bit PCM data has a trailing partial sample",
                "32-bit float PCM" => "32-bit float PCM data has a trailing partial sample",
                _ => "PCM data has a trailing partial sample",
            },
        });
    }
    Ok(data.chunks_exact(width))
}

fn integer_sample(sample: f32, max: f32) -> i64 {
    let sample = sample.clamp(-1.0, 1.0);
    if sample <= -1.0 {
        -(max as i64) - 1
    } else {
        (sample * max).round() as i64
    }
}

fn signed_max_for_bits(bits_per_sample: u16) -> f32 {
    let bits = bits_per_sample.clamp(2, 32) - 1;
    ((1_i64 << bits) - 1) as f32
}

fn read_u32_le(bytes: &Bytes, offset: usize) -> Result<u32> {
    bytes
        .get(offset..offset + 4)
        .map(|bytes| u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
        .ok_or_else(|| Error::Parse {
            format: "wav",
            message: "unexpected end of WAV object".to_owned(),
        })
}

fn read_u16_le_slice(bytes: &[u8], offset: usize) -> Result<u16> {
    bytes
        .get(offset..offset + 2)
        .map(|bytes| u16::from_le_bytes([bytes[0], bytes[1]]))
        .ok_or_else(|| Error::Parse {
            format: "wav",
            message: "unexpected end of fmt chunk".to_owned(),
        })
}

fn read_u32_le_slice(bytes: &[u8], offset: usize) -> Result<u32> {
    bytes
        .get(offset..offset + 4)
        .map(|bytes| u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
        .ok_or_else(|| Error::Parse {
            format: "wav",
            message: "unexpected end of fmt chunk".to_owned(),
        })
}

#[derive(Clone, Debug)]
struct ParsedWav {
    sample_rate: u32,
    channels: u16,
    block_align: u16,
    encoding: PcmEncoding,
    data: Bytes,
}

#[derive(Clone, Copy, Debug)]
struct FmtChunk {
    audio_format: u16,
    channels: u16,
    sample_rate: u32,
    block_align: u16,
    bits_per_sample: u16,
}

#[cfg(test)]
mod tests {
    use super::{
        PcmEncoding, PcmSampleFormat, decode_pcm_packet, demux_wav_bytes, encode_pcm_packet,
        mux_wav_bytes,
    };
    use crate::{AudioFrame, CodecId, MediaInfo, MediaType, StreamInfo, TimeBase};
    use bytes::Bytes;

    #[test]
    fn wav_demux_mux_round_trips_pcm_packets() {
        let frame = AudioFrame::new(48_000, 2, 0, vec![0.0, 0.5, -0.5, 1.0]).unwrap();
        let packet = encode_pcm_packet(&frame, 1, 0, PcmEncoding::signed_16()).unwrap();
        let mut stream = StreamInfo::new(
            1,
            MediaType::Audio,
            CodecId::Pcm,
            TimeBase::new(1, 48_000).unwrap(),
        )
        .with_audio_format(48_000, 2);
        super::set_pcm_tags(&mut stream, PcmEncoding::signed_16(), 4);
        let mut media = MediaInfo::default();
        media.push_stream(stream);

        let bytes = mux_wav_bytes(&media, &[packet]).unwrap();
        assert_eq!(&bytes[..4], b"RIFF");
        let demuxed = demux_wav_bytes(&bytes).unwrap();
        let decoded = decode_pcm_packet(&demuxed.media.streams[0], &demuxed.packets[0]).unwrap();

        assert_eq!(demuxed.media.streams[0].codec, CodecId::Pcm);
        assert_eq!(decoded.sample_rate, 48_000);
        assert_eq!(decoded.channels, 2);
        assert!((decoded.samples_f32_interleaved[1] - 0.5).abs() < 0.001);
    }

    #[test]
    fn wav_float32_encoding_round_trips() {
        let frame = AudioFrame::new(44_100, 1, 0, vec![-0.25, 0.25]).unwrap();
        let packet =
            encode_pcm_packet(&frame, 1, 0, PcmEncoding::new(PcmSampleFormat::Float, 32)).unwrap();
        let decoded_stream = StreamInfo::new(
            1,
            MediaType::Audio,
            CodecId::Pcm,
            TimeBase::new(1, 44_100).unwrap(),
        )
        .with_audio_format(44_100, 1);
        let mut tagged = decoded_stream;
        super::set_pcm_tags(&mut tagged, PcmEncoding::float_32(), 4);

        let decoded = decode_pcm_packet(&tagged, &packet).unwrap();

        assert_eq!(
            Bytes::copy_from_slice(&packet.data),
            Bytes::from(packet.data.to_vec())
        );
        assert_eq!(decoded.samples_f32_interleaved, vec![-0.25, 0.25]);
    }
}
