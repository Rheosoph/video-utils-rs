use super::{
    PlatformAudioDecoderConfig, PlatformAudioEncoderConfig, PlatformCodecProbe,
    PlatformVideoDecoderConfig, PlatformVideoEncoderConfig,
};
use crate::{
    audio::AudioFrame,
    bitstream::{
        aac::aac_packet_to_raw,
        h264::{avcc_parameter_sets, h264_packet_to_annex_b},
        h265::{h265_packet_to_annex_b, hvcc_parameter_sets},
    },
    codec::{CodecDirection, CodecId},
    error::{Error, Result},
    frame::RgbaFrame,
    packet::EncodedPacket,
    time::TimeBase,
};
use std::{
    ffi::{CString, c_char, c_void},
    ptr, slice,
};

#[repr(C)]
struct AMediaCodec {
    _private: [u8; 0],
}

#[repr(C)]
struct AMediaFormat {
    _private: [u8; 0],
}

#[repr(C)]
struct AMediaCrypto {
    _private: [u8; 0],
}

#[repr(C)]
struct ANativeWindow {
    _private: [u8; 0],
}

#[repr(C)]
#[derive(Clone, Copy)]
struct AMediaCodecBufferInfo {
    offset: isize,
    size: isize,
    presentation_time_us: i64,
    flags: u32,
}

type MediaStatus = i32;
type SSize = isize;

const AMEDIA_OK: MediaStatus = 0;
const AMEDIACODEC_CONFIGURE_FLAG_ENCODE: u32 = 1;
const AMEDIACODEC_BUFFER_FLAG_END_OF_STREAM: u32 = 4;
const AMEDIACODEC_INFO_TRY_AGAIN_LATER: SSize = -1;
const AMEDIACODEC_INFO_OUTPUT_BUFFERS_CHANGED: SSize = -3;
const AMEDIACODEC_INFO_OUTPUT_FORMAT_CHANGED: SSize = -2;
const DEQUEUE_TIMEOUT_US: i64 = 10_000;
const DEFAULT_TRACK_ID: u32 = 1;
const COLOR_FORMAT_32BIT_ABGR8888: i32 = 0x7f00_a000;
const PCM_ENCODING_FLOAT: i32 = 4;

#[link(name = "mediandk")]
unsafe extern "C" {
    fn AMediaCodec_createDecoderByType(mime_type: *const c_char) -> *mut AMediaCodec;
    fn AMediaCodec_createEncoderByType(mime_type: *const c_char) -> *mut AMediaCodec;
    fn AMediaCodec_configure(
        codec: *mut AMediaCodec,
        format: *const AMediaFormat,
        surface: *mut ANativeWindow,
        crypto: *mut AMediaCrypto,
        flags: u32,
    ) -> MediaStatus;
    fn AMediaCodec_start(codec: *mut AMediaCodec) -> MediaStatus;
    fn AMediaCodec_stop(codec: *mut AMediaCodec) -> MediaStatus;
    fn AMediaCodec_delete(codec: *mut AMediaCodec) -> MediaStatus;
    fn AMediaCodec_dequeueInputBuffer(codec: *mut AMediaCodec, timeout_us: i64) -> SSize;
    fn AMediaCodec_getInputBuffer(
        codec: *mut AMediaCodec,
        idx: usize,
        out_size: *mut usize,
    ) -> *mut u8;
    fn AMediaCodec_queueInputBuffer(
        codec: *mut AMediaCodec,
        idx: usize,
        offset: isize,
        size: usize,
        time: u64,
        flags: u32,
    ) -> MediaStatus;
    fn AMediaCodec_dequeueOutputBuffer(
        codec: *mut AMediaCodec,
        info: *mut AMediaCodecBufferInfo,
        timeout_us: i64,
    ) -> SSize;
    fn AMediaCodec_getOutputBuffer(
        codec: *mut AMediaCodec,
        idx: usize,
        out_size: *mut usize,
    ) -> *mut u8;
    fn AMediaCodec_releaseOutputBuffer(
        codec: *mut AMediaCodec,
        idx: usize,
        render: bool,
    ) -> MediaStatus;
    fn AMediaFormat_new() -> *mut AMediaFormat;
    fn AMediaFormat_delete(format: *mut AMediaFormat);
    fn AMediaFormat_setString(format: *mut AMediaFormat, name: *const c_char, value: *const c_char);
    fn AMediaFormat_setInt32(format: *mut AMediaFormat, name: *const c_char, value: i32);
    fn AMediaFormat_setBuffer(
        format: *mut AMediaFormat,
        name: *const c_char,
        data: *mut c_void,
        size: usize,
    );
}

pub struct CodecHandle {
    codec: CodecId,
    direction: CodecDirection,
    ptr: *mut AMediaCodec,
    shape: CodecShape,
}

#[derive(Clone, Copy)]
enum CodecShape {
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

impl Drop for CodecHandle {
    fn drop(&mut self) {
        // SAFETY: The handle owns the AMediaCodec pointer returned by the NDK.
        unsafe {
            if !self.ptr.is_null() {
                let _ = AMediaCodec_stop(self.ptr);
                let _ = AMediaCodec_delete(self.ptr);
            }
        }
    }
}

pub fn probe(codec: &CodecId, direction: CodecDirection) -> PlatformCodecProbe {
    let result = open_probe_codec(codec, direction)
        .map(|_| "AMediaCodec instance created by MIME type".to_owned())
        .map_err(|error| error.to_string());

    PlatformCodecProbe {
        backend: Some(crate::backend::BackendKind::AndroidMediaCodec),
        codec: codec.clone(),
        direction,
        supported: result.is_ok(),
        detail: result.unwrap_or_else(|message| message),
    }
}

pub fn open_video_decoder(config: &PlatformVideoDecoderConfig) -> Result<CodecHandle> {
    let mut handle = open_codec(
        &config.codec,
        CodecDirection::Decode,
        CodecShape::Video {
            width: config.width,
            height: config.height,
            time_base: TimeBase::microseconds(),
            frame_duration: 0,
        },
    )?;
    configure_video_codec(
        &mut handle,
        config.width,
        config.height,
        None,
        (!config.extra_data.is_empty()).then_some(config.extra_data.as_slice()),
    )?;
    Ok(handle)
}

pub fn open_video_encoder(config: &PlatformVideoEncoderConfig) -> Result<CodecHandle> {
    let mut handle = open_codec(
        &config.codec,
        CodecDirection::Encode,
        CodecShape::Video {
            width: config.width,
            height: config.height,
            time_base: config.time_base,
            frame_duration: config.frame_duration,
        },
    )?;
    configure_video_codec(
        &mut handle,
        config.width,
        config.height,
        config.bitrate,
        None,
    )?;
    Ok(handle)
}

pub fn open_audio_decoder(config: &PlatformAudioDecoderConfig) -> Result<CodecHandle> {
    let mut handle = open_codec(
        &config.codec,
        CodecDirection::Decode,
        CodecShape::Audio {
            sample_rate: config.sample_rate,
            channels: config.channels,
        },
    )?;
    configure_audio_codec(
        &mut handle,
        config.sample_rate,
        config.channels,
        None,
        (!config.extra_data.is_empty()).then_some(config.extra_data.as_slice()),
    )?;
    Ok(handle)
}

pub fn open_audio_encoder(config: &PlatformAudioEncoderConfig) -> Result<CodecHandle> {
    let mut handle = open_codec(
        &config.codec,
        CodecDirection::Encode,
        CodecShape::Audio {
            sample_rate: config.sample_rate,
            channels: config.channels,
        },
    )?;
    configure_audio_codec(
        &mut handle,
        config.sample_rate,
        config.channels,
        config.bitrate,
        None,
    )?;
    Ok(handle)
}

fn open_probe_codec(codec: &CodecId, direction: CodecDirection) -> Result<CodecHandle> {
    let shape = if codec.is_video() {
        CodecShape::Video {
            width: 16,
            height: 16,
            time_base: TimeBase::milliseconds(),
            frame_duration: 33,
        }
    } else {
        CodecShape::Audio {
            sample_rate: 48_000,
            channels: 2,
        }
    };
    open_codec(codec, direction, shape)
}

fn open_codec(
    codec: &CodecId,
    direction: CodecDirection,
    shape: CodecShape,
) -> Result<CodecHandle> {
    let mime = android_mime(codec).ok_or_else(|| Error::CodecBackend {
        codec: codec.clone(),
        operation: "open AMediaCodec",
        message: "codec has no Android MediaCodec MIME mapping".to_owned(),
    })?;
    let mime = CString::new(mime).map_err(|error| Error::CodecBackend {
        codec: codec.clone(),
        operation: "open AMediaCodec",
        message: error.to_string(),
    })?;

    // SAFETY: The CString is null-terminated and lives for the duration of the
    // call. Ownership of a non-null AMediaCodec pointer is transferred to the
    // returned handle.
    let ptr = unsafe {
        match direction {
            CodecDirection::Decode => AMediaCodec_createDecoderByType(mime.as_ptr()),
            CodecDirection::Encode => AMediaCodec_createEncoderByType(mime.as_ptr()),
        }
    };

    if ptr.is_null() {
        return Err(Error::CodecBackend {
            codec: codec.clone(),
            operation: "open AMediaCodec",
            message: "AMediaCodec_create*ByType returned null".to_owned(),
        });
    }

    Ok(CodecHandle {
        codec: codec.clone(),
        direction,
        ptr,
        shape,
    })
}

impl CodecHandle {
    pub fn decode_video_packet(&mut self, packet: &EncodedPacket) -> Result<Vec<RgbaFrame>> {
        debug_assert_eq!(self.direction, CodecDirection::Decode);
        let CodecShape::Video { width, height, .. } = self.shape else {
            return codec_backend_error(
                &self.codec,
                "decode AMediaCodec video packet",
                "handle is not a video decoder",
            );
        };
        let input = encoded_video_input(&self.codec, packet)?;
        self.queue_bytes(
            &input,
            packet
                .time_base
                .rescale(packet.pts, TimeBase::microseconds()) as u64,
            0,
        )?;
        let outputs = self.drain_output_bytes()?;
        outputs
            .into_iter()
            .map(|output| rgba_frame_from_android_output(output, width, height, &self.codec))
            .collect()
    }

    pub fn encode_video_frame(
        &mut self,
        codec: &CodecId,
        frame: &RgbaFrame,
        pts: i64,
    ) -> Result<Vec<EncodedPacket>> {
        debug_assert_eq!(self.direction, CodecDirection::Encode);
        let CodecShape::Video {
            width,
            height,
            time_base,
            frame_duration,
        } = self.shape
        else {
            return codec_backend_error(
                codec,
                "encode AMediaCodec video frame",
                "handle is not a video encoder",
            );
        };
        if frame.width != width || frame.height != height {
            return codec_backend_error(
                codec,
                "encode AMediaCodec video frame",
                format!(
                    "frame dimensions {}x{} do not match encoder config {}x{}",
                    frame.width, frame.height, width, height
                ),
            );
        }
        let input = tight_rgba_bytes(frame);
        let pts_us = time_base.rescale(pts, TimeBase::microseconds()) as u64;
        self.queue_bytes(&input, pts_us, 0)?;
        Ok(encoded_packets_from_outputs(
            codec,
            self.drain_output_bytes()?,
            time_base,
            pts,
            frame_duration,
        ))
    }

    pub fn flush_video_decoder(&mut self) -> Result<Vec<RgbaFrame>> {
        let CodecShape::Video { width, height, .. } = self.shape else {
            return Ok(Vec::new());
        };
        self.queue_eos()?;
        let outputs = self.drain_output_bytes()?;
        outputs
            .into_iter()
            .map(|output| rgba_frame_from_android_output(output, width, height, &self.codec))
            .collect()
    }

    pub fn finish_video_encoder(&mut self, codec: &CodecId) -> Result<Vec<EncodedPacket>> {
        let CodecShape::Video {
            time_base,
            frame_duration,
            ..
        } = self.shape
        else {
            return Ok(Vec::new());
        };
        self.queue_eos()?;
        Ok(encoded_packets_from_outputs(
            codec,
            self.drain_output_bytes()?,
            time_base,
            0,
            frame_duration,
        ))
    }

    pub fn decode_audio_packet(&mut self, packet: &EncodedPacket) -> Result<Vec<AudioFrame>> {
        debug_assert_eq!(self.direction, CodecDirection::Decode);
        let CodecShape::Audio {
            sample_rate,
            channels,
        } = self.shape
        else {
            return codec_backend_error(
                &self.codec,
                "decode AMediaCodec audio packet",
                "handle is not an audio decoder",
            );
        };
        let input = encoded_audio_input(&self.codec, packet)?;
        let pts_us = packet
            .time_base
            .rescale(packet.pts, TimeBase::microseconds()) as u64;
        self.queue_bytes(&input, pts_us, 0)?;
        let sample_time_base = TimeBase::new(1, sample_rate as i32)?;
        let pts = packet.time_base.rescale(packet.pts, sample_time_base);
        self.drain_output_bytes()?
            .into_iter()
            .map(|output| audio_frame_from_android_output(output, sample_rate, channels, pts))
            .collect()
    }

    pub fn encode_audio_frame(
        &mut self,
        codec: &CodecId,
        frame: &AudioFrame,
    ) -> Result<Vec<EncodedPacket>> {
        debug_assert_eq!(self.direction, CodecDirection::Encode);
        let CodecShape::Audio {
            sample_rate,
            channels,
        } = self.shape
        else {
            return codec_backend_error(
                codec,
                "encode AMediaCodec audio frame",
                "handle is not an audio encoder",
            );
        };
        if frame.sample_rate != sample_rate || frame.channels != channels {
            return codec_backend_error(
                codec,
                "encode AMediaCodec audio frame",
                format!(
                    "audio frame format {} Hz/{} channels does not match encoder config {} Hz/{} channels",
                    frame.sample_rate, frame.channels, sample_rate, channels
                ),
            );
        }
        let input = f32le_audio_bytes(&frame.samples_f32_interleaved);
        let time_base = TimeBase::new(1, frame.sample_rate as i32)?;
        let pts_us = time_base.rescale(frame.pts, TimeBase::microseconds()) as u64;
        self.queue_bytes(&input, pts_us, 0)?;
        Ok(encoded_packets_from_outputs(
            codec,
            self.drain_output_bytes()?,
            time_base,
            frame.pts,
            frame.sample_frames() as i64,
        ))
    }

    pub fn flush_audio_decoder(&mut self) -> Result<Vec<AudioFrame>> {
        let CodecShape::Audio {
            sample_rate,
            channels,
        } = self.shape
        else {
            return Ok(Vec::new());
        };
        self.queue_eos()?;
        self.drain_output_bytes()?
            .into_iter()
            .map(|output| audio_frame_from_android_output(output, sample_rate, channels, 0))
            .collect()
    }

    pub fn finish_audio_encoder(&mut self, codec: &CodecId) -> Result<Vec<EncodedPacket>> {
        let CodecShape::Audio { sample_rate, .. } = self.shape else {
            return Ok(Vec::new());
        };
        self.queue_eos()?;
        let time_base = TimeBase::new(1, sample_rate as i32)?;
        Ok(encoded_packets_from_outputs(
            codec,
            self.drain_output_bytes()?,
            time_base,
            0,
            0,
        ))
    }

    fn queue_eos(&mut self) -> Result<()> {
        self.queue_bytes(&[], 0, AMEDIACODEC_BUFFER_FLAG_END_OF_STREAM)
    }

    fn queue_bytes(&mut self, data: &[u8], pts_us: u64, flags: u32) -> Result<()> {
        // SAFETY: The codec is configured and started during construction.
        let index = unsafe { AMediaCodec_dequeueInputBuffer(self.ptr, DEQUEUE_TIMEOUT_US) };
        if index < 0 {
            return codec_backend_error(
                &self.codec,
                "queue AMediaCodec input",
                format!("no input buffer available, code {index}"),
            );
        }

        let mut capacity = 0usize;
        // SAFETY: index was returned by dequeueInputBuffer and has not been queued yet.
        let buffer = unsafe { AMediaCodec_getInputBuffer(self.ptr, index as usize, &mut capacity) };
        if buffer.is_null() {
            return codec_backend_error(
                &self.codec,
                "queue AMediaCodec input",
                "AMediaCodec_getInputBuffer returned null",
            );
        }
        if data.len() > capacity {
            return codec_backend_error(
                &self.codec,
                "queue AMediaCodec input",
                format!(
                    "input packet has {} bytes but AMediaCodec buffer holds {capacity}",
                    data.len()
                ),
            );
        }
        // SAFETY: buffer points to a writable input buffer with at least data.len() bytes.
        unsafe {
            ptr::copy_nonoverlapping(data.as_ptr(), buffer, data.len());
        }
        // SAFETY: index remains owned until queueInputBuffer transfers it back to codec.
        let status = unsafe {
            AMediaCodec_queueInputBuffer(self.ptr, index as usize, 0, data.len(), pts_us, flags)
        };
        if status != AMEDIA_OK {
            return media_status_error(&self.codec, "queue AMediaCodec input", status);
        }
        Ok(())
    }

    fn drain_output_bytes(&mut self) -> Result<Vec<OutputBytes>> {
        let mut out = Vec::new();
        loop {
            let mut info = AMediaCodecBufferInfo {
                offset: 0,
                size: 0,
                presentation_time_us: 0,
                flags: 0,
            };
            // SAFETY: info points to writable storage for output metadata.
            let index =
                unsafe { AMediaCodec_dequeueOutputBuffer(self.ptr, &mut info, DEQUEUE_TIMEOUT_US) };
            match index {
                AMEDIACODEC_INFO_TRY_AGAIN_LATER => break,
                AMEDIACODEC_INFO_OUTPUT_BUFFERS_CHANGED
                | AMEDIACODEC_INFO_OUTPUT_FORMAT_CHANGED => continue,
                value if value < 0 => {
                    return codec_backend_error(
                        &self.codec,
                        "drain AMediaCodec output",
                        format!("unexpected output buffer code {value}"),
                    );
                }
                value => {
                    let mut _buffer_size = 0usize;
                    // SAFETY: value was returned by dequeueOutputBuffer and has not been released yet.
                    let buffer = unsafe {
                        AMediaCodec_getOutputBuffer(self.ptr, value as usize, &mut _buffer_size)
                    };
                    if !buffer.is_null() && info.size > 0 {
                        let size = info.size.max(0) as usize;
                        // SAFETY: The NDK documents AMediaCodecBufferInfo.size
                        // as the authoritative output size through API 35 and
                        // AMediaCodec_getOutputBuffer out_size as authoritative
                        // after that. It also says the buffer-info offset must
                        // be ignored through API 35 and is always 0 after that.
                        let bytes = unsafe { slice::from_raw_parts(buffer, size) }.to_vec();
                        out.push(OutputBytes {
                            pts_us: info.presentation_time_us,
                            flags: info.flags,
                            bytes,
                        });
                    }
                    // SAFETY: Release the output buffer back to the codec; no surface render is requested.
                    let status =
                        unsafe { AMediaCodec_releaseOutputBuffer(self.ptr, value as usize, false) };
                    if status != AMEDIA_OK {
                        return media_status_error(
                            &self.codec,
                            "release AMediaCodec output",
                            status,
                        );
                    }
                    if info.flags & AMEDIACODEC_BUFFER_FLAG_END_OF_STREAM != 0 {
                        break;
                    }
                }
            }
        }
        Ok(out)
    }
}

struct OutputBytes {
    pts_us: i64,
    flags: u32,
    bytes: Vec<u8>,
}

fn configure_video_codec(
    handle: &mut CodecHandle,
    width: u32,
    height: u32,
    bitrate: Option<u32>,
    extra_data: Option<&[u8]>,
) -> Result<()> {
    let format = MediaFormatHandle::new(&handle.codec)?;
    format.set_string("mime", android_mime(&handle.codec).unwrap())?;
    format.set_i32("width", i32::try_from(width).unwrap_or(i32::MAX))?;
    format.set_i32("height", i32::try_from(height).unwrap_or(i32::MAX))?;
    format.set_i32("color-format", COLOR_FORMAT_32BIT_ABGR8888)?;
    if let Some(bitrate) = bitrate {
        format.set_i32("bitrate", i32::try_from(bitrate).unwrap_or(i32::MAX))?;
    }
    if let Some(extra_data) = extra_data
        && !extra_data.is_empty()
    {
        set_video_csd(&format, &handle.codec, extra_data)?;
    }
    configure_and_start(handle, &format)
}

fn configure_audio_codec(
    handle: &mut CodecHandle,
    sample_rate: u32,
    channels: u16,
    bitrate: Option<u32>,
    extra_data: Option<&[u8]>,
) -> Result<()> {
    let format = MediaFormatHandle::new(&handle.codec)?;
    format.set_string("mime", android_mime(&handle.codec).unwrap())?;
    format.set_i32(
        "sample-rate",
        i32::try_from(sample_rate).unwrap_or(i32::MAX),
    )?;
    format.set_i32("channel-count", i32::from(channels))?;
    format.set_i32("pcm-encoding", PCM_ENCODING_FLOAT)?;
    if let Some(bitrate) = bitrate {
        format.set_i32("bitrate", i32::try_from(bitrate).unwrap_or(i32::MAX))?;
    }
    if let Some(extra_data) = extra_data {
        format.set_buffer("csd-0", extra_data)?;
    }
    configure_and_start(handle, &format)
}

fn configure_and_start(handle: &mut CodecHandle, format: &MediaFormatHandle) -> Result<()> {
    let flags = match handle.direction {
        CodecDirection::Decode => 0,
        CodecDirection::Encode => AMEDIACODEC_CONFIGURE_FLAG_ENCODE,
    };
    // SAFETY: codec and format are live objects; no rendering surface or crypto is used.
    let status = unsafe {
        AMediaCodec_configure(
            handle.ptr,
            format.ptr,
            ptr::null_mut(),
            ptr::null_mut(),
            flags,
        )
    };
    if status != AMEDIA_OK {
        return media_status_error(&handle.codec, "configure AMediaCodec", status);
    }
    // SAFETY: codec was configured successfully.
    let status = unsafe { AMediaCodec_start(handle.ptr) };
    if status != AMEDIA_OK {
        return media_status_error(&handle.codec, "start AMediaCodec", status);
    }
    Ok(())
}

struct MediaFormatHandle {
    codec: CodecId,
    ptr: *mut AMediaFormat,
}

impl Drop for MediaFormatHandle {
    fn drop(&mut self) {
        // SAFETY: The handle owns the AMediaFormat pointer returned by AMediaFormat_new.
        unsafe {
            if !self.ptr.is_null() {
                AMediaFormat_delete(self.ptr);
            }
        }
    }
}

impl MediaFormatHandle {
    fn new(codec: &CodecId) -> Result<Self> {
        // SAFETY: Creates a new empty media format or null on failure.
        let ptr = unsafe { AMediaFormat_new() };
        if ptr.is_null() {
            return codec_backend_error(
                codec,
                "create AMediaFormat",
                "AMediaFormat_new returned null",
            );
        }
        Ok(Self {
            codec: codec.clone(),
            ptr,
        })
    }

    fn set_string(&self, key: &str, value: &str) -> Result<()> {
        let key = c_string(&self.codec, "set AMediaFormat string", key)?;
        let value = c_string(&self.codec, "set AMediaFormat string", value)?;
        // SAFETY: key and value are valid C strings for the duration of the call.
        unsafe {
            AMediaFormat_setString(self.ptr, key.as_ptr(), value.as_ptr());
        }
        Ok(())
    }

    fn set_i32(&self, key: &str, value: i32) -> Result<()> {
        let key = c_string(&self.codec, "set AMediaFormat int", key)?;
        // SAFETY: key is a valid C string for the duration of the call.
        unsafe {
            AMediaFormat_setInt32(self.ptr, key.as_ptr(), value);
        }
        Ok(())
    }

    fn set_buffer(&self, key: &str, data: &[u8]) -> Result<()> {
        let key = c_string(&self.codec, "set AMediaFormat buffer", key)?;
        // SAFETY: AMediaFormat copies the buffer contents during the call.
        unsafe {
            AMediaFormat_setBuffer(
                self.ptr,
                key.as_ptr(),
                data.as_ptr().cast_mut().cast(),
                data.len(),
            );
        }
        Ok(())
    }
}

fn set_video_csd(format: &MediaFormatHandle, codec: &CodecId, extra_data: &[u8]) -> Result<()> {
    match codec {
        CodecId::H264 => {
            let parameter_sets = avcc_parameter_sets(extra_data)?;
            let sps = annex_b_parameter_sets(&parameter_sets, |nal| {
                !nal.is_empty() && (nal[0] & 0x1f) == 7
            });
            let pps = annex_b_parameter_sets(&parameter_sets, |nal| {
                !nal.is_empty() && (nal[0] & 0x1f) == 8
            });
            if sps.is_empty() || pps.is_empty() {
                return codec_backend_error(
                    codec,
                    "configure AMediaCodec",
                    "H.264 decoder extra_data must contain SPS and PPS parameter sets",
                );
            }
            format.set_buffer("csd-0", &sps)?;
            format.set_buffer("csd-1", &pps)
        }
        CodecId::H265 => {
            let parameter_sets = hvcc_parameter_sets(extra_data)?;
            let csd = annex_b_parameter_sets(&parameter_sets, |nal| {
                nal.len() >= 2 && matches!((nal[0] >> 1) & 0x3f, 32..=34)
            });
            if csd.is_empty() {
                return codec_backend_error(
                    codec,
                    "configure AMediaCodec",
                    "HEVC decoder extra_data must contain VPS, SPS, and PPS parameter sets",
                );
            }
            format.set_buffer("csd-0", &csd)
        }
        _ => format.set_buffer("csd-0", extra_data),
    }
}

fn annex_b_parameter_sets(
    parameter_sets: &[bytes::Bytes],
    keep: impl Fn(&[u8]) -> bool,
) -> Vec<u8> {
    let mut out = Vec::new();
    for nal in parameter_sets {
        if keep(nal) {
            out.extend_from_slice(&[0, 0, 0, 1]);
            out.extend_from_slice(nal);
        }
    }
    out
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

fn rgba_frame_from_android_output(
    output: OutputBytes,
    width: u32,
    height: u32,
    codec: &CodecId,
) -> Result<RgbaFrame> {
    let stride = width as usize * 4;
    let expected = stride * height as usize;
    if output.bytes.len() < expected {
        return codec_backend_error(
            codec,
            "decode AMediaCodec video packet",
            format!(
                "decoded video output has {} bytes, expected at least {expected}; this device may not support direct RGBA ByteBuffer output",
                output.bytes.len()
            ),
        );
    }
    RgbaFrame::new(width, height, stride, output.bytes[..expected].to_vec())
}

fn audio_frame_from_android_output(
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
    let pts = if output.pts_us >= 0 {
        TimeBase::microseconds().rescale(output.pts_us, TimeBase::new(1, sample_rate as i32)?)
    } else {
        fallback_pts
    };
    AudioFrame::new(sample_rate, channels, pts, samples)
}

fn encoded_packets_from_outputs(
    codec: &CodecId,
    outputs: Vec<OutputBytes>,
    time_base: TimeBase,
    fallback_pts: i64,
    fallback_duration: i64,
) -> Vec<EncodedPacket> {
    outputs
        .into_iter()
        .map(|output| {
            let pts = if output.pts_us >= 0 {
                TimeBase::microseconds().rescale(output.pts_us, time_base)
            } else {
                fallback_pts
            };
            EncodedPacket::new(
                DEFAULT_TRACK_ID,
                codec.clone(),
                pts,
                fallback_duration,
                time_base,
                output.bytes,
            )
            .with_keyframe(output.flags & 1 != 0)
        })
        .collect()
}

fn tight_rgba_bytes(frame: &RgbaFrame) -> Vec<u8> {
    let row_bytes = frame.width as usize * 4;
    if frame.stride == row_bytes {
        return frame.data[..row_bytes * frame.height as usize].to_vec();
    }

    let mut out = Vec::with_capacity(row_bytes * frame.height as usize);
    for row in 0..frame.height as usize {
        let offset = row * frame.stride;
        out.extend_from_slice(&frame.data[offset..offset + row_bytes]);
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

fn c_string(codec: &CodecId, operation: &'static str, value: &str) -> Result<CString> {
    CString::new(value).map_err(|error| Error::CodecBackend {
        codec: codec.clone(),
        operation,
        message: error.to_string(),
    })
}

fn media_status_error<T>(
    codec: &CodecId,
    operation: &'static str,
    status: MediaStatus,
) -> Result<T> {
    codec_backend_error(
        codec,
        operation,
        format!("AMediaCodec returned media_status_t {status}"),
    )
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

fn android_mime(codec: &CodecId) -> Option<&'static str> {
    match codec {
        CodecId::H264 => Some("video/avc"),
        CodecId::H265 => Some("video/hevc"),
        CodecId::Aac => Some("audio/mp4a-latm"),
        CodecId::Eac3 => Some("audio/eac3"),
        CodecId::Dts => Some("audio/vnd.dts"),
        _ => None,
    }
}
