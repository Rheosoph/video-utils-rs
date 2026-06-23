use crate::{
    audio::AudioFrame,
    bitstream::aac::aac_packet_to_raw,
    codec::{AudioDecoder, CodecDescriptor, CodecId, Decoder},
    error::{Error, Result},
    packet::EncodedPacket,
    time::TimeBase,
};
use bytes::Bytes;
use std::fmt;
use std::io::Cursor;
use symphonia::core::{
    audio::{Channels, SampleBuffer},
    codecs::{
        CODEC_TYPE_AAC, CODEC_TYPE_FLAC, CODEC_TYPE_MP3, CODEC_TYPE_OPUS, CODEC_TYPE_VORBIS,
        CodecParameters, CodecType, DecoderOptions,
    },
    errors::Error as SymphoniaError,
    formats::{FormatOptions, Packet as SymphoniaPacket},
    io::MediaSourceStream,
    meta::MetadataOptions,
    probe::Hint,
    units::TimeBase as SymphoniaTimeBase,
};

/// Audio-file decoder backed by Symphonia.
///
/// This adapter decodes an in-memory media file into one or more interleaved
/// `AudioFrame` values. The selected track is Symphonia's default track.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SymphoniaAudioDecoder {
    codec: Option<CodecId>,
    hint_extension: Option<String>,
}

/// Packet-level compressed audio decoder backed by Symphonia.
///
/// This adapter decodes one encoded audio access unit at a time through the
/// crate's `AudioDecoder` trait. It is intended for packets demuxed by the
/// container adapters where stream metadata and optional codec-private bytes
/// are already known.
pub struct SymphoniaPacketAudioDecoder {
    codec: CodecId,
    sample_rate: u32,
    channels: u16,
    codec_config: Option<Bytes>,
    decoder: Option<Box<dyn symphonia::core::codecs::Decoder>>,
}

impl SymphoniaPacketAudioDecoder {
    /// Create a packet decoder without codec-private data.
    pub fn new(codec: CodecId, sample_rate: u32, channels: u16) -> Result<Self> {
        Self::with_codec_config(codec, sample_rate, channels, None)
    }

    /// Create a packet decoder with optional codec-private data such as AAC
    /// AudioSpecificConfig or Vorbis/Opus/FLAC headers supplied by a container.
    pub fn with_codec_config(
        codec: CodecId,
        sample_rate: u32,
        channels: u16,
        codec_config: Option<Bytes>,
    ) -> Result<Self> {
        if sample_rate == 0 {
            return Err(Error::InvalidAudioBuffer {
                reason: "sample rate must be non-zero",
            });
        }
        if channels == 0 {
            return Err(Error::InvalidAudioBuffer {
                reason: "channel count must be non-zero",
            });
        }
        let _ = symphonia_codec_type(&codec)?;
        let _ = symphonia_channels(channels)?;
        Ok(Self {
            codec,
            sample_rate,
            channels,
            codec_config,
            decoder: None,
        })
    }

    fn ensure_decoder(&mut self, packet_time_base: TimeBase) -> Result<()> {
        if self.decoder.is_some() {
            return Ok(());
        }

        let mut params = CodecParameters::new();
        params
            .for_codec(symphonia_codec_type(&self.codec)?)
            .with_sample_rate(self.sample_rate)
            .with_time_base(SymphoniaTimeBase::new(
                u32::try_from(packet_time_base.num).map_err(|_| Error::InvalidTimeBase {
                    num: packet_time_base.num,
                    den: packet_time_base.den,
                })?,
                u32::try_from(packet_time_base.den).map_err(|_| Error::InvalidTimeBase {
                    num: packet_time_base.num,
                    den: packet_time_base.den,
                })?,
            ))
            .with_channels(symphonia_channels(self.channels)?);
        if let Some(config) = &self.codec_config {
            params.extra_data = Some(config.to_vec().into_boxed_slice());
        }

        self.decoder = Some(
            symphonia::default::get_codecs()
                .make(&params, &DecoderOptions::default())
                .map_err(symphonia_error)?,
        );
        Ok(())
    }
}

impl fmt::Debug for SymphoniaPacketAudioDecoder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SymphoniaPacketAudioDecoder")
            .field("codec", &self.codec)
            .field("sample_rate", &self.sample_rate)
            .field("channels", &self.channels)
            .field("has_codec_config", &self.codec_config.is_some())
            .field("decoder_initialized", &self.decoder.is_some())
            .finish()
    }
}

impl CodecDescriptor for SymphoniaPacketAudioDecoder {
    fn name(&self) -> &'static str {
        "symphonia-audio-packet/decoder"
    }

    fn codec_id(&self) -> CodecId {
        self.codec.clone()
    }
}

impl AudioDecoder for SymphoniaPacketAudioDecoder {
    fn decode_packet(&mut self, packet: &EncodedPacket) -> Result<Vec<AudioFrame>> {
        if packet.codec != self.codec {
            return Err(Error::CodecMismatch {
                expected: self.codec.clone(),
                actual: packet.codec.clone(),
            });
        }
        if packet.pts < 0 || packet.duration < 0 {
            return Err(Error::InvalidPacketTiming {
                reason: "negative audio packet timing cannot be decoded by Symphonia",
            });
        }

        self.ensure_decoder(packet.time_base)?;
        let payload = match packet.codec {
            CodecId::Aac => aac_packet_to_raw(packet)?,
            _ => packet.data.clone(),
        };
        let symphonia_packet = SymphoniaPacket::new_from_slice(
            packet.track_id,
            u64::try_from(packet.decode_order_ts()).map_err(|_| Error::InvalidPacketTiming {
                reason: "negative audio decode timestamp cannot be decoded by Symphonia",
            })?,
            u64::try_from(packet.duration).map_err(|_| Error::InvalidPacketTiming {
                reason: "negative audio packet duration cannot be decoded by Symphonia",
            })?,
            &payload,
        );
        let decoded = self
            .decoder
            .as_mut()
            .expect("decoder initialized above")
            .decode(&symphonia_packet)
            .map_err(symphonia_error)?;
        decoded_audio_to_frame(decoded, packet.pts, packet.time_base).map(|frame| vec![frame])
    }

    fn flush(&mut self) -> Result<Vec<AudioFrame>> {
        if let Some(decoder) = &mut self.decoder {
            decoder.finalize();
        }
        Ok(Vec::new())
    }
}

impl SymphoniaAudioDecoder {
    /// Create a decoder without a container/extension hint.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a decoder with a format hint such as `wav`, `mp3`, `m4a`, or `flac`.
    #[must_use]
    pub fn with_extension(extension: impl Into<String>) -> Self {
        Self {
            codec: None,
            hint_extension: Some(extension.into()),
        }
    }

    /// Create a decoder annotated for one expected audio codec.
    #[must_use]
    pub fn for_codec(codec: CodecId) -> Self {
        Self {
            codec: Some(codec),
            hint_extension: None,
        }
    }

    /// Create a decoder annotated for one codec and one container/extension hint.
    #[must_use]
    pub fn for_codec_with_extension(codec: CodecId, extension: impl Into<String>) -> Self {
        Self {
            codec: Some(codec),
            hint_extension: Some(extension.into()),
        }
    }
}

impl CodecDescriptor for SymphoniaAudioDecoder {
    fn name(&self) -> &'static str {
        "symphonia-audio/decoder"
    }

    fn codec_id(&self) -> CodecId {
        self.codec
            .clone()
            .unwrap_or_else(|| CodecId::Unknown("symphonia-audio".to_owned()))
    }
}

impl Decoder for SymphoniaAudioDecoder {
    type Input = [u8];
    type Output = Vec<AudioFrame>;

    fn decode(&mut self, input: &Self::Input) -> Result<Self::Output> {
        let media = Box::new(Cursor::new(input.to_vec()));
        let stream = MediaSourceStream::new(media, Default::default());

        let mut hint = Hint::new();
        if let Some(extension) = &self.hint_extension {
            hint.with_extension(extension);
        }

        let probed = symphonia::default::get_probe()
            .format(
                &hint,
                stream,
                &FormatOptions::default(),
                &MetadataOptions::default(),
            )
            .map_err(symphonia_error)?;

        let mut format = probed.format;
        let (track_id, codec_params) = {
            let track = format.default_track().ok_or(Error::Parse {
                format: "audio",
                message: "no default audio track found".to_owned(),
            })?;
            (track.id, track.codec_params.clone())
        };

        let mut decoder = symphonia::default::get_codecs()
            .make(&codec_params, &DecoderOptions::default())
            .map_err(symphonia_error)?;

        let mut frames = Vec::new();
        let mut next_pts = 0_i64;

        loop {
            let packet = match format.next_packet() {
                Ok(packet) => packet,
                Err(SymphoniaError::IoError(err))
                    if err.kind() == std::io::ErrorKind::UnexpectedEof =>
                {
                    break;
                }
                Err(SymphoniaError::ResetRequired) => {
                    return Err(Error::Unsupported {
                        operation: "audio decode",
                        reason: "stream reset is not implemented",
                    });
                }
                Err(err) => return Err(symphonia_error(err)),
            };

            if packet.track_id() != track_id {
                continue;
            }

            let decoded = match decoder.decode(&packet) {
                Ok(decoded) => decoded,
                Err(SymphoniaError::DecodeError(_)) => continue,
                Err(err) => return Err(symphonia_error(err)),
            };

            let frame = decoded_audio_to_frame(decoded, next_pts, TimeBase::new(1, 1).unwrap())?;
            if frame.samples_f32_interleaved.is_empty() {
                continue;
            }

            let frame_sample_count = frame.sample_frames();
            frames.push(frame);
            next_pts += frame_sample_count as i64;
        }

        Ok(frames)
    }
}

fn decoded_audio_to_frame(
    decoded: symphonia::core::audio::AudioBufferRef<'_>,
    pts: i64,
    input_time_base: TimeBase,
) -> Result<AudioFrame> {
    let spec = *decoded.spec();
    let sample_rate = spec.rate;
    let channels = spec.channels.count() as u16;
    let mut sample_buffer = SampleBuffer::<f32>::new(decoded.capacity() as u64, spec);
    sample_buffer.copy_interleaved_ref(decoded);
    let samples = sample_buffer.samples().to_vec();
    let pts = input_time_base.rescale(pts, TimeBase::new(1, sample_rate as i32)?);
    AudioFrame::new(sample_rate, channels, pts, samples)
}

fn symphonia_codec_type(codec: &CodecId) -> Result<CodecType> {
    match codec {
        CodecId::Aac => Ok(CODEC_TYPE_AAC),
        CodecId::Flac => Ok(CODEC_TYPE_FLAC),
        CodecId::Mp3 => Ok(CODEC_TYPE_MP3),
        CodecId::Opus => Ok(CODEC_TYPE_OPUS),
        CodecId::Vorbis => Ok(CODEC_TYPE_VORBIS),
        _ => Err(Error::Unsupported {
            operation: "audio packet decode",
            reason: "codec is not supported by SymphoniaPacketAudioDecoder",
        }),
    }
}

fn symphonia_channels(channels: u16) -> Result<Channels> {
    match channels {
        1 => Ok(Channels::FRONT_LEFT),
        2 => Ok(Channels::FRONT_LEFT | Channels::FRONT_RIGHT),
        3 => Ok(Channels::FRONT_LEFT | Channels::FRONT_RIGHT | Channels::FRONT_CENTRE),
        4 => Ok(Channels::FRONT_LEFT
            | Channels::FRONT_RIGHT
            | Channels::REAR_LEFT
            | Channels::REAR_RIGHT),
        5 => Ok(Channels::FRONT_LEFT
            | Channels::FRONT_RIGHT
            | Channels::FRONT_CENTRE
            | Channels::REAR_LEFT
            | Channels::REAR_RIGHT),
        6 => Ok(Channels::FRONT_LEFT
            | Channels::FRONT_RIGHT
            | Channels::FRONT_CENTRE
            | Channels::LFE1
            | Channels::REAR_LEFT
            | Channels::REAR_RIGHT),
        7 => Ok(Channels::FRONT_LEFT
            | Channels::FRONT_RIGHT
            | Channels::FRONT_CENTRE
            | Channels::LFE1
            | Channels::REAR_LEFT
            | Channels::REAR_RIGHT
            | Channels::REAR_CENTRE),
        8 => Ok(Channels::FRONT_LEFT
            | Channels::FRONT_RIGHT
            | Channels::FRONT_CENTRE
            | Channels::LFE1
            | Channels::REAR_LEFT
            | Channels::REAR_RIGHT
            | Channels::SIDE_LEFT
            | Channels::SIDE_RIGHT),
        _ => Err(Error::Unsupported {
            operation: "audio packet decode",
            reason: "channel layouts above 8 channels are not mapped",
        }),
    }
}

fn symphonia_error(err: SymphoniaError) -> Error {
    Error::Parse {
        format: "audio",
        message: err.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::{SymphoniaAudioDecoder, SymphoniaPacketAudioDecoder};
    use crate::{
        AudioDecoder, AudioFrame, CodecDescriptor, CodecId, Decoder, EncodedPacket, Encoder, Error,
        TimeBase, WavPcmEncoder,
    };

    #[test]
    fn decodes_wav_bytes() {
        let frame = AudioFrame::new(44_100, 1, 0, vec![0.0, 0.5, -0.5, 0.25]).unwrap();
        let mut wav = WavPcmEncoder::new();
        let bytes = wav.encode(&frame).unwrap();
        let mut decoder = SymphoniaAudioDecoder::for_codec_with_extension(CodecId::Pcm, "wav");

        let frames = decoder.decode(&bytes).unwrap();

        assert_eq!(decoder.codec_id(), CodecId::Pcm);
        let decoded_samples: usize = frames.iter().map(|frame| frame.sample_frames()).sum();
        assert_eq!(frames[0].sample_rate, 44_100);
        assert_eq!(frames[0].channels, 1);
        assert_eq!(decoded_samples, 4);
    }

    #[test]
    fn packet_decoder_reports_codec_mismatch() {
        let mut decoder = SymphoniaPacketAudioDecoder::new(CodecId::Mp3, 44_100, 2).unwrap();
        let packet = EncodedPacket::new(
            1,
            CodecId::Aac,
            0,
            1024,
            TimeBase::new(1, 48_000).unwrap(),
            vec![0u8; 8],
        );

        let err = decoder.decode_packet(&packet).unwrap_err();

        assert!(matches!(
            err,
            Error::CodecMismatch {
                expected: CodecId::Mp3,
                actual: CodecId::Aac
            }
        ));
    }

    #[test]
    fn packet_decoder_rejects_unmapped_codec() {
        let err = SymphoniaPacketAudioDecoder::new(CodecId::Ac3, 48_000, 2).unwrap_err();

        assert!(matches!(err, Error::Unsupported { .. }));
    }
}
