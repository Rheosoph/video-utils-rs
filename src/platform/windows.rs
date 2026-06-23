use super::{
    PlatformAudioDecoderConfig, PlatformAudioEncoderConfig, PlatformCodecProbe,
    PlatformVideoDecoderConfig, PlatformVideoEncoderConfig,
};
use crate::{
    audio::AudioFrame,
    backend::BackendKind,
    bitstream::{
        aac::aac_packet_to_raw, h264::h264_packet_to_annex_b, h265::h265_packet_to_annex_b,
    },
    codec::{CodecDirection, CodecId},
    error::{Error, Result},
    frame::RgbaFrame,
    packet::EncodedPacket,
    time::TimeBase,
};
use std::{mem::ManuallyDrop, ptr, slice};
use windows::{
    Win32::{Media::MediaFoundation::*, System::Com::CoTaskMemFree},
    core::GUID,
};

const INPUT_STREAM_ID: u32 = 0;
const OUTPUT_STREAM_ID: u32 = 0;
const DEFAULT_TRACK_ID: u32 = 1;
const HNS_DEN: i32 = 10_000_000;
const DEFAULT_AUDIO_OUTPUT_BYTES: u32 = 1_048_576;
const DEFAULT_COMPRESSED_OUTPUT_BYTES: u32 = 1_048_576;

pub struct TransformHandle {
    codec: CodecId,
    direction: CodecDirection,
    transform: IMFTransform,
    shape: TransformShape,
    output_type: IMFMediaType,
    output_stream: OutputStreamInfo,
}

#[derive(Clone, Copy)]
enum TransformShape {
    Video {
        width: u32,
        height: u32,
        time_base: TimeBase,
        frame_duration: i64,
    },
    Audio {
        sample_rate: u32,
        channels: u16,
    },
}

struct OutputBytes {
    pts_hns: Option<i64>,
    duration_hns: Option<i64>,
    bytes: Vec<u8>,
}

#[derive(Clone, Copy, Debug)]
struct OutputStreamInfo {
    capacity: u32,
    provides_samples: bool,
}

pub fn probe(codec: &CodecId, direction: CodecDirection) -> PlatformCodecProbe {
    let result = enumerate_first(codec, direction, false)
        .map(|(count, _)| format!("Media Foundation transform count={count}"))
        .map_err(|error| error.to_string());

    PlatformCodecProbe {
        backend: Some(BackendKind::WindowsMediaFoundation),
        codec: codec.clone(),
        direction,
        supported: result.is_ok(),
        detail: result.unwrap_or_else(|message| message),
    }
}

pub fn open_video_decoder(config: &PlatformVideoDecoderConfig) -> Result<TransformHandle> {
    open_transform(
        &config.codec,
        CodecDirection::Decode,
        TransformShape::Video {
            width: config.width,
            height: config.height,
            time_base: TimeBase::microseconds(),
            frame_duration: 0,
        },
        None,
    )
}

pub fn open_video_encoder(config: &PlatformVideoEncoderConfig) -> Result<TransformHandle> {
    open_transform(
        &config.codec,
        CodecDirection::Encode,
        TransformShape::Video {
            width: config.width,
            height: config.height,
            time_base: config.time_base,
            frame_duration: config.frame_duration,
        },
        config.bitrate,
    )
}

pub fn open_audio_decoder(config: &PlatformAudioDecoderConfig) -> Result<TransformHandle> {
    open_transform(
        &config.codec,
        CodecDirection::Decode,
        TransformShape::Audio {
            sample_rate: config.sample_rate,
            channels: config.channels,
        },
        None,
    )
}

pub fn open_audio_encoder(config: &PlatformAudioEncoderConfig) -> Result<TransformHandle> {
    open_transform(
        &config.codec,
        CodecDirection::Encode,
        TransformShape::Audio {
            sample_rate: config.sample_rate,
            channels: config.channels,
        },
        config.bitrate,
    )
}

fn open_transform(
    codec: &CodecId,
    direction: CodecDirection,
    shape: TransformShape,
    bitrate: Option<u32>,
) -> Result<TransformHandle> {
    let activate =
        enumerate_first(codec, direction, true)?
            .1
            .ok_or_else(|| Error::CodecBackend {
                codec: codec.clone(),
                operation: "open Media Foundation transform",
                message: "MFTEnumEx returned no activation objects".to_owned(),
            })?;
    // SAFETY: The IMFActivate was returned by Media Foundation enumeration and
    // remains valid for this call.
    let transform: IMFTransform =
        unsafe { activate.ActivateObject() }.map_err(|error| Error::CodecBackend {
            codec: codec.clone(),
            operation: "open Media Foundation transform",
            message: format!("IMFActivate::ActivateObject failed: {error}"),
        })?;
    let output_type = configure_transform(codec, direction, shape, bitrate, &transform)?;
    let output_stream = output_stream_info(codec, direction, shape, &transform)?;
    Ok(TransformHandle {
        codec: codec.clone(),
        direction,
        transform,
        shape,
        output_type,
        output_stream,
    })
}

impl TransformHandle {
    pub fn decode_video_packet(&mut self, packet: &EncodedPacket) -> Result<Vec<RgbaFrame>> {
        debug_assert_eq!(self.direction, CodecDirection::Decode);
        let TransformShape::Video { width, height, .. } = self.shape else {
            return codec_backend_error(
                &self.codec,
                "decode Media Foundation video packet",
                "handle is not a video decoder",
            );
        };

        let input = encoded_video_input(&self.codec, packet)?;
        let pts = packet.time_base.rescale(packet.pts, hns_time_base()?);
        let duration = packet
            .time_base
            .rescale(packet.duration.max(0), hns_time_base()?);
        let mut outputs = self.process_input(&input, pts, duration)?;
        outputs.extend(self.drain_output_bytes()?);
        outputs
            .into_iter()
            .map(|output| rgb32_frame_from_windows_output(output, width, height, &self.codec))
            .collect()
    }

    pub fn encode_video_frame(
        &mut self,
        codec: &CodecId,
        frame: &RgbaFrame,
        pts: i64,
    ) -> Result<Vec<EncodedPacket>> {
        debug_assert_eq!(self.direction, CodecDirection::Encode);
        let TransformShape::Video {
            width,
            height,
            time_base,
            frame_duration,
        } = self.shape
        else {
            return codec_backend_error(
                codec,
                "encode Media Foundation video frame",
                "handle is not a video encoder",
            );
        };
        if frame.width != width || frame.height != height {
            return codec_backend_error(
                codec,
                "encode Media Foundation video frame",
                format!(
                    "frame dimensions {}x{} do not match encoder config {}x{}",
                    frame.width, frame.height, width, height
                ),
            );
        }

        let input = rgba_to_rgb32_bytes(frame);
        let pts_hns = time_base.rescale(pts, hns_time_base()?);
        let duration_hns = time_base.rescale(frame_duration.max(0), hns_time_base()?);
        let mut outputs = self.process_input(&input, pts_hns, duration_hns)?;
        outputs.extend(self.drain_output_bytes()?);
        encoded_packets_from_outputs(codec, outputs, time_base, pts, frame_duration)
    }

    pub fn decode_audio_packet(&mut self, packet: &EncodedPacket) -> Result<Vec<AudioFrame>> {
        debug_assert_eq!(self.direction, CodecDirection::Decode);
        let TransformShape::Audio {
            sample_rate,
            channels,
        } = self.shape
        else {
            return codec_backend_error(
                &self.codec,
                "decode Media Foundation audio packet",
                "handle is not an audio decoder",
            );
        };

        let input = encoded_audio_input(&self.codec, packet)?;
        let pts = packet.time_base.rescale(packet.pts, hns_time_base()?);
        let duration = packet
            .time_base
            .rescale(packet.duration.max(0), hns_time_base()?);
        let mut outputs = self.process_input(&input, pts, duration)?;
        outputs.extend(self.drain_output_bytes()?);
        let fallback_pts = packet
            .time_base
            .rescale(packet.pts, TimeBase::new(1, sample_rate as i32)?);
        outputs
            .into_iter()
            .map(|output| {
                audio_frame_from_windows_output(output, sample_rate, channels, fallback_pts)
            })
            .collect()
    }

    pub fn encode_audio_frame(
        &mut self,
        codec: &CodecId,
        frame: &AudioFrame,
    ) -> Result<Vec<EncodedPacket>> {
        debug_assert_eq!(self.direction, CodecDirection::Encode);
        let TransformShape::Audio {
            sample_rate,
            channels,
        } = self.shape
        else {
            return codec_backend_error(
                codec,
                "encode Media Foundation audio frame",
                "handle is not an audio encoder",
            );
        };
        if frame.sample_rate != sample_rate || frame.channels != channels {
            return codec_backend_error(
                codec,
                "encode Media Foundation audio frame",
                format!(
                    "audio frame format {} Hz/{} channels does not match encoder config {} Hz/{} channels",
                    frame.sample_rate, frame.channels, sample_rate, channels
                ),
            );
        }

        let input = f32le_audio_bytes(&frame.samples_f32_interleaved);
        let time_base = TimeBase::new(1, frame.sample_rate as i32)?;
        let pts_hns = time_base.rescale(frame.pts, hns_time_base()?);
        let duration_hns = time_base.rescale(frame.sample_frames() as i64, hns_time_base()?);
        let mut outputs = self.process_input(&input, pts_hns, duration_hns)?;
        outputs.extend(self.drain_output_bytes()?);
        encoded_packets_from_outputs(
            codec,
            outputs,
            time_base,
            frame.pts,
            frame.sample_frames() as i64,
        )
    }

    pub fn flush_video_decoder(&mut self) -> Result<Vec<RgbaFrame>> {
        let TransformShape::Video { width, height, .. } = self.shape else {
            return Ok(Vec::new());
        };
        self.drain_transform()?;
        self.drain_output_bytes()?
            .into_iter()
            .map(|output| rgb32_frame_from_windows_output(output, width, height, &self.codec))
            .collect()
    }

    pub fn finish_video_encoder(&mut self, codec: &CodecId) -> Result<Vec<EncodedPacket>> {
        let TransformShape::Video {
            time_base,
            frame_duration,
            ..
        } = self.shape
        else {
            return Ok(Vec::new());
        };
        self.drain_transform()?;
        encoded_packets_from_outputs(
            codec,
            self.drain_output_bytes()?,
            time_base,
            0,
            frame_duration,
        )
    }

    pub fn flush_audio_decoder(&mut self) -> Result<Vec<AudioFrame>> {
        let TransformShape::Audio {
            sample_rate,
            channels,
        } = self.shape
        else {
            return Ok(Vec::new());
        };
        self.drain_transform()?;
        self.drain_output_bytes()?
            .into_iter()
            .map(|output| audio_frame_from_windows_output(output, sample_rate, channels, 0))
            .collect()
    }

    pub fn finish_audio_encoder(&mut self, codec: &CodecId) -> Result<Vec<EncodedPacket>> {
        let TransformShape::Audio { sample_rate, .. } = self.shape else {
            return Ok(Vec::new());
        };
        self.drain_transform()?;
        let time_base = TimeBase::new(1, sample_rate as i32)?;
        encoded_packets_from_outputs(codec, self.drain_output_bytes()?, time_base, 0, 0)
    }

    fn process_input(
        &mut self,
        data: &[u8],
        pts_hns: i64,
        duration_hns: i64,
    ) -> Result<Vec<OutputBytes>> {
        let sample = sample_from_bytes(&self.codec, data, pts_hns, duration_hns)?;
        let mut drained = Vec::new();
        loop {
            // SAFETY: The sample owns a contiguous IMFMediaBuffer initialized with
            // the input packet/frame bytes. Stream 0 is selected during media type
            // configuration.
            let result = unsafe { self.transform.ProcessInput(INPUT_STREAM_ID, &sample, 0) };
            match result {
                Ok(()) => return Ok(drained),
                Err(error) if error.code() == MF_E_NOTACCEPTING => {
                    let mut outputs = self.drain_output_bytes()?;
                    if outputs.is_empty() {
                        return Err(Error::CodecBackend {
                            codec: self.codec.clone(),
                            operation: "process Media Foundation input",
                            message: format!(
                                "IMFTransform::ProcessInput returned MF_E_NOTACCEPTING, but ProcessOutput produced no data: {error}"
                            ),
                        });
                    }
                    drained.append(&mut outputs);
                }
                Err(error) => {
                    return Err(Error::CodecBackend {
                        codec: self.codec.clone(),
                        operation: "process Media Foundation input",
                        message: format!("IMFTransform::ProcessInput failed: {error}"),
                    });
                }
            }
        }
    }

    fn drain_transform(&mut self) -> Result<()> {
        // SAFETY: Messages are sent to a live transform to drain queued output.
        unsafe {
            self.transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_END_OF_STREAM, 0)
                .map_err(|error| Error::CodecBackend {
                    codec: self.codec.clone(),
                    operation: "drain Media Foundation transform",
                    message: format!("MFT_MESSAGE_NOTIFY_END_OF_STREAM failed: {error}"),
                })?;
            self.transform
                .ProcessMessage(MFT_MESSAGE_COMMAND_DRAIN, 0)
                .map_err(|error| Error::CodecBackend {
                    codec: self.codec.clone(),
                    operation: "drain Media Foundation transform",
                    message: format!("MFT_MESSAGE_COMMAND_DRAIN failed: {error}"),
                })
        }
    }

    fn drain_output_bytes(&mut self) -> Result<Vec<OutputBytes>> {
        let mut out = Vec::new();
        loop {
            let output_sample = output_sample_for_stream(&self.codec, self.output_stream)?;
            let mut output = MFT_OUTPUT_DATA_BUFFER {
                dwStreamID: OUTPUT_STREAM_ID,
                pSample: ManuallyDrop::new(output_sample),
                dwStatus: 0,
                pEvents: ManuallyDrop::new(None),
            };
            let mut status = 0;
            // SAFETY: The output data buffer either points to a caller-allocated
            // sample sized from GetOutputStreamInfo or leaves pSample null for
            // transforms that provide their own samples.
            let result = unsafe {
                self.transform
                    .ProcessOutput(0, slice::from_mut(&mut output), &mut status)
            };
            // SAFETY: MFT_OUTPUT_DATA_BUFFER uses ManuallyDrop because COM
            // ownership is explicit. Move the fields back into Rust so they are
            // dropped on all result paths.
            let sample = unsafe { ManuallyDrop::take(&mut output.pSample) };
            let _events = unsafe { ManuallyDrop::take(&mut output.pEvents) };

            match result {
                Ok(()) => {
                    if let Some(sample) = sample {
                        let output_bytes = sample_to_output_bytes(&self.codec, &sample)?;
                        if !output_bytes.bytes.is_empty() {
                            out.push(output_bytes);
                        }
                    }
                    if output.dwStatus & MFT_OUTPUT_DATA_BUFFER_STREAM_END.0 as u32 != 0 {
                        break;
                    }
                    if status & MFT_PROCESS_OUTPUT_STATUS_NEW_STREAMS.0 as u32 != 0 {
                        self.refresh_output_stream_info()?;
                        continue;
                    }
                    if output.dwStatus & MFT_OUTPUT_DATA_BUFFER_FORMAT_CHANGE.0 as u32 != 0 {
                        self.renegotiate_output_type()?;
                        continue;
                    }
                    if output.dwStatus & MFT_OUTPUT_DATA_BUFFER_INCOMPLETE.0 as u32 != 0 {
                        continue;
                    }
                }
                Err(error) if error.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => break,
                Err(error) if error.code() == MF_E_TRANSFORM_STREAM_CHANGE => {
                    self.renegotiate_output_type()?;
                    continue;
                }
                Err(error) if error.code() == MF_E_BUFFERTOOSMALL => {
                    let old_capacity = self.output_stream.capacity;
                    self.refresh_output_stream_info()?;
                    if self.output_stream.capacity <= old_capacity {
                        return Err(Error::CodecBackend {
                            codec: self.codec.clone(),
                            operation: "process Media Foundation output",
                            message: format!(
                                "IMFTransform::ProcessOutput reported MF_E_BUFFERTOOSMALL, but output capacity did not grow beyond {old_capacity} bytes: {error}"
                            ),
                        });
                    }
                    continue;
                }
                Err(error) => {
                    return Err(Error::CodecBackend {
                        codec: self.codec.clone(),
                        operation: "process Media Foundation output",
                        message: format!("IMFTransform::ProcessOutput failed: {error}"),
                    });
                }
            }
        }
        Ok(out)
    }

    fn refresh_output_stream_info(&mut self) -> Result<()> {
        self.output_stream =
            output_stream_info(&self.codec, self.direction, self.shape, &self.transform)?;
        Ok(())
    }

    fn renegotiate_output_type(&mut self) -> Result<()> {
        // SAFETY: The desired output type is the same type accepted during
        // initial transform configuration for stream 0.
        unsafe {
            self.transform
                .SetOutputType(OUTPUT_STREAM_ID, &self.output_type, 0)
                .map_err(|error| {
                    mf_error(&self.codec, "renegotiate Media Foundation output", error)
                })?;
        }
        self.refresh_output_stream_info()
    }
}

fn configure_transform(
    codec: &CodecId,
    direction: CodecDirection,
    shape: TransformShape,
    bitrate: Option<u32>,
    transform: &IMFTransform,
) -> Result<IMFMediaType> {
    let compressed_subtype = *subtypes(codec)
        .and_then(|subtypes| subtypes.first())
        .ok_or_else(|| Error::CodecBackend {
            codec: codec.clone(),
            operation: "configure Media Foundation transform",
            message: "codec has no Media Foundation subtype mapping".to_owned(),
        })?;
    let (input_type, output_type) = match (direction, shape) {
        (
            CodecDirection::Decode,
            TransformShape::Video {
                width,
                height,
                time_base,
                frame_duration,
            },
        ) => (
            video_media_type(
                codec,
                &compressed_subtype,
                width,
                height,
                time_base,
                frame_duration,
                None,
            )?,
            video_media_type(
                codec,
                &MFVideoFormat_RGB32,
                width,
                height,
                time_base,
                frame_duration,
                None,
            )?,
        ),
        (
            CodecDirection::Encode,
            TransformShape::Video {
                width,
                height,
                time_base,
                frame_duration,
            },
        ) => (
            video_media_type(
                codec,
                &MFVideoFormat_RGB32,
                width,
                height,
                time_base,
                frame_duration,
                None,
            )?,
            video_media_type(
                codec,
                &compressed_subtype,
                width,
                height,
                time_base,
                frame_duration,
                bitrate,
            )?,
        ),
        (
            CodecDirection::Decode,
            TransformShape::Audio {
                sample_rate,
                channels,
            },
        ) => (
            audio_media_type(codec, &compressed_subtype, sample_rate, channels, bitrate)?,
            audio_media_type(codec, &MFAudioFormat_Float, sample_rate, channels, None)?,
        ),
        (
            CodecDirection::Encode,
            TransformShape::Audio {
                sample_rate,
                channels,
            },
        ) => (
            audio_media_type(codec, &MFAudioFormat_Float, sample_rate, channels, None)?,
            audio_media_type(codec, &compressed_subtype, sample_rate, channels, bitrate)?,
        ),
    };

    // SAFETY: Media types are created through MFCreateMediaType and configured
    // for stream 0, matching the transform enumeration category.
    unsafe {
        match direction {
            CodecDirection::Decode => {
                transform
                    .SetInputType(INPUT_STREAM_ID, &input_type, 0)
                    .map_err(|error| mf_error(codec, "configure Media Foundation input", error))?;
                transform
                    .SetOutputType(OUTPUT_STREAM_ID, &output_type, 0)
                    .map_err(|error| mf_error(codec, "configure Media Foundation output", error))?;
            }
            CodecDirection::Encode => {
                transform
                    .SetOutputType(OUTPUT_STREAM_ID, &output_type, 0)
                    .map_err(|error| mf_error(codec, "configure Media Foundation output", error))?;
                transform
                    .SetInputType(INPUT_STREAM_ID, &input_type, 0)
                    .map_err(|error| mf_error(codec, "configure Media Foundation input", error))?;
            }
        }
        transform
            .ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)
            .map_err(|error| mf_error(codec, "start Media Foundation streaming", error))?;
        transform
            .ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)
            .map_err(|error| mf_error(codec, "start Media Foundation stream", error))?;
    }
    Ok(output_type)
}

fn video_media_type(
    codec: &CodecId,
    subtype: &GUID,
    width: u32,
    height: u32,
    time_base: TimeBase,
    frame_duration: i64,
    bitrate: Option<u32>,
) -> Result<IMFMediaType> {
    // SAFETY: Creates an empty Media Foundation media type.
    let ty = unsafe { MFCreateMediaType() }
        .map_err(|error| mf_error(codec, "create Media Foundation video media type", error))?;
    // SAFETY: Attribute keys and values are Media Foundation constants.
    unsafe {
        ty.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)
            .map_err(|error| mf_error(codec, "set Media Foundation video major type", error))?;
        ty.SetGUID(&MF_MT_SUBTYPE, subtype)
            .map_err(|error| mf_error(codec, "set Media Foundation video subtype", error))?;
        ty.SetUINT64(&MF_MT_FRAME_SIZE, pack_u32_pair(width, height))
            .map_err(|error| mf_error(codec, "set Media Foundation frame size", error))?;
        ty.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pack_u32_pair(1, 1))
            .map_err(|error| mf_error(codec, "set Media Foundation pixel aspect ratio", error))?;
        ty.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)
            .map_err(|error| mf_error(codec, "set Media Foundation interlace mode", error))?;
        if frame_duration > 0 {
            let seconds = time_base.ticks_to_seconds(frame_duration);
            if seconds.is_finite() && seconds > 0.0 {
                let fps = (1.0 / seconds).round().clamp(1.0, u32::MAX as f64) as u32;
                ty.SetUINT64(&MF_MT_FRAME_RATE, pack_u32_pair(fps, 1))
                    .map_err(|error| mf_error(codec, "set Media Foundation frame rate", error))?;
            }
        }
        if let Some(bitrate) = bitrate {
            ty.SetUINT32(&MF_MT_AVG_BITRATE, bitrate)
                .map_err(|error| mf_error(codec, "set Media Foundation bitrate", error))?;
        }
    }
    Ok(ty)
}

fn audio_media_type(
    codec: &CodecId,
    subtype: &GUID,
    sample_rate: u32,
    channels: u16,
    bitrate: Option<u32>,
) -> Result<IMFMediaType> {
    // SAFETY: Creates an empty Media Foundation media type.
    let ty = unsafe { MFCreateMediaType() }
        .map_err(|error| mf_error(codec, "create Media Foundation audio media type", error))?;
    // SAFETY: Attribute keys and values are Media Foundation constants.
    unsafe {
        ty.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio)
            .map_err(|error| mf_error(codec, "set Media Foundation audio major type", error))?;
        ty.SetGUID(&MF_MT_SUBTYPE, subtype)
            .map_err(|error| mf_error(codec, "set Media Foundation audio subtype", error))?;
        ty.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, sample_rate)
            .map_err(|error| mf_error(codec, "set Media Foundation sample rate", error))?;
        ty.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, u32::from(channels))
            .map_err(|error| mf_error(codec, "set Media Foundation channels", error))?;
        if *subtype == MFAudioFormat_Float {
            let block_align = u32::from(channels) * 4;
            ty.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 32)
                .map_err(|error| mf_error(codec, "set Media Foundation bits/sample", error))?;
            ty.SetUINT32(&MF_MT_AUDIO_BLOCK_ALIGNMENT, block_align)
                .map_err(|error| mf_error(codec, "set Media Foundation block alignment", error))?;
            ty.SetUINT32(
                &MF_MT_AUDIO_AVG_BYTES_PER_SECOND,
                sample_rate.saturating_mul(block_align),
            )
            .map_err(|error| mf_error(codec, "set Media Foundation avg bytes/sec", error))?;
        } else if let Some(bitrate) = bitrate {
            ty.SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, bitrate / 8)
                .map_err(|error| mf_error(codec, "set Media Foundation avg bytes/sec", error))?;
        }
    }
    Ok(ty)
}

fn enumerate_first(
    codec: &CodecId,
    direction: CodecDirection,
    keep_first: bool,
) -> Result<(u32, Option<IMFActivate>)> {
    let category = category(codec, direction).ok_or_else(|| Error::CodecBackend {
        codec: codec.clone(),
        operation: "enumerate Media Foundation transforms",
        message: "codec has no Media Foundation category mapping".to_owned(),
    })?;
    let major_type = major_type(codec).ok_or_else(|| Error::CodecBackend {
        codec: codec.clone(),
        operation: "enumerate Media Foundation transforms",
        message: "codec has no Media Foundation major type mapping".to_owned(),
    })?;
    let subtypes = subtypes(codec).ok_or_else(|| Error::CodecBackend {
        codec: codec.clone(),
        operation: "enumerate Media Foundation transforms",
        message: "codec has no Media Foundation subtype mapping".to_owned(),
    })?;

    // SAFETY: Initializes Media Foundation for transform enumeration on this
    // thread/process. Repeated MFStartup calls are permitted by the API.
    unsafe { MFStartup(MF_VERSION, MFSTARTUP_FULL) }.map_err(|error| Error::CodecBackend {
        codec: codec.clone(),
        operation: "enumerate Media Foundation transforms",
        message: format!("MFStartup failed: {error}"),
    })?;

    let mut last_error = None;
    for subtype in subtypes {
        let mut type_info = MFT_REGISTER_TYPE_INFO {
            guidMajorType: major_type,
            guidSubtype: *subtype,
        };
        let (input_type, output_type) = match direction {
            CodecDirection::Decode => (Some(&type_info as *const MFT_REGISTER_TYPE_INFO), None),
            CodecDirection::Encode => (None, Some(&type_info as *const MFT_REGISTER_TYPE_INFO)),
        };

        let mut activates: *mut Option<IMFActivate> = ptr::null_mut();
        let mut count = 0;
        let flags = MFT_ENUM_FLAG_SYNCMFT
            | MFT_ENUM_FLAG_ASYNCMFT
            | MFT_ENUM_FLAG_HARDWARE
            | MFT_ENUM_FLAG_LOCALMFT;
        // SAFETY: All pointers either point to local MFT_REGISTER_TYPE_INFO
        // values valid for the call or are null through Option::None. The output
        // array is freed with CoTaskMemFree below.
        let result = unsafe {
            MFTEnumEx(
                category,
                flags,
                input_type,
                output_type,
                &mut activates,
                &mut count,
            )
        };

        match result {
            Ok(()) if count > 0 && !activates.is_null() => {
                let first = take_first_activation(activates, count, keep_first);
                return Ok((count, first));
            }
            Ok(()) => {
                last_error = Some("MFTEnumEx returned zero transforms".to_owned());
                if !activates.is_null() {
                    // SAFETY: activates was allocated by Media Foundation.
                    unsafe {
                        CoTaskMemFree(Some(activates.cast()));
                    }
                }
            }
            Err(error) => {
                last_error = Some(format!("MFTEnumEx failed: {error}"));
                if !activates.is_null() {
                    // SAFETY: activates was allocated by Media Foundation.
                    unsafe {
                        CoTaskMemFree(Some(activates.cast()));
                    }
                }
            }
        }

        // Keep mutable local visible as mutable for APIs that may require
        // non-const struct ABI in future windows-rs updates.
        type_info.guidMajorType = major_type;
    }

    Err(Error::CodecBackend {
        codec: codec.clone(),
        operation: "enumerate Media Foundation transforms",
        message: last_error.unwrap_or_else(|| "no subtype produced a transform".to_owned()),
    })
}

fn take_first_activation(
    activates: *mut Option<IMFActivate>,
    count: u32,
    keep_first: bool,
) -> Option<IMFActivate> {
    // SAFETY: MFTEnumEx returned `count` activation entries in this CoTaskMem
    // array. Each Option<IMFActivate> is moved out at most once before freeing.
    unsafe {
        let entries = slice::from_raw_parts_mut(activates, count as usize);
        let mut first = None;
        for entry in entries {
            if keep_first && first.is_none() {
                first = entry.take();
            } else {
                let _ = entry.take();
            }
        }
        CoTaskMemFree(Some(activates.cast()));
        first
    }
}

fn sample_from_bytes(
    codec: &CodecId,
    data: &[u8],
    pts_hns: i64,
    duration_hns: i64,
) -> Result<IMFSample> {
    let len = u32::try_from(data.len()).map_err(|_| Error::CodecBackend {
        codec: codec.clone(),
        operation: "create Media Foundation input sample",
        message: "input buffer is larger than u32::MAX".to_owned(),
    })?;
    // SAFETY: Creates an empty sample and a memory buffer of the requested size.
    let sample = unsafe { MFCreateSample() }
        .map_err(|error| mf_error(codec, "create Media Foundation input sample", error))?;
    let buffer = unsafe { MFCreateMemoryBuffer(len.max(1)) }
        .map_err(|error| mf_error(codec, "create Media Foundation input buffer", error))?;
    copy_bytes_to_buffer(codec, &buffer, data)?;
    // SAFETY: The sample and buffer are valid Media Foundation objects.
    unsafe {
        sample
            .AddBuffer(&buffer)
            .map_err(|error| mf_error(codec, "attach Media Foundation input buffer", error))?;
        sample
            .SetSampleTime(pts_hns)
            .map_err(|error| mf_error(codec, "set Media Foundation sample time", error))?;
        sample
            .SetSampleDuration(duration_hns)
            .map_err(|error| mf_error(codec, "set Media Foundation sample duration", error))?;
    }
    Ok(sample)
}

fn empty_output_sample(codec: &CodecId, capacity: u32) -> Result<IMFSample> {
    // SAFETY: Creates an empty sample and memory buffer for MFT output.
    let sample = unsafe { MFCreateSample() }
        .map_err(|error| mf_error(codec, "create Media Foundation output sample", error))?;
    let buffer = unsafe { MFCreateMemoryBuffer(capacity.max(1)) }
        .map_err(|error| mf_error(codec, "create Media Foundation output buffer", error))?;
    // SAFETY: The sample and buffer are valid Media Foundation objects.
    unsafe {
        sample
            .AddBuffer(&buffer)
            .map_err(|error| mf_error(codec, "attach Media Foundation output buffer", error))?;
    }
    Ok(sample)
}

fn output_sample_for_stream(
    codec: &CodecId,
    stream: OutputStreamInfo,
) -> Result<Option<IMFSample>> {
    if stream.provides_samples {
        Ok(None)
    } else {
        empty_output_sample(codec, stream.capacity).map(Some)
    }
}

fn output_stream_info(
    codec: &CodecId,
    direction: CodecDirection,
    shape: TransformShape,
    transform: &IMFTransform,
) -> Result<OutputStreamInfo> {
    // SAFETY: The transform has configured stream 0 media types before this is
    // queried. Media Foundation returns allocation requirements by value.
    let info = unsafe { transform.GetOutputStreamInfo(OUTPUT_STREAM_ID) }
        .map_err(|error| mf_error(codec, "query Media Foundation output stream info", error))?;
    let flags = info.dwFlags;
    let must_provide_samples = flags & MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 as u32 != 0;
    let can_provide_samples = flags & MFT_OUTPUT_STREAM_CAN_PROVIDE_SAMPLES.0 as u32 != 0;
    let provides_samples = must_provide_samples || (can_provide_samples && info.cbSize == 0);
    let capacity = if provides_samples {
        info.cbSize
    } else {
        info.cbSize
            .max(fallback_output_capacity(direction, shape)?)
            .max(1)
    };
    Ok(OutputStreamInfo {
        capacity,
        provides_samples,
    })
}

fn fallback_output_capacity(direction: CodecDirection, shape: TransformShape) -> Result<u32> {
    match (direction, shape) {
        (CodecDirection::Decode, TransformShape::Video { width, height, .. }) => {
            video_output_capacity(width, height)
        }
        (CodecDirection::Decode, TransformShape::Audio { .. }) => Ok(DEFAULT_AUDIO_OUTPUT_BYTES),
        (CodecDirection::Encode, TransformShape::Video { .. })
        | (CodecDirection::Encode, TransformShape::Audio { .. }) => {
            Ok(DEFAULT_COMPRESSED_OUTPUT_BYTES)
        }
    }
}

fn copy_bytes_to_buffer(codec: &CodecId, buffer: &IMFMediaBuffer, data: &[u8]) -> Result<()> {
    let len = u32::try_from(data.len()).map_err(|_| Error::CodecBackend {
        codec: codec.clone(),
        operation: "copy Media Foundation buffer",
        message: "buffer is larger than u32::MAX".to_owned(),
    })?;
    let mut ptr = ptr::null_mut();
    let mut max_len = 0;
    // SAFETY: Lock returns a writable pointer for this memory buffer until
    // Unlock is called below.
    unsafe {
        buffer
            .Lock(&mut ptr, Some(&mut max_len), None)
            .map_err(|error| mf_error(codec, "lock Media Foundation input buffer", error))?;
        if len > max_len {
            let _ = buffer.Unlock();
            return codec_backend_error(
                codec,
                "copy Media Foundation buffer",
                format!(
                    "buffer holds {max_len} bytes but {} bytes were requested",
                    len
                ),
            );
        }
        if !data.is_empty() {
            ptr::copy_nonoverlapping(data.as_ptr(), ptr, data.len());
        }
        buffer
            .Unlock()
            .map_err(|error| mf_error(codec, "unlock Media Foundation input buffer", error))?;
        buffer
            .SetCurrentLength(len)
            .map_err(|error| mf_error(codec, "set Media Foundation buffer length", error))?;
    }
    Ok(())
}

fn sample_to_output_bytes(codec: &CodecId, sample: &IMFSample) -> Result<OutputBytes> {
    // SAFETY: The sample came from ProcessOutput. ConvertToContiguousBuffer
    // gives a readable media buffer for its combined payload.
    let (pts_hns, duration_hns, buffer) = unsafe {
        let pts_hns = sample.GetSampleTime().ok();
        let duration_hns = sample.GetSampleDuration().ok();
        let buffer = sample
            .ConvertToContiguousBuffer()
            .map_err(|error| mf_error(codec, "read Media Foundation output sample", error))?;
        (pts_hns, duration_hns, buffer)
    };
    let mut ptr = ptr::null_mut();
    let mut len = 0;
    // SAFETY: Lock returns a readable pointer for this media buffer until
    // Unlock is called below.
    let bytes = unsafe {
        buffer
            .Lock(&mut ptr, None, Some(&mut len))
            .map_err(|error| mf_error(codec, "lock Media Foundation output buffer", error))?;
        let bytes = if ptr.is_null() || len == 0 {
            Vec::new()
        } else {
            slice::from_raw_parts(ptr, len as usize).to_vec()
        };
        buffer
            .Unlock()
            .map_err(|error| mf_error(codec, "unlock Media Foundation output buffer", error))?;
        bytes
    };

    Ok(OutputBytes {
        pts_hns,
        duration_hns,
        bytes,
    })
}

fn encoded_video_input(codec: &CodecId, packet: &EncodedPacket) -> Result<bytes::Bytes> {
    match codec {
        CodecId::H264 => h264_packet_to_annex_b(packet, None),
        CodecId::H265 => h265_packet_to_annex_b(packet, None),
        _ => Ok(packet.data.clone()),
    }
}

fn encoded_audio_input(codec: &CodecId, packet: &EncodedPacket) -> Result<bytes::Bytes> {
    match codec {
        CodecId::Aac => aac_packet_to_raw(packet),
        _ => Ok(packet.data.clone()),
    }
}

fn rgb32_frame_from_windows_output(
    output: OutputBytes,
    width: u32,
    height: u32,
    codec: &CodecId,
) -> Result<RgbaFrame> {
    let row_bytes = width as usize * 4;
    let expected = row_bytes * height as usize;
    if output.bytes.len() < expected {
        return codec_backend_error(
            codec,
            "decode Media Foundation video packet",
            format!(
                "decoded RGB32 output has {} bytes, expected at least {expected}",
                output.bytes.len()
            ),
        );
    }

    let mut rgba = Vec::with_capacity(expected);
    for bgra in output.bytes[..expected].chunks_exact(4) {
        rgba.extend_from_slice(&[bgra[2], bgra[1], bgra[0], bgra[3]]);
    }
    RgbaFrame::new(width, height, row_bytes, rgba)
}

fn audio_frame_from_windows_output(
    output: OutputBytes,
    sample_rate: u32,
    channels: u16,
    fallback_pts: i64,
) -> Result<AudioFrame> {
    let samples = output
        .bytes
        .chunks_exact(4)
        .map(|bytes| f32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
        .collect();
    let pts = match output.pts_hns {
        Some(pts_hns) => hns_time_base()?.rescale(pts_hns, TimeBase::new(1, sample_rate as i32)?),
        None => fallback_pts,
    };
    AudioFrame::new(sample_rate, channels, pts, samples)
}

fn encoded_packets_from_outputs(
    codec: &CodecId,
    outputs: Vec<OutputBytes>,
    time_base: TimeBase,
    fallback_pts: i64,
    fallback_duration: i64,
) -> Result<Vec<EncodedPacket>> {
    let hns = hns_time_base()?;
    Ok(outputs
        .into_iter()
        .map(|output| {
            let pts = output
                .pts_hns
                .map(|pts| hns.rescale(pts, time_base))
                .unwrap_or(fallback_pts);
            let duration = output
                .duration_hns
                .map(|duration| hns.rescale(duration, time_base))
                .unwrap_or(fallback_duration);
            EncodedPacket::new(
                DEFAULT_TRACK_ID,
                codec.clone(),
                pts,
                duration,
                time_base,
                output.bytes,
            )
        })
        .collect())
}

fn rgba_to_rgb32_bytes(frame: &RgbaFrame) -> Vec<u8> {
    let row_bytes = frame.width as usize * 4;
    let mut out = Vec::with_capacity(row_bytes * frame.height as usize);
    for row in 0..frame.height as usize {
        let offset = row * frame.stride;
        for rgba in frame.data[offset..offset + row_bytes].chunks_exact(4) {
            out.extend_from_slice(&[rgba[2], rgba[1], rgba[0], rgba[3]]);
        }
    }
    out
}

fn f32le_audio_bytes(samples: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(samples.len() * 4);
    for sample in samples {
        out.extend_from_slice(&sample.to_le_bytes());
    }
    out
}

fn video_output_capacity(width: u32, height: u32) -> Result<u32> {
    width
        .checked_mul(height)
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or(Error::CodecBackend {
            codec: CodecId::RawVideo,
            operation: "allocate Media Foundation video output",
            message: "video frame dimensions overflow output buffer size".to_owned(),
        })
}

fn pack_u32_pair(hi: u32, lo: u32) -> u64 {
    (u64::from(hi) << 32) | u64::from(lo)
}

fn hns_time_base() -> Result<TimeBase> {
    TimeBase::new(1, HNS_DEN)
}

fn category(codec: &CodecId, direction: CodecDirection) -> Option<GUID> {
    match (codec.is_video(), codec.is_audio(), direction) {
        (true, _, CodecDirection::Decode) => Some(MFT_CATEGORY_VIDEO_DECODER),
        (true, _, CodecDirection::Encode) => Some(MFT_CATEGORY_VIDEO_ENCODER),
        (_, true, CodecDirection::Decode) => Some(MFT_CATEGORY_AUDIO_DECODER),
        (_, true, CodecDirection::Encode) => Some(MFT_CATEGORY_AUDIO_ENCODER),
        _ => None,
    }
}

fn major_type(codec: &CodecId) -> Option<GUID> {
    if codec.is_video() {
        Some(MFMediaType_Video)
    } else if codec.is_audio() {
        Some(MFMediaType_Audio)
    } else {
        None
    }
}

fn subtypes(codec: &CodecId) -> Option<&'static [GUID]> {
    match codec {
        CodecId::H264 => Some(&[MFVideoFormat_H264, MFVideoFormat_H264_ES]),
        CodecId::H265 => Some(&[MFVideoFormat_HEVC, MFVideoFormat_HEVC_ES]),
        CodecId::Aac => Some(&[MFAudioFormat_AAC]),
        CodecId::Eac3 => Some(&[MFAudioFormat_Dolby_DDPlus]),
        CodecId::Dts => Some(&[
            MFAudioFormat_DTS,
            MFAudioFormat_DTS_HD,
            MFAudioFormat_DTS_LBR,
            MFAudioFormat_DTS_UHD,
        ]),
        CodecId::Wma => Some(&[MFAudioFormat_WMAudioV9, MFAudioFormat_WMAudioV8]),
        _ => None,
    }
}

fn mf_error(codec: &CodecId, operation: &'static str, error: windows::core::Error) -> Error {
    Error::CodecBackend {
        codec: codec.clone(),
        operation,
        message: error.to_string(),
    }
}

fn codec_backend_error<T>(
    codec: &CodecId,
    operation: &'static str,
    message: impl Into<String>,
) -> Result<T> {
    Err(Error::CodecBackend {
        codec: codec.clone(),
        operation,
        message: message.into(),
    })
}
