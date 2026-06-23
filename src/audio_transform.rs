use crate::{
    audio::{AudioFrame, FadeShape, apply_gain, apply_gain_db, fade, normalize_peak},
    container::ContainerFormat,
    error::Result,
};
use object_store::path::Path;

/// One decoded-audio transformation step.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum AudioTransform {
    /// Apply a linear gain factor.
    Gain { factor: f32 },
    /// Apply gain in decibels.
    GainDb { db: f32 },
    /// Peak-normalize to a target amplitude in `0.0..=1.0`.
    NormalizePeak { target_peak: f32 },
    /// Apply fade-in and fade-out envelopes.
    Fade {
        /// Fade-in length in sample frames.
        fade_in_samples: usize,
        /// Fade-out length in sample frames.
        fade_out_samples: usize,
        /// Fade curve shape.
        shape: FadeShape,
    },
}

impl AudioTransform {
    /// Apply this transform to a decoded audio frame.
    pub fn apply(self, frame: &mut AudioFrame) -> Result<()> {
        match self {
            Self::Gain { factor } => apply_gain(frame, factor),
            Self::GainDb { db } => apply_gain_db(frame, db),
            Self::NormalizePeak { target_peak } => normalize_peak(frame, target_peak)?,
            Self::Fade {
                fade_in_samples,
                fade_out_samples,
                shape,
            } => fade(frame, fade_in_samples, fade_out_samples, shape),
        }
        Ok(())
    }
}

/// Ordered decoded-audio transformation pipeline.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AudioTransformPipeline {
    /// Transform steps applied in order.
    pub steps: Vec<AudioTransform>,
}

impl AudioTransformPipeline {
    /// Create an empty pipeline.
    #[must_use]
    pub const fn new() -> Self {
        Self { steps: Vec::new() }
    }

    /// Append a transform step.
    pub fn push(&mut self, transform: AudioTransform) {
        self.steps.push(transform);
    }

    /// Return a pipeline with an additional transform step.
    #[must_use]
    pub fn with(mut self, transform: AudioTransform) -> Self {
        self.push(transform);
        self
    }

    /// Apply all steps to one decoded audio frame.
    pub fn apply(&self, frame: &AudioFrame) -> Result<AudioFrame> {
        let mut current = frame.clone();
        for step in &self.steps {
            step.apply(&mut current)?;
        }
        Ok(current)
    }

    /// Apply all steps to a sequence of frames.
    pub fn apply_all<'a>(
        &self,
        frames: impl IntoIterator<Item = &'a AudioFrame>,
    ) -> Result<Vec<AudioFrame>> {
        frames.into_iter().map(|frame| self.apply(frame)).collect()
    }
}

/// Options for object-store decoded-audio transformation.
#[derive(Clone, Debug, PartialEq)]
pub struct ObjectAudioTransformJob {
    /// Ordered decoded-audio transforms.
    pub pipeline: AudioTransformPipeline,
    /// Specific audio track to transform. The first audio track is used when unset.
    pub audio_track_id: Option<u32>,
    /// Output PCM encoding.
    #[cfg(feature = "containers")]
    pub output_encoding: crate::containers::PcmEncoding,
}

impl ObjectAudioTransformJob {
    /// Create an audio transform job with signed 16-bit PCM output.
    #[must_use]
    pub fn new(pipeline: AudioTransformPipeline) -> Self {
        Self {
            pipeline,
            audio_track_id: None,
            #[cfg(feature = "containers")]
            output_encoding: crate::containers::PcmEncoding::signed_16(),
        }
    }

    /// Select a specific audio track.
    #[must_use]
    pub const fn with_audio_track(mut self, track_id: u32) -> Self {
        self.audio_track_id = Some(track_id);
        self
    }

    /// Select output PCM encoding.
    #[cfg(feature = "containers")]
    #[must_use]
    pub const fn with_output_encoding(mut self, encoding: crate::containers::PcmEncoding) -> Self {
        self.output_encoding = encoding;
        self
    }
}

/// Report returned by object-store audio transformation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ObjectAudioTransformReport {
    /// Source object key.
    pub source: Path,
    /// Target object key.
    pub target: Path,
    /// Source container format.
    pub source_format: ContainerFormat,
    /// Target container format.
    pub target_format: ContainerFormat,
    /// Transformed audio track id.
    pub audio_track_id: u32,
    /// Encoded input packets read from the source object.
    pub input_packets: usize,
    /// Decoded audio frames that were transformed.
    pub decoded_frames: usize,
    /// Encoded audio packets produced for output.
    pub encoded_audio_packets: usize,
    /// Bytes written to the target object.
    pub bytes_written: u64,
}

#[cfg(feature = "containers")]
mod object_store_impl {
    use super::{ObjectAudioTransformJob, ObjectAudioTransformReport};
    #[cfg(feature = "audio-io")]
    use crate::{
        codec::Decoder, codecs::SymphoniaAudioDecoder, object_store_io::read_object_bytes,
    };
    use crate::{
        codec::{CodecId, MediaType},
        container::ContainerFormat,
        containers,
        error::{Error, Result},
        media::{MediaInfo, StreamInfo},
        object_store_io::{demux_object, detect_object_container_format, write_object_bytes},
        packet::EncodedPacket,
        time::TimeBase,
    };
    use object_store::{ObjectStore, path::Path};

    /// Transform PCM audio frames from one object into another object in the same store.
    pub async fn transform_object_audio_same_store(
        store: &dyn ObjectStore,
        source: &Path,
        target: &Path,
        job: &ObjectAudioTransformJob,
    ) -> Result<ObjectAudioTransformReport> {
        transform_object_audio_between_stores(store, source, store, target, job).await
    }

    /// Transform PCM audio frames between object-store objects.
    pub async fn transform_object_audio_between_stores(
        source_store: &dyn ObjectStore,
        source: &Path,
        target_store: &dyn ObjectStore,
        target: &Path,
        job: &ObjectAudioTransformJob,
    ) -> Result<ObjectAudioTransformReport> {
        let source_format = detect_object_container_format(source_store, source).await?;
        let target_format = target_format_from_path(target)?;
        if target_format != ContainerFormat::Wav {
            return Err(Error::Unsupported {
                operation: "audio transform",
                reason: "object audio transforms currently write WAV PCM output",
            });
        }

        let demuxed = demux_object(source_store, source).await?;
        let audio_track_id = select_audio_track(&demuxed.media.streams, job.audio_track_id)?;
        let input_stream =
            demuxed
                .media
                .stream(audio_track_id)
                .ok_or(Error::IncompatibleTrack {
                    track_id: audio_track_id,
                    reason: "selected audio track is missing from source media",
                })?;
        if input_stream.codec != CodecId::Pcm {
            return Err(Error::Unsupported {
                operation: "audio transform",
                reason: "object audio transforms currently require decoded PCM packet input",
            });
        }

        let input_packets = demuxed
            .packets
            .iter()
            .filter(|packet| packet.track_id == audio_track_id)
            .collect::<Vec<_>>();
        if input_packets.is_empty() {
            return Err(Error::EmptyInput);
        }

        let mut output_packets = Vec::<EncodedPacket>::new();
        let mut output_media = MediaInfo::default();
        let mut decoded_frames = 0_usize;
        let mut output_pts = 0_i64;
        let sample_rate = input_stream.sample_rate.ok_or(Error::InvalidAudioBuffer {
            reason: "PCM stream is missing sample rate",
        })?;
        let channels = input_stream.channels.ok_or(Error::InvalidAudioBuffer {
            reason: "PCM stream is missing channel count",
        })?;
        let time_base = TimeBase::new(1, sample_rate as i32)?;

        for packet in input_packets {
            let frame = containers::decode_pcm_packet(input_stream, packet)?;
            let transformed = job.pipeline.apply(&frame)?;
            decoded_frames += 1;
            let encoded = containers::encode_pcm_packet(
                &transformed,
                audio_track_id,
                output_pts,
                job.output_encoding,
            )?;
            output_pts += transformed.sample_frames() as i64;
            output_packets.push(encoded);
        }

        let mut output_stream =
            StreamInfo::new(audio_track_id, MediaType::Audio, CodecId::Pcm, time_base)
                .with_audio_format(sample_rate, channels);
        output_stream.duration = Some(output_pts);
        let block_align = channels
            .checked_mul(job.output_encoding.bits_per_sample / 8)
            .ok_or(Error::Unsupported {
                operation: "audio transform",
                reason: "output PCM block alignment is too large",
            })?;
        containers::set_pcm_tags(&mut output_stream, job.output_encoding, block_align);
        output_media.duration_seconds = Some(output_pts as f64 / sample_rate as f64);
        output_media.push_stream(output_stream);

        let bytes = containers::mux_wav_bytes(&output_media, &output_packets)?;
        let bytes_written = bytes.len() as u64;
        write_object_bytes(target_store, target, bytes).await?;

        Ok(ObjectAudioTransformReport {
            source: source.clone(),
            target: target.clone(),
            source_format,
            target_format,
            audio_track_id,
            input_packets: demuxed.packets.len(),
            decoded_frames,
            encoded_audio_packets: output_packets.len(),
            bytes_written,
        })
    }

    /// Decode an audio file object with Symphonia, transform decoded frames, and write WAV output
    /// in the same store.
    #[cfg(feature = "audio-io")]
    pub async fn transform_object_audio_file_to_wav_same_store(
        store: &dyn ObjectStore,
        source: &Path,
        target: &Path,
        job: &ObjectAudioTransformJob,
    ) -> Result<ObjectAudioTransformReport> {
        transform_object_audio_file_to_wav_between_stores(store, source, store, target, job).await
    }

    /// Decode an audio file object with Symphonia, transform decoded frames, and write WAV output.
    #[cfg(feature = "audio-io")]
    pub async fn transform_object_audio_file_to_wav_between_stores(
        source_store: &dyn ObjectStore,
        source: &Path,
        target_store: &dyn ObjectStore,
        target: &Path,
        job: &ObjectAudioTransformJob,
    ) -> Result<ObjectAudioTransformReport> {
        let source_format = detect_object_container_format(source_store, source).await?;
        let target_format = target_format_from_path(target)?;
        if target_format != ContainerFormat::Wav {
            return Err(Error::Unsupported {
                operation: "audio transform",
                reason: "Symphonia object audio transforms currently write WAV PCM output",
            });
        }

        let bytes = read_object_bytes(source_store, source).await?;
        let mut decoder = source
            .extension()
            .map(SymphoniaAudioDecoder::with_extension)
            .unwrap_or_default();
        let decoded = decoder.decode(&bytes)?;
        if decoded.is_empty() {
            return Err(Error::EmptyInput);
        }

        let audio_track_id = job.audio_track_id.unwrap_or(1);
        let sample_rate = decoded[0].sample_rate;
        let channels = decoded[0].channels;
        let time_base = TimeBase::new(1, sample_rate as i32)?;
        let mut output_packets = Vec::<EncodedPacket>::new();
        let mut output_pts = 0_i64;
        for frame in &decoded {
            if frame.sample_rate != sample_rate || frame.channels != channels {
                return Err(Error::Unsupported {
                    operation: "audio transform",
                    reason: "decoded audio frames changed format mid-stream",
                });
            }
            let transformed = job.pipeline.apply(frame)?;
            let encoded = containers::encode_pcm_packet(
                &transformed,
                audio_track_id,
                output_pts,
                job.output_encoding,
            )?;
            output_pts += transformed.sample_frames() as i64;
            output_packets.push(encoded);
        }

        let mut output_stream =
            StreamInfo::new(audio_track_id, MediaType::Audio, CodecId::Pcm, time_base)
                .with_audio_format(sample_rate, channels);
        output_stream.duration = Some(output_pts);
        let block_align = channels
            .checked_mul(job.output_encoding.bits_per_sample / 8)
            .ok_or(Error::Unsupported {
                operation: "audio transform",
                reason: "output PCM block alignment is too large",
            })?;
        containers::set_pcm_tags(&mut output_stream, job.output_encoding, block_align);

        let mut output_media = MediaInfo {
            duration_seconds: Some(output_pts as f64 / sample_rate as f64),
            ..Default::default()
        };
        output_media.push_stream(output_stream);

        let bytes = containers::mux_wav_bytes(&output_media, &output_packets)?;
        let bytes_written = bytes.len() as u64;
        write_object_bytes(target_store, target, bytes).await?;

        Ok(ObjectAudioTransformReport {
            source: source.clone(),
            target: target.clone(),
            source_format,
            target_format,
            audio_track_id,
            input_packets: 0,
            decoded_frames: decoded.len(),
            encoded_audio_packets: output_packets.len(),
            bytes_written,
        })
    }

    fn select_audio_track(streams: &[StreamInfo], requested: Option<u32>) -> Result<u32> {
        if let Some(track_id) = requested {
            let stream = streams
                .iter()
                .find(|stream| stream.track_id == track_id)
                .ok_or(Error::IncompatibleTrack {
                    track_id,
                    reason: "requested audio track is missing",
                })?;
            if stream.media_type != MediaType::Audio {
                return Err(Error::IncompatibleTrack {
                    track_id,
                    reason: "requested track is not audio",
                });
            }
            return Ok(track_id);
        }

        streams
            .iter()
            .find(|stream| stream.media_type == MediaType::Audio)
            .map(|stream| stream.track_id)
            .ok_or(Error::Unsupported {
                operation: "audio transform",
                reason: "source media has no audio track",
            })
    }

    fn target_format_from_path(target: &Path) -> Result<ContainerFormat> {
        ContainerFormat::from_path(target).ok_or(Error::Unsupported {
            operation: "container detection",
            reason: "target object key extension is not a recognized media container",
        })
    }
}

#[cfg(feature = "containers")]
pub use object_store_impl::{
    transform_object_audio_between_stores, transform_object_audio_same_store,
};
#[cfg(all(feature = "containers", feature = "audio-io"))]
pub use object_store_impl::{
    transform_object_audio_file_to_wav_between_stores,
    transform_object_audio_file_to_wav_same_store,
};
