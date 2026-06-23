use super::{
    PlatformAudioDecoderConfig, PlatformAudioEncoderConfig, PlatformCodecProbe,
    PlatformVideoDecoderConfig, PlatformVideoEncoderConfig,
};
use crate::{
    audio::AudioFrame,
    backend::BackendKind,
    bitstream::{
        aac::aac_packet_to_adts, h264::h264_packet_to_annex_b, h265::h265_packet_to_annex_b,
    },
    codec::{CodecDirection, CodecId},
    error::{Error, Result},
    frame::RgbaFrame,
    packet::EncodedPacket,
    time::TimeBase,
};
use bytes::Bytes;
use std::{
    collections::VecDeque,
    ffi::{CStr, CString, c_char, c_int, c_void},
    ptr, slice,
    sync::OnceLock,
};

const RTLD_NOW: c_int = 2;
const GST_STATE_NULL: c_int = 1;
const GST_STATE_PLAYING: c_int = 4;
const GST_STATE_CHANGE_FAILURE: c_int = 0;
const GST_FLOW_OK: c_int = 0;
const GST_MAP_READ: c_int = 1;
const GST_SECOND: u64 = 1_000_000_000;
const PULL_TIMEOUT: u64 = 5 * GST_SECOND;
const LIVE_PULL_TIMEOUT: u64 = 250_000_000;
const DEFAULT_TRACK_ID: u32 = 1;

#[link(name = "dl")]
unsafe extern "C" {
    fn dlopen(filename: *const c_char, flag: c_int) -> *mut c_void;
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
    fn dlclose(handle: *mut c_void) -> c_int;
    fn dlerror() -> *const c_char;
}

#[repr(C)]
struct GError {
    domain: u32,
    code: c_int,
    message: *mut c_char,
}

#[repr(C)]
struct GstMapInfo {
    memory: *mut c_void,
    flags: c_int,
    data: *mut u8,
    size: usize,
    maxsize: usize,
    user_data: [*mut c_void; 4],
    reserved: [*mut c_void; 4],
}

type GErrorFree = unsafe extern "C" fn(*mut GError);
type GstInitCheck =
    unsafe extern "C" fn(*mut c_int, *mut *mut *mut c_char, *mut *mut GError) -> c_int;
type GstElementFactoryFind = unsafe extern "C" fn(*const c_char) -> *mut c_void;
type GstParseLaunch = unsafe extern "C" fn(*const c_char, *mut *mut GError) -> *mut c_void;
type GstElementSetState = unsafe extern "C" fn(*mut c_void, c_int) -> c_int;
type GstBinGetByName = unsafe extern "C" fn(*mut c_void, *const c_char) -> *mut c_void;
type GstObjectUnref = unsafe extern "C" fn(*mut c_void);
type GstMiniObjectUnref = unsafe extern "C" fn(*mut c_void);
type GstBufferNewAllocate = unsafe extern "C" fn(*mut c_void, usize, *mut c_void) -> *mut c_void;
type GstBufferFill = unsafe extern "C" fn(*mut c_void, usize, *const c_void, usize) -> usize;
type GstBufferMap = unsafe extern "C" fn(*mut c_void, *mut GstMapInfo, c_int) -> c_int;
type GstBufferUnmap = unsafe extern "C" fn(*mut c_void, *mut GstMapInfo);
type GstSampleGetBuffer = unsafe extern "C" fn(*mut c_void) -> *mut c_void;
type GstSampleGetCaps = unsafe extern "C" fn(*mut c_void) -> *mut c_void;
type GstSampleUnref = unsafe extern "C" fn(*mut c_void);
type GstCapsGetStructure = unsafe extern "C" fn(*mut c_void, u32) -> *mut c_void;
type GstStructureGetInt = unsafe extern "C" fn(*mut c_void, *const c_char, *mut c_int) -> c_int;
type GstAppSrcPushBuffer = unsafe extern "C" fn(*mut c_void, *mut c_void) -> c_int;
type GstAppSrcEndOfStream = unsafe extern "C" fn(*mut c_void) -> c_int;
type GstAppSinkTryPullSample = unsafe extern "C" fn(*mut c_void, u64) -> *mut c_void;

#[derive(Clone, Copy)]
struct GstSymbols {
    element_factory_find: GstElementFactoryFind,
    parse_launch: GstParseLaunch,
    element_set_state: GstElementSetState,
    bin_get_by_name: GstBinGetByName,
    object_unref: GstObjectUnref,
    mini_object_unref: GstMiniObjectUnref,
    buffer_new_allocate: GstBufferNewAllocate,
    buffer_fill: GstBufferFill,
    buffer_map: GstBufferMap,
    buffer_unmap: GstBufferUnmap,
    sample_get_buffer: GstSampleGetBuffer,
    sample_get_caps: GstSampleGetCaps,
    sample_unref: GstSampleUnref,
    caps_get_structure: GstCapsGetStructure,
    structure_get_int: GstStructureGetInt,
}

#[derive(Clone, Copy)]
struct GstAppSymbols {
    app_src_push_buffer: GstAppSrcPushBuffer,
    app_src_end_of_stream: GstAppSrcEndOfStream,
    app_sink_try_pull_sample: GstAppSinkTryPullSample,
}

struct GstLibrary {
    _gst_handle: *mut c_void,
    _app_handle: *mut c_void,
    _glib_handle: *mut c_void,
    g_error_free: GErrorFree,
    gst: GstSymbols,
    app: GstAppSymbols,
}

// SAFETY: The loaded handles and function pointers are immutable after
// initialization. GStreamer itself owns the synchronization for its global
// runtime and element operations.
unsafe impl Send for GstLibrary {}
unsafe impl Sync for GstLibrary {}

enum ElementRole {
    VideoDecoder {
        width: u32,
        height: u32,
        extra_data: Option<Bytes>,
        session: Option<RunningPipeline>,
    },
    VideoEncoder {
        width: u32,
        height: u32,
        time_base: TimeBase,
        frame_duration: i64,
        factory: &'static str,
        session: Option<RunningPipeline>,
        pending_pts: VecDeque<i64>,
    },
    AudioDecoder {
        sample_rate: u32,
        channels: u16,
        extra_data: Option<Bytes>,
        session: Option<RunningPipeline>,
        pending_pts: VecDeque<i64>,
    },
    AudioEncoder {
        sample_rate: u32,
        channels: u16,
        factory: &'static str,
        session: Option<RunningPipeline>,
        pending_ranges: VecDeque<(i64, i64, TimeBase)>,
    },
}

pub struct ElementHandle {
    codec: CodecId,
    direction: CodecDirection,
    role: ElementRole,
    library: &'static GstLibrary,
}

struct RunningPipeline {
    library: &'static GstLibrary,
    codec: CodecId,
    operation: &'static str,
    pipeline: *mut c_void,
    src: *mut c_void,
    sink: *mut c_void,
}

struct RawSample {
    data: Vec<u8>,
    width: Option<u32>,
    height: Option<u32>,
    sample_rate: Option<u32>,
    channels: Option<u16>,
}

impl Drop for RunningPipeline {
    fn drop(&mut self) {
        // SAFETY: Pipeline and child references are owned by this runner.
        unsafe {
            if !self.pipeline.is_null() {
                (self.library.gst.element_set_state)(self.pipeline, GST_STATE_NULL);
            }
            if !self.src.is_null() {
                (self.library.gst.object_unref)(self.src);
            }
            if !self.sink.is_null() {
                (self.library.gst.object_unref)(self.sink);
            }
            if !self.pipeline.is_null() {
                (self.library.gst.object_unref)(self.pipeline);
            }
        }
    }
}

pub fn probe(codec: &CodecId, direction: CodecDirection) -> PlatformCodecProbe {
    let result = probe_factories(codec, direction);
    PlatformCodecProbe {
        backend: Some(BackendKind::GStreamer),
        codec: codec.clone(),
        direction,
        supported: result.is_ok(),
        detail: result.unwrap_or_else(|message| message),
    }
}

pub fn open_video_decoder(config: &PlatformVideoDecoderConfig) -> Result<ElementHandle> {
    let library = open_checked_library(
        &config.codec,
        CodecDirection::Decode,
        "open GStreamer video decoder",
    )?;
    let _ = select_installed_factory(
        library,
        &config.codec,
        CodecDirection::Decode,
        "open GStreamer video decoder",
    )?;

    Ok(ElementHandle {
        codec: config.codec.clone(),
        direction: CodecDirection::Decode,
        role: ElementRole::VideoDecoder {
            width: config.width,
            height: config.height,
            extra_data: optional_bytes(&config.extra_data),
            session: None,
        },
        library,
    })
}

pub fn open_video_encoder(config: &PlatformVideoEncoderConfig) -> Result<ElementHandle> {
    let library = open_checked_library(
        &config.codec,
        CodecDirection::Encode,
        "open GStreamer video encoder",
    )?;
    let factory = select_installed_factory(
        library,
        &config.codec,
        CodecDirection::Encode,
        "open GStreamer video encoder",
    )?;

    Ok(ElementHandle {
        codec: config.codec.clone(),
        direction: CodecDirection::Encode,
        role: ElementRole::VideoEncoder {
            width: config.width,
            height: config.height,
            time_base: config.time_base,
            frame_duration: config.frame_duration,
            factory,
            session: None,
            pending_pts: VecDeque::new(),
        },
        library,
    })
}

pub fn open_audio_decoder(config: &PlatformAudioDecoderConfig) -> Result<ElementHandle> {
    let library = open_checked_library(
        &config.codec,
        CodecDirection::Decode,
        "open GStreamer audio decoder",
    )?;
    let _ = select_installed_factory(
        library,
        &config.codec,
        CodecDirection::Decode,
        "open GStreamer audio decoder",
    )?;

    Ok(ElementHandle {
        codec: config.codec.clone(),
        direction: CodecDirection::Decode,
        role: ElementRole::AudioDecoder {
            sample_rate: config.sample_rate,
            channels: config.channels,
            extra_data: optional_bytes(&config.extra_data),
            session: None,
            pending_pts: VecDeque::new(),
        },
        library,
    })
}

pub fn open_audio_encoder(config: &PlatformAudioEncoderConfig) -> Result<ElementHandle> {
    let library = open_checked_library(
        &config.codec,
        CodecDirection::Encode,
        "open GStreamer audio encoder",
    )?;
    let factory = select_installed_factory(
        library,
        &config.codec,
        CodecDirection::Encode,
        "open GStreamer audio encoder",
    )?;

    Ok(ElementHandle {
        codec: config.codec.clone(),
        direction: CodecDirection::Encode,
        role: ElementRole::AudioEncoder {
            sample_rate: config.sample_rate,
            channels: config.channels,
            factory,
            session: None,
            pending_ranges: VecDeque::new(),
        },
        library,
    })
}

impl ElementHandle {
    pub fn decode_video_packet(&mut self, packet: &EncodedPacket) -> Result<Vec<RgbaFrame>> {
        debug_assert_eq!(self.direction, CodecDirection::Decode);
        let codec = self.codec.clone();
        let library = self.library;
        let ElementRole::VideoDecoder {
            width,
            height,
            extra_data,
            session,
        } = &mut self.role
        else {
            return codec_backend_error(
                &codec,
                "decode GStreamer video packet",
                "handle is not a video decoder",
            );
        };

        let input = encoded_video_input(&codec, packet, extra_data.as_ref())?;
        let width = *width;
        let height = *height;
        let session = ensure_session(
            session,
            library,
            codec.clone(),
            "decode GStreamer video packet",
            || video_decode_pipeline(&codec),
        )?;
        let samples = session.push_and_pull(&input, LIVE_PULL_TIMEOUT)?;
        samples
            .into_iter()
            .map(|sample| raw_sample_to_rgba_frame(sample, width, height, &codec))
            .collect()
    }

    pub fn encode_video_frame(
        &mut self,
        codec: &CodecId,
        frame: &RgbaFrame,
        pts: i64,
    ) -> Result<Vec<EncodedPacket>> {
        debug_assert_eq!(self.direction, CodecDirection::Encode);
        let library = self.library;
        let ElementRole::VideoEncoder {
            width,
            height,
            time_base,
            frame_duration,
            factory,
            session,
            pending_pts,
        } = &mut self.role
        else {
            return codec_backend_error(
                codec,
                "encode GStreamer video frame",
                "handle is not a video encoder",
            );
        };
        if frame.width != *width || frame.height != *height {
            return codec_backend_error(
                codec,
                "encode GStreamer video frame",
                format!(
                    "frame dimensions {}x{} do not match encoder config {}x{}",
                    frame.width, frame.height, width, height
                ),
            );
        }

        let input = tight_rgba_bytes(frame);
        let time_base = *time_base;
        let frame_duration = *frame_duration;
        let width = *width;
        let height = *height;
        let factory = *factory;
        let session = ensure_session(
            session,
            library,
            codec.clone(),
            "encode GStreamer video frame",
            || video_encode_pipeline(codec, width, height, time_base, frame_duration, factory),
        )?;
        pending_pts.push_back(pts);
        let samples = session.push_and_pull(&input, LIVE_PULL_TIMEOUT)?;
        Ok(video_packets_from_samples(
            codec,
            samples,
            pending_pts,
            time_base,
            frame_duration,
            pts,
        ))
    }

    pub fn decode_audio_packet(&mut self, packet: &EncodedPacket) -> Result<Vec<AudioFrame>> {
        debug_assert_eq!(self.direction, CodecDirection::Decode);
        let codec = self.codec.clone();
        let library = self.library;
        let ElementRole::AudioDecoder {
            sample_rate,
            channels,
            extra_data,
            session,
            pending_pts,
        } = &mut self.role
        else {
            return codec_backend_error(
                &codec,
                "decode GStreamer audio packet",
                "handle is not an audio decoder",
            );
        };

        let input = encoded_audio_input(&codec, packet, extra_data.as_ref())?;
        let sample_rate = *sample_rate;
        let channels = *channels;
        let session = ensure_session(
            session,
            library,
            codec.clone(),
            "decode GStreamer audio packet",
            || audio_decode_pipeline(&codec),
        )?;
        pending_pts.push_back(packet.pts);
        let samples = session.push_and_pull(&input, LIVE_PULL_TIMEOUT)?;
        audio_frames_from_samples(
            samples,
            sample_rate,
            channels,
            packet.time_base,
            pending_pts,
            packet.pts,
            &codec,
        )
    }

    pub fn encode_audio_frame(
        &mut self,
        codec: &CodecId,
        frame: &AudioFrame,
    ) -> Result<Vec<EncodedPacket>> {
        debug_assert_eq!(self.direction, CodecDirection::Encode);
        let library = self.library;
        let ElementRole::AudioEncoder {
            sample_rate,
            channels,
            factory,
            session,
            pending_ranges,
        } = &mut self.role
        else {
            return codec_backend_error(
                codec,
                "encode GStreamer audio frame",
                "handle is not an audio encoder",
            );
        };
        if frame.sample_rate != *sample_rate || frame.channels != *channels {
            return codec_backend_error(
                codec,
                "encode GStreamer audio frame",
                format!(
                    "audio frame format {} Hz/{} channels does not match encoder config {} Hz/{} channels",
                    frame.sample_rate, frame.channels, sample_rate, channels
                ),
            );
        }

        let input = f32le_audio_bytes(&frame.samples_f32_interleaved);
        let time_base = TimeBase::new(
            1,
            i32_from_u32(frame.sample_rate, codec, "encode GStreamer audio frame")?,
        )?;
        let duration = frame.sample_frames() as i64;
        let sample_rate = *sample_rate;
        let channels = *channels;
        let factory = *factory;
        let session = ensure_session(
            session,
            library,
            codec.clone(),
            "encode GStreamer audio frame",
            || audio_encode_pipeline(codec, sample_rate, channels, factory),
        )?;
        pending_ranges.push_back((frame.pts, duration, time_base));
        let samples = session.push_and_pull(&input, LIVE_PULL_TIMEOUT)?;
        Ok(audio_packets_from_samples(
            codec,
            samples,
            pending_ranges,
            frame.pts,
            duration,
            time_base,
        ))
    }

    pub fn flush_video_decoder(&mut self) -> Result<Vec<RgbaFrame>> {
        let codec = self.codec.clone();
        let ElementRole::VideoDecoder {
            width,
            height,
            session,
            ..
        } = &mut self.role
        else {
            return Ok(Vec::new());
        };
        let Some(session) = session.take() else {
            return Ok(Vec::new());
        };
        session.end_of_stream()?;
        let samples = session.pull_samples_timeout(PULL_TIMEOUT)?;
        samples
            .into_iter()
            .map(|sample| raw_sample_to_rgba_frame(sample, *width, *height, &codec))
            .collect()
    }

    pub fn finish_video_encoder(&mut self, codec: &CodecId) -> Result<Vec<EncodedPacket>> {
        let ElementRole::VideoEncoder {
            time_base,
            frame_duration,
            session,
            pending_pts,
            ..
        } = &mut self.role
        else {
            return Ok(Vec::new());
        };
        let Some(session) = session.take() else {
            return Ok(Vec::new());
        };
        session.end_of_stream()?;
        let samples = session.pull_samples_timeout(PULL_TIMEOUT)?;
        Ok(video_packets_from_samples(
            codec,
            samples,
            pending_pts,
            *time_base,
            *frame_duration,
            0,
        ))
    }

    pub fn flush_audio_decoder(&mut self) -> Result<Vec<AudioFrame>> {
        let codec = self.codec.clone();
        let ElementRole::AudioDecoder {
            sample_rate,
            channels,
            session,
            pending_pts,
            ..
        } = &mut self.role
        else {
            return Ok(Vec::new());
        };
        let Some(session) = session.take() else {
            return Ok(Vec::new());
        };
        session.end_of_stream()?;
        let samples = session.pull_samples_timeout(PULL_TIMEOUT)?;
        let time_base = TimeBase::new(
            1,
            i32_from_u32(*sample_rate, &codec, "flush GStreamer audio decoder")?,
        )?;
        audio_frames_from_samples(
            samples,
            *sample_rate,
            *channels,
            time_base,
            pending_pts,
            0,
            &codec,
        )
    }

    pub fn finish_audio_encoder(&mut self, codec: &CodecId) -> Result<Vec<EncodedPacket>> {
        let ElementRole::AudioEncoder {
            session,
            pending_ranges,
            ..
        } = &mut self.role
        else {
            return Ok(Vec::new());
        };
        let Some(session) = session.take() else {
            return Ok(Vec::new());
        };
        session.end_of_stream()?;
        let samples = session.pull_samples_timeout(PULL_TIMEOUT)?;
        let fallback_time_base = TimeBase::milliseconds();
        Ok(audio_packets_from_samples(
            codec,
            samples,
            pending_ranges,
            0,
            0,
            fallback_time_base,
        ))
    }
}

fn ensure_session<'a, F>(
    slot: &'a mut Option<RunningPipeline>,
    library: &'static GstLibrary,
    codec: CodecId,
    operation: &'static str,
    build_pipeline: F,
) -> Result<&'a RunningPipeline>
where
    F: FnOnce() -> Result<String>,
{
    if slot.is_none() {
        let pipeline = build_pipeline()?;
        *slot = Some(RunningPipeline::new(library, codec, operation, &pipeline)?);
    }
    Ok(slot.as_ref().expect("session was just initialized"))
}

fn video_packets_from_samples(
    codec: &CodecId,
    samples: Vec<RawSample>,
    pending_pts: &mut VecDeque<i64>,
    time_base: TimeBase,
    frame_duration: i64,
    fallback_pts: i64,
) -> Vec<EncodedPacket> {
    let mut next_fallback_pts = fallback_pts;
    samples
        .into_iter()
        .map(|sample| {
            let pts = pending_pts.pop_front().unwrap_or(next_fallback_pts);
            let duration = frame_duration;
            next_fallback_pts = pts + duration;
            EncodedPacket::new(
                DEFAULT_TRACK_ID,
                codec.clone(),
                pts,
                duration,
                time_base,
                sample.data,
            )
            .with_keyframe(true)
        })
        .collect()
}

fn audio_frames_from_samples(
    samples: Vec<RawSample>,
    fallback_sample_rate: u32,
    fallback_channels: u16,
    input_time_base: TimeBase,
    pending_pts: &mut VecDeque<i64>,
    fallback_pts: i64,
    codec: &CodecId,
) -> Result<Vec<AudioFrame>> {
    samples
        .into_iter()
        .map(|sample| {
            let pts = pending_pts.pop_front().unwrap_or(fallback_pts);
            raw_sample_to_audio_frame(
                sample,
                fallback_sample_rate,
                fallback_channels,
                input_time_base,
                pts,
                codec,
            )
        })
        .collect()
}

fn audio_packets_from_samples(
    codec: &CodecId,
    samples: Vec<RawSample>,
    pending_ranges: &mut VecDeque<(i64, i64, TimeBase)>,
    fallback_pts: i64,
    fallback_duration: i64,
    fallback_time_base: TimeBase,
) -> Vec<EncodedPacket> {
    samples
        .into_iter()
        .map(|sample| {
            let (pts, duration, time_base) = pending_ranges.pop_front().unwrap_or((
                fallback_pts,
                fallback_duration,
                fallback_time_base,
            ));
            EncodedPacket::new(
                DEFAULT_TRACK_ID,
                codec.clone(),
                pts,
                duration,
                time_base,
                sample.data,
            )
        })
        .collect()
}

impl RunningPipeline {
    fn new(
        library: &'static GstLibrary,
        codec: CodecId,
        operation: &'static str,
        description: &str,
    ) -> Result<Self> {
        let description = CString::new(description).map_err(|_| Error::CodecBackend {
            codec: codec.clone(),
            operation,
            message: "GStreamer pipeline description contains an interior NUL byte".to_owned(),
        })?;
        let mut error: *mut GError = ptr::null_mut();
        // SAFETY: GStreamer is initialized and description is a valid C string.
        let pipeline = unsafe { (library.gst.parse_launch)(description.as_ptr(), &mut error) };
        if !error.is_null() {
            let message = take_g_error(&library, error);
            // SAFETY: A non-null partial pipeline must be unreffed on parse errors.
            unsafe {
                if !pipeline.is_null() {
                    (library.gst.object_unref)(pipeline);
                }
            }
            return codec_backend_error(
                &codec,
                operation,
                format!("gst_parse_launch failed: {message}"),
            );
        }
        if pipeline.is_null() {
            return codec_backend_error(&codec, operation, "gst_parse_launch returned null");
        }

        let src_name = c_string("src");
        let sink_name = c_string("sink");
        // SAFETY: pipeline is a GstPipeline/GstBin returned by gst_parse_launch.
        let src = unsafe { (library.gst.bin_get_by_name)(pipeline, src_name.as_ptr()) };
        // SAFETY: pipeline is a GstPipeline/GstBin returned by gst_parse_launch.
        let sink = unsafe { (library.gst.bin_get_by_name)(pipeline, sink_name.as_ptr()) };
        if src.is_null() || sink.is_null() {
            unref_pipeline_parts(&library, pipeline, src, sink);
            return codec_backend_error(
                &codec,
                operation,
                "GStreamer pipeline is missing appsrc/appsink",
            );
        }

        // SAFETY: pipeline is valid and owned by this runner.
        let state = unsafe { (library.gst.element_set_state)(pipeline, GST_STATE_PLAYING) };
        if state == GST_STATE_CHANGE_FAILURE {
            unref_pipeline_parts(&library, pipeline, src, sink);
            return codec_backend_error(
                &codec,
                operation,
                "failed to set GStreamer pipeline to PLAYING",
            );
        }

        Ok(Self {
            library,
            codec,
            operation,
            pipeline,
            src,
            sink,
        })
    }

    fn push_and_pull(&self, data: &[u8], timeout_ns: u64) -> Result<Vec<RawSample>> {
        self.push_buffer(data)?;
        self.pull_samples_timeout(timeout_ns)
    }

    fn push_buffer(&self, data: &[u8]) -> Result<()> {
        // SAFETY: The allocation request is valid; GStreamer owns the returned buffer.
        let buffer = unsafe {
            (self.library.gst.buffer_new_allocate)(ptr::null_mut(), data.len(), ptr::null_mut())
        };
        if buffer.is_null() {
            return codec_backend_error(
                &self.codec,
                self.operation,
                "gst_buffer_new_allocate returned null",
            );
        }

        // SAFETY: buffer is valid and data points to data.len() initialized bytes.
        let written =
            unsafe { (self.library.gst.buffer_fill)(buffer, 0, data.as_ptr().cast(), data.len()) };
        if written != data.len() {
            // SAFETY: appsrc has not taken ownership yet, so we still own the GstBuffer.
            unsafe {
                (self.library.gst.mini_object_unref)(buffer);
            }
            return codec_backend_error(
                &self.codec,
                self.operation,
                format!("gst_buffer_fill wrote {written} of {} bytes", data.len()),
            );
        }

        // SAFETY: appsrc is valid and takes ownership of the buffer.
        let flow = unsafe { (self.library.app.app_src_push_buffer)(self.src, buffer) };
        if flow != GST_FLOW_OK {
            return codec_backend_error(
                &self.codec,
                self.operation,
                format!("gst_app_src_push_buffer returned flow code {flow}"),
            );
        }
        Ok(())
    }

    fn end_of_stream(&self) -> Result<()> {
        // SAFETY: appsrc is valid for this pipeline.
        let flow = unsafe { (self.library.app.app_src_end_of_stream)(self.src) };
        if flow != GST_FLOW_OK {
            return codec_backend_error(
                &self.codec,
                self.operation,
                format!("gst_app_src_end_of_stream returned flow code {flow}"),
            );
        }
        Ok(())
    }

    fn pull_samples_timeout(&self, timeout_ns: u64) -> Result<Vec<RawSample>> {
        let mut samples = Vec::new();
        loop {
            // SAFETY: appsink is valid for this pipeline. Null means timeout or EOS.
            let sample =
                unsafe { (self.library.app.app_sink_try_pull_sample)(self.sink, timeout_ns) };
            if sample.is_null() {
                break;
            }
            let copied = self.copy_sample(sample);
            // SAFETY: sample is owned by the caller of gst_app_sink_try_pull_sample.
            unsafe {
                (self.library.gst.sample_unref)(sample);
            }
            samples.push(copied?);
        }
        Ok(samples)
    }

    fn copy_sample(&self, sample: *mut c_void) -> Result<RawSample> {
        // SAFETY: sample is valid for the duration of this function.
        let buffer = unsafe { (self.library.gst.sample_get_buffer)(sample) };
        if buffer.is_null() {
            return codec_backend_error(
                &self.codec,
                self.operation,
                "GStreamer sample has no buffer",
            );
        }
        // SAFETY: sample is valid; null caps are handled below.
        let caps = unsafe { (self.library.gst.sample_get_caps)(sample) };
        let width = self.sample_int(caps, "width").and_then(nonnegative_u32);
        let height = self.sample_int(caps, "height").and_then(nonnegative_u32);
        let sample_rate = self.sample_int(caps, "rate").and_then(nonnegative_u32);
        let channels = self
            .sample_int(caps, "channels")
            .and_then(nonnegative_u32)
            .and_then(|value| u16::try_from(value).ok());

        let mut map = GstMapInfo {
            memory: ptr::null_mut(),
            flags: 0,
            data: ptr::null_mut(),
            size: 0,
            maxsize: 0,
            user_data: [ptr::null_mut(); 4],
            reserved: [ptr::null_mut(); 4],
        };
        // SAFETY: buffer is valid and map points to writable storage.
        let ok = unsafe { (self.library.gst.buffer_map)(buffer, &mut map, GST_MAP_READ) };
        if ok == 0 {
            return codec_backend_error(&self.codec, self.operation, "gst_buffer_map failed");
        }

        let data = if map.data.is_null() || map.size == 0 {
            Vec::new()
        } else {
            // SAFETY: gst_buffer_map provides a valid readable byte region until unmap.
            unsafe { slice::from_raw_parts(map.data.cast_const(), map.size).to_vec() }
        };
        // SAFETY: map was initialized by gst_buffer_map for this buffer.
        unsafe {
            (self.library.gst.buffer_unmap)(buffer, &mut map);
        }

        Ok(RawSample {
            data,
            width,
            height,
            sample_rate,
            channels,
        })
    }

    fn sample_int(&self, caps: *mut c_void, field: &str) -> Option<c_int> {
        if caps.is_null() {
            return None;
        }
        // SAFETY: caps is valid and index 0 is the first caps structure when present.
        let structure = unsafe { (self.library.gst.caps_get_structure)(caps, 0) };
        if structure.is_null() {
            return None;
        }
        let field = c_string(field);
        let mut value = 0;
        // SAFETY: structure and field name are valid.
        let ok =
            unsafe { (self.library.gst.structure_get_int)(structure, field.as_ptr(), &mut value) };
        (ok != 0).then_some(value)
    }
}

fn probe_factories(
    codec: &CodecId,
    direction: CodecDirection,
) -> std::result::Result<String, String> {
    let factories = factories_for(codec, direction)
        .ok_or_else(|| "codec has no GStreamer factory mapping".to_owned())?;
    let library = open_library()?;

    let mut installed = Vec::new();
    for factory in factories {
        if factory_is_installed(&library, factory) {
            installed.push(*factory);
        }
    }

    if installed.is_empty() {
        Err(format!(
            "GStreamer is available, but none of the mapped factories are installed: {}",
            factories.join(", ")
        ))
    } else {
        Ok(format!(
            "GStreamer factories available: {}; appsrc/appsink pipeline plumbing available",
            installed.join(", ")
        ))
    }
}

fn open_checked_library(
    codec: &CodecId,
    direction: CodecDirection,
    operation: &'static str,
) -> Result<&'static GstLibrary> {
    factories_for(codec, direction).ok_or_else(|| Error::CodecBackend {
        codec: codec.clone(),
        operation,
        message: "codec has no GStreamer factory mapping".to_owned(),
    })?;
    open_library().map_err(|message| Error::CodecBackend {
        codec: codec.clone(),
        operation,
        message,
    })
}

fn select_installed_factory(
    library: &GstLibrary,
    codec: &CodecId,
    direction: CodecDirection,
    operation: &'static str,
) -> Result<&'static str> {
    let factories = factories_for(codec, direction).ok_or_else(|| Error::CodecBackend {
        codec: codec.clone(),
        operation,
        message: "codec has no GStreamer factory mapping".to_owned(),
    })?;

    factories
        .iter()
        .copied()
        .find(|factory| factory_is_installed(library, factory))
        .ok_or_else(|| Error::CodecBackend {
            codec: codec.clone(),
            operation,
            message: format!(
                "none of the mapped GStreamer factories are installed: {}",
                factories.join(", ")
            ),
        })
}

fn factory_is_installed(library: &GstLibrary, factory: &str) -> bool {
    let factory_name = c_string(factory);
    // SAFETY: GStreamer is initialized and factory_name is a valid C string.
    let factory_ptr = unsafe { (library.gst.element_factory_find)(factory_name.as_ptr()) };
    if factory_ptr.is_null() {
        false
    } else {
        // SAFETY: factory_ptr is returned with a reference.
        unsafe {
            (library.gst.object_unref)(factory_ptr);
        }
        true
    }
}

fn open_library() -> std::result::Result<&'static GstLibrary, String> {
    static GST_LIBRARY: OnceLock<std::result::Result<GstLibrary, String>> = OnceLock::new();

    match GST_LIBRARY.get_or_init(open_library_once) {
        Ok(library) => Ok(library),
        Err(message) => Err(message.clone()),
    }
}

fn open_library_once() -> std::result::Result<GstLibrary, String> {
    let glib_handle = open_dynamic_library("libglib-2.0.so.0")?;
    let gst_handle = match open_dynamic_library("libgstreamer-1.0.so.0") {
        Ok(handle) => handle,
        Err(error) => {
            close_dynamic_library(glib_handle);
            return Err(error);
        }
    };
    let app_handle = match open_dynamic_library("libgstapp-1.0.so.0") {
        Ok(handle) => handle,
        Err(error) => {
            close_dynamic_library(gst_handle);
            close_dynamic_library(glib_handle);
            return Err(error);
        }
    };

    // SAFETY: The handles are valid. Each symbol is required for the appsrc/appsink path.
    let loaded = unsafe { load_library_symbols(glib_handle, gst_handle, app_handle) };
    let (g_error_free, gst, app, init_check) = match loaded {
        Ok(symbols) => symbols,
        Err(error) => {
            close_dynamic_library(app_handle);
            close_dynamic_library(gst_handle);
            close_dynamic_library(glib_handle);
            return Err(error);
        }
    };

    let mut argc = 0;
    let mut argv: *mut *mut c_char = ptr::null_mut();
    let mut error: *mut GError = ptr::null_mut();
    // SAFETY: GStreamer permits empty argc/argv and reports initialization errors via GError.
    let ok = unsafe { init_check(&mut argc, &mut argv, &mut error) };
    if ok == 0 {
        let message = if error.is_null() {
            "gst_init_check failed".to_owned()
        } else {
            take_g_error_parts(g_error_free, error)
        };
        close_dynamic_library(app_handle);
        close_dynamic_library(gst_handle);
        close_dynamic_library(glib_handle);
        return Err(message);
    }

    Ok(GstLibrary {
        _gst_handle: gst_handle,
        _app_handle: app_handle,
        _glib_handle: glib_handle,
        g_error_free,
        gst,
        app,
    })
}

type LoadedSymbols = (GErrorFree, GstSymbols, GstAppSymbols, GstInitCheck);

unsafe fn load_library_symbols(
    glib_handle: *mut c_void,
    gst_handle: *mut c_void,
    app_handle: *mut c_void,
) -> std::result::Result<LoadedSymbols, String> {
    let g_error_free = unsafe { load_symbol(glib_handle, b"g_error_free\0") }?;
    let init_check = unsafe { load_symbol(gst_handle, b"gst_init_check\0") }?;
    let gst = GstSymbols {
        element_factory_find: unsafe { load_symbol(gst_handle, b"gst_element_factory_find\0") }?,
        parse_launch: unsafe { load_symbol(gst_handle, b"gst_parse_launch\0") }?,
        element_set_state: unsafe { load_symbol(gst_handle, b"gst_element_set_state\0") }?,
        bin_get_by_name: unsafe { load_symbol(gst_handle, b"gst_bin_get_by_name\0") }?,
        object_unref: unsafe { load_symbol(gst_handle, b"gst_object_unref\0") }?,
        mini_object_unref: unsafe { load_symbol(gst_handle, b"gst_mini_object_unref\0") }?,
        buffer_new_allocate: unsafe { load_symbol(gst_handle, b"gst_buffer_new_allocate\0") }?,
        buffer_fill: unsafe { load_symbol(gst_handle, b"gst_buffer_fill\0") }?,
        buffer_map: unsafe { load_symbol(gst_handle, b"gst_buffer_map\0") }?,
        buffer_unmap: unsafe { load_symbol(gst_handle, b"gst_buffer_unmap\0") }?,
        sample_get_buffer: unsafe { load_symbol(gst_handle, b"gst_sample_get_buffer\0") }?,
        sample_get_caps: unsafe { load_symbol(gst_handle, b"gst_sample_get_caps\0") }?,
        sample_unref: unsafe { load_symbol(gst_handle, b"gst_sample_unref\0") }?,
        caps_get_structure: unsafe { load_symbol(gst_handle, b"gst_caps_get_structure\0") }?,
        structure_get_int: unsafe { load_symbol(gst_handle, b"gst_structure_get_int\0") }?,
    };
    let app = GstAppSymbols {
        app_src_push_buffer: unsafe { load_symbol(app_handle, b"gst_app_src_push_buffer\0") }?,
        app_src_end_of_stream: unsafe { load_symbol(app_handle, b"gst_app_src_end_of_stream\0") }?,
        app_sink_try_pull_sample: unsafe {
            load_symbol(app_handle, b"gst_app_sink_try_pull_sample\0")
        }?,
    };
    Ok((g_error_free, gst, app, init_check))
}

fn open_dynamic_library(name: &str) -> std::result::Result<*mut c_void, String> {
    let name = c_string(name);
    // SAFETY: name is a valid C string; dlopen returns an owned library handle.
    let handle = unsafe { dlopen(name.as_ptr(), RTLD_NOW) };
    if handle.is_null() {
        Err(format!(
            "dlopen({}) failed: {}",
            name.to_string_lossy(),
            dl_error()
        ))
    } else {
        Ok(handle)
    }
}

fn close_dynamic_library(handle: *mut c_void) {
    // SAFETY: handle is either null or owned by the caller.
    unsafe {
        if !handle.is_null() {
            let _ = dlclose(handle);
        }
    }
}

unsafe fn load_symbol<T: Copy>(
    handle: *mut c_void,
    name: &'static [u8],
) -> std::result::Result<T, String> {
    let ptr = unsafe { dlsym(handle, name.as_ptr().cast()) };
    if ptr.is_null() {
        return Err(format!(
            "dlsym({}) failed: {}",
            String::from_utf8_lossy(&name[..name.len() - 1]),
            dl_error()
        ));
    }
    Ok(unsafe { std::mem::transmute_copy(&ptr) })
}

fn encoded_video_input(
    codec: &CodecId,
    packet: &EncodedPacket,
    extra_data: Option<&Bytes>,
) -> Result<Bytes> {
    match codec {
        CodecId::H264 => h264_packet_to_annex_b(packet, extra_data),
        CodecId::H265 => h265_packet_to_annex_b(packet, extra_data),
        _ => Ok(packet.data.clone()),
    }
}

fn encoded_audio_input(
    codec: &CodecId,
    packet: &EncodedPacket,
    extra_data: Option<&Bytes>,
) -> Result<Bytes> {
    match codec {
        CodecId::Aac => aac_packet_to_adts(packet, extra_data),
        _ => Ok(packet.data.clone()),
    }
}

fn video_decode_pipeline(codec: &CodecId) -> Result<String> {
    let input = match codec {
        CodecId::H264 => {
            "video/x-h264,stream-format=(string)byte-stream,alignment=(string)au ! h264parse ! decodebin"
        }
        CodecId::H265 => {
            "video/x-h265,stream-format=(string)byte-stream,alignment=(string)au ! h265parse ! decodebin"
        }
        CodecId::ProRes => "video/x-prores ! decodebin",
        _ => {
            return codec_backend_error(
                codec,
                "decode GStreamer video packet",
                "codec has no GStreamer video decode pipeline",
            );
        }
    };
    Ok(format!(
        "{} caps={} ! videoconvert ! video/x-raw,format=(string)RGBA ! {}",
        appsrc_prefix(),
        input,
        appsink_suffix()
    ))
}

fn video_encode_pipeline(
    codec: &CodecId,
    width: u32,
    height: u32,
    time_base: TimeBase,
    frame_duration: i64,
    factory: &str,
) -> Result<String> {
    let (fps_num, fps_den) = framerate_fraction(time_base, frame_duration);
    let output = match codec {
        CodecId::H264 => format!(
            "{factory} ! h264parse ! video/x-h264,stream-format=(string)byte-stream,alignment=(string)au"
        ),
        CodecId::H265 => format!(
            "{factory} ! h265parse ! video/x-h265,stream-format=(string)byte-stream,alignment=(string)au"
        ),
        CodecId::ProRes => factory.to_owned(),
        _ => {
            return codec_backend_error(
                codec,
                "encode GStreamer video frame",
                "codec has no GStreamer video encode pipeline",
            );
        }
    };
    Ok(format!(
        "{} caps=video/x-raw,format=(string)RGBA,width=(int){width},height=(int){height},framerate=(fraction){fps_num}/{fps_den} ! videoconvert ! {output} ! {}",
        appsrc_prefix(),
        appsink_suffix()
    ))
}

fn audio_decode_pipeline(codec: &CodecId) -> Result<String> {
    let input = match codec {
        CodecId::Aac => {
            "audio/mpeg,mpegversion=(int)4,stream-format=(string)adts ! aacparse ! decodebin"
        }
        CodecId::Eac3 => "audio/x-eac3 ! decodebin",
        CodecId::Dts => "audio/x-dts ! decodebin",
        CodecId::Wma => "audio/x-wma ! decodebin",
        _ => {
            return codec_backend_error(
                codec,
                "decode GStreamer audio packet",
                "codec has no GStreamer audio decode pipeline",
            );
        }
    };
    Ok(format!(
        "{} caps={} ! audioconvert ! audioresample ! audio/x-raw,format=(string)F32LE,layout=(string)interleaved ! {}",
        appsrc_prefix(),
        input,
        appsink_suffix()
    ))
}

fn audio_encode_pipeline(
    codec: &CodecId,
    sample_rate: u32,
    channels: u16,
    factory: &str,
) -> Result<String> {
    let output = match codec {
        CodecId::Aac => format!(
            "{factory} ! aacparse ! audio/mpeg,mpegversion=(int)4,stream-format=(string)adts"
        ),
        CodecId::Eac3 => factory.to_owned(),
        _ => {
            return codec_backend_error(
                codec,
                "encode GStreamer audio frame",
                "codec has no GStreamer audio encode pipeline",
            );
        }
    };
    Ok(format!(
        "{} caps=audio/x-raw,format=(string)F32LE,layout=(string)interleaved,rate=(int){sample_rate},channels=(int){channels} ! audioconvert ! audioresample ! {output} ! {}",
        appsrc_prefix(),
        appsink_suffix()
    ))
}

fn appsrc_prefix() -> &'static str {
    "appsrc name=src is-live=false format=time block=true do-timestamp=false"
}

fn appsink_suffix() -> &'static str {
    "appsink name=sink sync=false async=false emit-signals=false"
}

fn raw_sample_to_rgba_frame(
    sample: RawSample,
    fallback_width: u32,
    fallback_height: u32,
    codec: &CodecId,
) -> Result<RgbaFrame> {
    let width = sample.width.unwrap_or(fallback_width);
    let height = sample.height.unwrap_or(fallback_height);
    if width == 0 || height == 0 {
        return codec_backend_error(
            codec,
            "decode GStreamer video packet",
            "decoded video sample has no dimensions",
        );
    }
    let stride = width as usize * 4;
    let expected = stride * height as usize;
    if sample.data.len() < expected {
        return codec_backend_error(
            codec,
            "decode GStreamer video packet",
            format!(
                "decoded RGBA sample has {} bytes, expected at least {expected}",
                sample.data.len()
            ),
        );
    }
    RgbaFrame::new(width, height, stride, sample.data[..expected].to_vec())
}

fn raw_sample_to_audio_frame(
    sample: RawSample,
    fallback_sample_rate: u32,
    fallback_channels: u16,
    input_time_base: TimeBase,
    input_pts: i64,
    codec: &CodecId,
) -> Result<AudioFrame> {
    let sample_rate = sample.sample_rate.unwrap_or(fallback_sample_rate);
    let channels = sample.channels.unwrap_or(fallback_channels);
    if sample_rate == 0 || channels == 0 {
        return codec_backend_error(
            codec,
            "decode GStreamer audio packet",
            "decoded audio sample has no rate/channels",
        );
    }
    if !sample.data.len().is_multiple_of(4) {
        return codec_backend_error(
            codec,
            "decode GStreamer audio packet",
            format!(
                "decoded F32LE sample has non-f32 byte length {}",
                sample.data.len()
            ),
        );
    }
    let samples = sample
        .data
        .chunks_exact(4)
        .map(|bytes| f32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
        .collect();
    let sample_time_base = TimeBase::new(
        1,
        i32_from_u32(sample_rate, codec, "decode GStreamer audio packet")?,
    )?;
    let pts = input_time_base.rescale(input_pts, sample_time_base);
    AudioFrame::new(sample_rate, channels, pts, samples)
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

fn framerate_fraction(time_base: TimeBase, frame_duration: i64) -> (i32, i32) {
    let duration = i32::try_from(frame_duration)
        .ok()
        .filter(|value| *value > 0);
    duration
        .and_then(|duration| time_base.num.checked_mul(duration))
        .filter(|den| *den > 0)
        .map(|den| (time_base.den, den))
        .unwrap_or((30, 1))
}

fn optional_bytes(data: &[u8]) -> Option<Bytes> {
    (!data.is_empty()).then(|| Bytes::copy_from_slice(data))
}

fn i32_from_u32(value: u32, codec: &CodecId, operation: &'static str) -> Result<i32> {
    i32::try_from(value).map_err(|_| Error::CodecBackend {
        codec: codec.clone(),
        operation,
        message: format!("value {value} does not fit in i32"),
    })
}

fn nonnegative_u32(value: c_int) -> Option<u32> {
    u32::try_from(value).ok()
}

fn unref_pipeline_parts(
    library: &GstLibrary,
    pipeline: *mut c_void,
    src: *mut c_void,
    sink: *mut c_void,
) {
    // SAFETY: Pointers are either null or owned references from GStreamer.
    unsafe {
        if !pipeline.is_null() {
            (library.gst.element_set_state)(pipeline, GST_STATE_NULL);
        }
        if !src.is_null() {
            (library.gst.object_unref)(src);
        }
        if !sink.is_null() {
            (library.gst.object_unref)(sink);
        }
        if !pipeline.is_null() {
            (library.gst.object_unref)(pipeline);
        }
    }
}

fn take_g_error(library: &GstLibrary, error: *mut GError) -> String {
    take_g_error_parts(library.g_error_free, error)
}

fn take_g_error_parts(g_error_free: GErrorFree, error: *mut GError) -> String {
    // SAFETY: error is a valid GError pointer owned by the caller.
    unsafe {
        let message = if (*error).message.is_null() {
            "unknown GError".to_owned()
        } else {
            CStr::from_ptr((*error).message)
                .to_string_lossy()
                .into_owned()
        };
        g_error_free(error);
        message
    }
}

fn dl_error() -> String {
    // SAFETY: dlerror returns either null or a valid C string owned by libc.
    unsafe {
        let error = dlerror();
        if error.is_null() {
            "unknown dynamic loader error".to_owned()
        } else {
            CStr::from_ptr(error).to_string_lossy().into_owned()
        }
    }
}

fn c_string(value: &str) -> CString {
    CString::new(value).expect("GStreamer static strings must not contain NUL bytes")
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

fn factories_for(codec: &CodecId, direction: CodecDirection) -> Option<&'static [&'static str]> {
    match (codec, direction) {
        (CodecId::H264, CodecDirection::Decode) => {
            Some(&["avdec_h264", "openh264dec", "vah264dec", "vaapih264dec"])
        }
        (CodecId::H264, CodecDirection::Encode) => {
            Some(&["x264enc", "openh264enc", "vah264enc", "vaapih264enc"])
        }
        (CodecId::H265, CodecDirection::Decode) => {
            Some(&["avdec_h265", "vah265dec", "vaapih265dec"])
        }
        (CodecId::H265, CodecDirection::Encode) => Some(&["x265enc", "vah265enc", "vaapih265enc"]),
        (CodecId::ProRes, CodecDirection::Decode) => Some(&["avdec_prores", "proresdec"]),
        (CodecId::ProRes, CodecDirection::Encode) => Some(&["proresenc", "avenc_prores"]),
        (CodecId::Aac, CodecDirection::Decode) => Some(&["avdec_aac", "faad", "fdkaacdec"]),
        (CodecId::Aac, CodecDirection::Encode) => {
            Some(&["fdkaacenc", "voaacenc", "avenc_aac", "avenc_aac_mf"])
        }
        (CodecId::Eac3, CodecDirection::Decode) => Some(&["avdec_eac3", "avdec_ac3"]),
        (CodecId::Eac3, CodecDirection::Encode) => Some(&["avenc_eac3"]),
        (CodecId::Dts, CodecDirection::Decode) => Some(&["avdec_dca"]),
        (CodecId::Wma, CodecDirection::Decode) => Some(&["avdec_wmav2", "avdec_wmav1"]),
        _ => None,
    }
}
