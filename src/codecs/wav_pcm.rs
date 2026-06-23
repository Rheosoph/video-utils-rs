use crate::{
    audio::AudioFrame,
    codec::{CodecDescriptor, CodecId, Decoder, Encoder},
    error::{Error, Result},
};
use std::io::Cursor;

/// WAV PCM decoder backed by `hound`.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct WavPcmDecoder;

impl WavPcmDecoder {
    /// Create a WAV PCM decoder.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl CodecDescriptor for WavPcmDecoder {
    fn name(&self) -> &'static str {
        "wav-pcm/decoder"
    }

    fn codec_id(&self) -> CodecId {
        CodecId::Pcm
    }
}

impl Decoder for WavPcmDecoder {
    type Input = [u8];
    type Output = AudioFrame;

    fn decode(&mut self, input: &Self::Input) -> Result<Self::Output> {
        let reader = hound::WavReader::new(Cursor::new(input.to_vec())).map_err(wav_error)?;
        let spec = reader.spec();
        let channels = spec.channels;
        let sample_rate = spec.sample_rate;

        let samples = match spec.sample_format {
            hound::SampleFormat::Float => reader
                .into_samples::<f32>()
                .map(|sample| sample.map(|value| value.clamp(-1.0, 1.0)))
                .collect::<std::result::Result<Vec<_>, _>>()
                .map_err(wav_error)?,
            hound::SampleFormat::Int if spec.bits_per_sample == 8 => reader
                .into_samples::<i8>()
                .map(|sample| sample.map(|value| value as f32 / i8::MAX as f32))
                .collect::<std::result::Result<Vec<_>, _>>()
                .map_err(wav_error)?,
            hound::SampleFormat::Int if spec.bits_per_sample <= 16 => reader
                .into_samples::<i16>()
                .map(|sample| sample.map(|value| value as f32 / i16::MAX as f32))
                .collect::<std::result::Result<Vec<_>, _>>()
                .map_err(wav_error)?,
            hound::SampleFormat::Int => {
                let denom = signed_max_for_bits(spec.bits_per_sample);
                reader
                    .into_samples::<i32>()
                    .map(|sample| sample.map(|value| (value as f32 / denom).clamp(-1.0, 1.0)))
                    .collect::<std::result::Result<Vec<_>, _>>()
                    .map_err(wav_error)?
            }
        };

        AudioFrame::new(sample_rate, channels, 0, samples)
    }
}

/// WAV PCM encoder backed by `hound`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WavPcmEncoder {
    sample_format: WavPcmSampleFormat,
    bits_per_sample: u16,
}

/// WAV sample encoding written by `WavPcmEncoder`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WavPcmSampleFormat {
    /// Signed integer PCM.
    Integer,
    /// 32-bit IEEE float PCM.
    Float,
}

impl Default for WavPcmEncoder {
    fn default() -> Self {
        Self::new()
    }
}

impl WavPcmEncoder {
    /// Create a signed 16-bit PCM WAV encoder.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            sample_format: WavPcmSampleFormat::Integer,
            bits_per_sample: 16,
        }
    }

    /// Create an encoder with a specific integer PCM bit depth.
    #[must_use]
    pub const fn with_bits_per_sample(bits_per_sample: u16) -> Self {
        Self {
            sample_format: WavPcmSampleFormat::Integer,
            bits_per_sample,
        }
    }

    /// Create an encoder that writes 32-bit float WAV.
    #[must_use]
    pub const fn float32() -> Self {
        Self {
            sample_format: WavPcmSampleFormat::Float,
            bits_per_sample: 32,
        }
    }

    /// Sample format written by this encoder.
    #[must_use]
    pub const fn sample_format(self) -> WavPcmSampleFormat {
        self.sample_format
    }

    /// Bits per sample written by this encoder.
    #[must_use]
    pub const fn bits_per_sample(self) -> u16 {
        self.bits_per_sample
    }
}

impl CodecDescriptor for WavPcmEncoder {
    fn name(&self) -> &'static str {
        "wav-pcm/encoder"
    }

    fn codec_id(&self) -> CodecId {
        CodecId::Pcm
    }
}

impl Encoder for WavPcmEncoder {
    type Input = AudioFrame;
    type Output = Vec<u8>;

    fn encode(&mut self, input: &Self::Input) -> Result<Self::Output> {
        validate_encode_format(self.sample_format, self.bits_per_sample)?;

        let spec = hound::WavSpec {
            channels: input.channels,
            sample_rate: input.sample_rate,
            bits_per_sample: self.bits_per_sample,
            sample_format: match self.sample_format {
                WavPcmSampleFormat::Integer => hound::SampleFormat::Int,
                WavPcmSampleFormat::Float => hound::SampleFormat::Float,
            },
        };

        let mut cursor = Cursor::new(Vec::new());
        {
            let mut writer = hound::WavWriter::new(&mut cursor, spec).map_err(wav_error)?;
            match self.sample_format {
                WavPcmSampleFormat::Integer => match self.bits_per_sample {
                    8 => write_integer_samples::<i8>(&mut writer, input, i8::MAX as f32)?,
                    16 => write_integer_samples::<i16>(&mut writer, input, i16::MAX as f32)?,
                    24 => {
                        write_integer_samples::<i32>(&mut writer, input, signed_max_for_bits(24))?
                    }
                    32 => {
                        write_integer_samples::<i32>(&mut writer, input, signed_max_for_bits(32))?
                    }
                    _ => unreachable!("validate_encode_format should reject invalid bit depths"),
                },
                WavPcmSampleFormat::Float => {
                    for sample in &input.samples_f32_interleaved {
                        writer
                            .write_sample(sample.clamp(-1.0, 1.0))
                            .map_err(wav_error)?;
                    }
                }
            }
            writer.finalize().map_err(wav_error)?;
        }

        Ok(cursor.into_inner())
    }
}

trait IntegerWavSample: hound::Sample {
    fn from_i64(value: i64) -> Self;
}

impl IntegerWavSample for i8 {
    fn from_i64(value: i64) -> Self {
        value.clamp(i8::MIN as i64, i8::MAX as i64) as Self
    }
}

impl IntegerWavSample for i16 {
    fn from_i64(value: i64) -> Self {
        value.clamp(i16::MIN as i64, i16::MAX as i64) as Self
    }
}

impl IntegerWavSample for i32 {
    fn from_i64(value: i64) -> Self {
        value.clamp(i32::MIN as i64, i32::MAX as i64) as Self
    }
}

fn write_integer_samples<S>(
    writer: &mut hound::WavWriter<&mut Cursor<Vec<u8>>>,
    input: &AudioFrame,
    max: f32,
) -> Result<()>
where
    S: IntegerWavSample,
{
    for sample in &input.samples_f32_interleaved {
        let sample = (*sample).clamp(-1.0, 1.0);
        let sample = if sample <= -1.0 {
            -(max as i64) - 1
        } else {
            (sample * max).round() as i64
        };
        writer
            .write_sample(S::from_i64(sample))
            .map_err(wav_error)?;
    }
    Ok(())
}

fn validate_encode_format(sample_format: WavPcmSampleFormat, bits_per_sample: u16) -> Result<()> {
    match (sample_format, bits_per_sample) {
        (WavPcmSampleFormat::Integer, 8 | 16 | 24 | 32) | (WavPcmSampleFormat::Float, 32) => Ok(()),
        (WavPcmSampleFormat::Float, _) => Err(Error::Unsupported {
            operation: "wav encode",
            reason: "float WAV output requires 32 bits per sample",
        }),
        (WavPcmSampleFormat::Integer, _) => Err(Error::Unsupported {
            operation: "wav encode",
            reason: "integer WAV output supports 8, 16, 24, or 32 bits per sample",
        }),
    }
}

fn signed_max_for_bits(bits_per_sample: u16) -> f32 {
    let bits = bits_per_sample.clamp(2, 32) - 1;
    ((1_i64 << bits) - 1) as f32
}

fn wav_error(err: hound::Error) -> Error {
    Error::Parse {
        format: "wav",
        message: err.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::{WavPcmDecoder, WavPcmEncoder};
    use crate::{AudioFrame, Decoder, Encoder};

    #[test]
    fn wav_pcm_round_trips_audio_frames() {
        let frame = AudioFrame::new(48_000, 2, 0, vec![0.0, 0.25, -0.5, 0.75]).unwrap();
        let mut encoder = WavPcmEncoder::new();
        let mut decoder = WavPcmDecoder::new();

        let bytes = encoder.encode(&frame).unwrap();
        let decoded = decoder.decode(&bytes).unwrap();

        assert_eq!(decoded.sample_rate, 48_000);
        assert_eq!(decoded.channels, 2);
        assert_eq!(decoded.sample_frames(), 2);
        assert!((decoded.samples_f32_interleaved[1] - 0.25).abs() < 0.001);
    }

    #[test]
    fn wav_pcm_writes_other_supported_depths() {
        let frame = AudioFrame::new(48_000, 1, 0, vec![-1.0, -0.25, 0.25, 1.0]).unwrap();

        for mut encoder in [
            WavPcmEncoder::with_bits_per_sample(8),
            WavPcmEncoder::with_bits_per_sample(24),
            WavPcmEncoder::with_bits_per_sample(32),
            WavPcmEncoder::float32(),
        ] {
            let bytes = encoder.encode(&frame).unwrap();
            let decoded = WavPcmDecoder::new().decode(&bytes).unwrap();

            assert_eq!(decoded.sample_rate, 48_000);
            assert_eq!(decoded.channels, 1);
            assert_eq!(decoded.sample_frames(), 4);
        }
    }
}
