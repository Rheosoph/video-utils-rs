use super::{
    PlatformAudioDecoderConfig, PlatformAudioEncoderConfig, PlatformCodecProbe,
    PlatformVideoDecoderConfig, PlatformVideoEncoderConfig, platform_codec_error,
};
use crate::{
    audio::AudioFrame,
    backend::BackendKind,
    bitstream::{
        aac::{aac_packet_to_raw, audio_specific_config_from_format},
        h264::{avcc_length_size, avcc_parameter_sets, h264_packet_to_length_prefixed},
        h265::{h265_packet_to_length_prefixed, hvcc_length_size, hvcc_parameter_sets},
    },
    codec::{CodecDirection, CodecId},
    error::{Error, Result},
    frame::RgbaFrame,
    packet::EncodedPacket,
    time::TimeBase,
};
use bytes::Bytes;
use std::{
    ffi::{c_char, c_void},
    ptr, slice,
    sync::Mutex,
};

type Boolean = u8;
type CFAllocatorRef = *const c_void;
type CFDictionaryRef = *const c_void;
type CFTypeRef = *const c_void;
type CFIndex = isize;
type CMBlockBufferRef = *mut c_void;
type CMBlockBufferFlags = u32;
type CMItemCount = CFIndex;
type CMSampleBufferRef = *mut c_void;
type CMFormatDescriptionRef = *mut c_void;
type CMVideoFormatDescriptionRef = *mut c_void;
type CVImageBufferRef = *mut c_void;
type CVPixelBufferRef = *mut c_void;
type VTDecompressionSessionRef = *mut c_void;
type VTCompressionSessionRef = *mut c_void;
type AudioConverterRef = *mut c_void;
type AudioConverterComplexInputDataProc = Option<
    extern "C" fn(
        AudioConverterRef,
        *mut u32,
        *mut AudioBufferList,
        *mut *mut AudioStreamPacketDescription,
        *mut c_void,
    ) -> OSStatus,
>;
type OSStatus = i32;
type OSType = u32;
type VTEncodeInfoFlags = u32;
type VTDecodeInfoFlags = u32;
type VTDecodeFrameFlags = u32;
type VTEncodeFrameFlags = u32;
type CVReturn = i32;
type CVPixelBufferLockFlags = u64;

#[repr(C)]
struct CFDictionaryKeyCallBacks {
    version: CFIndex,
    retain: *const c_void,
    release: *const c_void,
    copy_description: *const c_void,
    equal: *const c_void,
    hash: *const c_void,
}

#[repr(C)]
struct CFDictionaryValueCallBacks {
    version: CFIndex,
    retain: *const c_void,
    release: *const c_void,
    copy_description: *const c_void,
    equal: *const c_void,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct CMTime {
    value: i64,
    timescale: i32,
    flags: u32,
    epoch: i64,
}

#[repr(C)]
struct CMSampleTimingInfo {
    duration: CMTime,
    presentation_time_stamp: CMTime,
    decode_time_stamp: CMTime,
}

#[repr(C)]
struct VTDecompressionOutputCallbackRecord {
    decompression_output_callback: Option<
        extern "C" fn(
            decompression_output_ref_con: *mut c_void,
            source_frame_ref_con: *mut c_void,
            status: OSStatus,
            info_flags: VTDecodeInfoFlags,
            image_buffer: CVImageBufferRef,
            presentation_time_stamp: CMTime,
            presentation_duration: CMTime,
        ),
    >,
    decompression_output_ref_con: *mut c_void,
}

const fn fourcc(bytes: &[u8; 4]) -> OSType {
    ((bytes[0] as OSType) << 24)
        | ((bytes[1] as OSType) << 16)
        | ((bytes[2] as OSType) << 8)
        | bytes[3] as OSType
}

const K_CM_VIDEO_CODEC_TYPE_H264: OSType = fourcc(b"avc1");
const K_CM_VIDEO_CODEC_TYPE_HEVC: OSType = fourcc(b"hvc1");
const K_CM_VIDEO_CODEC_TYPE_APPLE_PRO_RES_422: OSType = fourcc(b"apcn");
const K_CV_PIXEL_FORMAT_TYPE_32_BGRA: OSType = fourcc(b"BGRA");
const K_AUDIO_FORMAT_MPEG4_AAC: OSType = fourcc(b"aac ");
const K_AUDIO_FORMAT_ENHANCED_AC3: OSType = fourcc(b"ec-3");
const K_AUDIO_FORMAT_LINEAR_PCM: OSType = fourcc(b"lpcm");
const K_AUDIO_CONVERTER_DECOMPRESSION_MAGIC_COOKIE: OSType = fourcc(b"dmgc");
const K_AUDIO_CONVERTER_ENCODE_BIT_RATE: OSType = fourcc(b"brat");
const K_AUDIO_FORMAT_FLAG_IS_FLOAT: u32 = 1 << 0;
const K_AUDIO_FORMAT_FLAG_IS_PACKED: u32 = 1 << 3;
const K_CF_NUMBER_SINT32_TYPE: CFIndex = 3;
const K_CV_PIXEL_BUFFER_LOCK_READ_ONLY: CVPixelBufferLockFlags = 1;
const K_CM_TIME_FLAGS_VALID: u32 = 1 << 0;
const CM_TIME_SCALE: i32 = 1_000_000;
const DEFAULT_TRACK_ID: u32 = 1;

#[repr(C)]
#[derive(Clone, Copy)]
struct AudioStreamBasicDescription {
    sample_rate: f64,
    format_id: OSType,
    format_flags: u32,
    bytes_per_packet: u32,
    frames_per_packet: u32,
    bytes_per_frame: u32,
    channels_per_frame: u32,
    bits_per_channel: u32,
    reserved: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct AudioStreamPacketDescription {
    start_offset: i64,
    variable_frames_in_packet: u32,
    data_byte_size: u32,
}

#[repr(C)]
struct AudioBuffer {
    number_channels: u32,
    data_byte_size: u32,
    data: *mut c_void,
}

#[repr(C)]
struct AudioBufferList {
    number_buffers: u32,
    buffers: [AudioBuffer; 1],
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    static kCFAllocatorNull: CFAllocatorRef;
    static kCFTypeDictionaryKeyCallBacks: CFDictionaryKeyCallBacks;
    static kCFTypeDictionaryValueCallBacks: CFDictionaryValueCallBacks;

    fn CFRelease(cf: CFTypeRef);
    fn CFNumberCreate(
        allocator: CFAllocatorRef,
        the_type: CFIndex,
        value_ptr: *const c_void,
    ) -> CFTypeRef;
    fn CFDictionaryCreate(
        allocator: CFAllocatorRef,
        keys: *const *const c_void,
        values: *const *const c_void,
        num_values: CFIndex,
        key_callbacks: *const CFDictionaryKeyCallBacks,
        value_callbacks: *const CFDictionaryValueCallBacks,
    ) -> CFDictionaryRef;
}

#[link(name = "CoreMedia", kind = "framework")]
unsafe extern "C" {
    fn CMBlockBufferCreateWithMemoryBlock(
        structure_allocator: CFAllocatorRef,
        memory_block: *mut c_void,
        block_length: usize,
        block_allocator: CFAllocatorRef,
        custom_block_source: *const c_void,
        offset_to_data: usize,
        data_length: usize,
        flags: CMBlockBufferFlags,
        block_buffer_out: *mut CMBlockBufferRef,
    ) -> OSStatus;
    fn CMBlockBufferGetDataPointer(
        the_buffer: CMBlockBufferRef,
        offset: usize,
        length_at_offset_out: *mut usize,
        total_length_out: *mut usize,
        data_pointer_out: *mut *mut c_char,
    ) -> OSStatus;
    fn CMVideoFormatDescriptionCreate(
        allocator: CFAllocatorRef,
        codec_type: OSType,
        width: i32,
        height: i32,
        extensions: CFDictionaryRef,
        format_description_out: *mut CMVideoFormatDescriptionRef,
    ) -> OSStatus;
    fn CMVideoFormatDescriptionCreateFromH264ParameterSets(
        allocator: CFAllocatorRef,
        parameter_set_count: usize,
        parameter_set_pointers: *const *const u8,
        parameter_set_sizes: *const usize,
        nal_unit_header_length: i32,
        format_description_out: *mut CMVideoFormatDescriptionRef,
    ) -> OSStatus;
    fn CMVideoFormatDescriptionCreateFromHEVCParameterSets(
        allocator: CFAllocatorRef,
        parameter_set_count: usize,
        parameter_set_pointers: *const *const u8,
        parameter_set_sizes: *const usize,
        nal_unit_header_length: i32,
        extensions: CFDictionaryRef,
        format_description_out: *mut CMVideoFormatDescriptionRef,
    ) -> OSStatus;
    fn CMVideoFormatDescriptionGetH264ParameterSetAtIndex(
        video_desc: CMFormatDescriptionRef,
        parameter_set_index: usize,
        parameter_set_pointer_out: *mut *const u8,
        parameter_set_size_out: *mut usize,
        parameter_set_count_out: *mut usize,
        nal_unit_header_length_out: *mut i32,
    ) -> OSStatus;
    fn CMVideoFormatDescriptionGetHEVCParameterSetAtIndex(
        video_desc: CMFormatDescriptionRef,
        parameter_set_index: usize,
        parameter_set_pointer_out: *mut *const u8,
        parameter_set_size_out: *mut usize,
        parameter_set_count_out: *mut usize,
        nal_unit_header_length_out: *mut i32,
    ) -> OSStatus;
    fn CMSampleBufferCreateReady(
        allocator: CFAllocatorRef,
        data_buffer: CMBlockBufferRef,
        format_description: CMVideoFormatDescriptionRef,
        num_samples: CMItemCount,
        num_sample_timing_entries: CMItemCount,
        sample_timing_array: *const CMSampleTimingInfo,
        num_sample_size_entries: CMItemCount,
        sample_size_array: *const usize,
        sample_buffer_out: *mut CMSampleBufferRef,
    ) -> OSStatus;
    fn CMSampleBufferGetDataBuffer(sbuf: CMSampleBufferRef) -> CMBlockBufferRef;
    fn CMSampleBufferGetDuration(sbuf: CMSampleBufferRef) -> CMTime;
    fn CMSampleBufferGetFormatDescription(sbuf: CMSampleBufferRef) -> CMFormatDescriptionRef;
    fn CMSampleBufferGetPresentationTimeStamp(sbuf: CMSampleBufferRef) -> CMTime;
}

#[link(name = "CoreVideo", kind = "framework")]
unsafe extern "C" {
    static kCVPixelBufferPixelFormatTypeKey: CFTypeRef;

    fn CVPixelBufferCreate(
        allocator: CFAllocatorRef,
        width: usize,
        height: usize,
        pixel_format_type: OSType,
        pixel_buffer_attributes: CFDictionaryRef,
        pixel_buffer_out: *mut CVPixelBufferRef,
    ) -> CVReturn;
    fn CVPixelBufferLockBaseAddress(
        pixel_buffer: CVPixelBufferRef,
        lock_flags: CVPixelBufferLockFlags,
    ) -> CVReturn;
    fn CVPixelBufferUnlockBaseAddress(
        pixel_buffer: CVPixelBufferRef,
        unlock_flags: CVPixelBufferLockFlags,
    ) -> CVReturn;
    fn CVPixelBufferGetBaseAddress(pixel_buffer: CVPixelBufferRef) -> *mut c_void;
    fn CVPixelBufferGetBytesPerRow(pixel_buffer: CVPixelBufferRef) -> usize;
    fn CVPixelBufferGetPixelFormatType(pixel_buffer: CVPixelBufferRef) -> OSType;
    fn CVPixelBufferGetWidth(pixel_buffer: CVPixelBufferRef) -> usize;
    fn CVPixelBufferGetHeight(pixel_buffer: CVPixelBufferRef) -> usize;
}

#[link(name = "VideoToolbox", kind = "framework")]
unsafe extern "C" {
    fn VTIsHardwareDecodeSupported(codec_type: OSType) -> Boolean;
    fn VTDecompressionSessionCreate(
        allocator: CFAllocatorRef,
        video_format_description: CMVideoFormatDescriptionRef,
        video_decoder_specification: CFDictionaryRef,
        destination_image_buffer_attributes: CFDictionaryRef,
        output_callback: *const VTDecompressionOutputCallbackRecord,
        decompression_session_out: *mut VTDecompressionSessionRef,
    ) -> OSStatus;
    fn VTDecompressionSessionDecodeFrame(
        session: VTDecompressionSessionRef,
        sample_buffer: CMSampleBufferRef,
        decode_flags: VTDecodeFrameFlags,
        source_frame_ref_con: *mut c_void,
        info_flags_out: *mut VTDecodeInfoFlags,
    ) -> OSStatus;
    fn VTDecompressionSessionWaitForAsynchronousFrames(
        session: VTDecompressionSessionRef,
    ) -> OSStatus;
    fn VTDecompressionSessionInvalidate(session: VTDecompressionSessionRef);
    fn VTCompressionSessionCreate(
        allocator: CFAllocatorRef,
        width: i32,
        height: i32,
        codec_type: OSType,
        encoder_specification: CFDictionaryRef,
        image_buffer_attributes: CFDictionaryRef,
        compressed_data_allocator: CFAllocatorRef,
        output_callback: Option<
            extern "C" fn(
                output_callback_ref_con: *mut c_void,
                source_frame_ref_con: *mut c_void,
                status: OSStatus,
                info_flags: VTEncodeInfoFlags,
                sample_buffer: CMSampleBufferRef,
            ),
        >,
        output_callback_ref_con: *mut c_void,
        compression_session_out: *mut VTCompressionSessionRef,
    ) -> OSStatus;
    fn VTCompressionSessionEncodeFrame(
        session: VTCompressionSessionRef,
        image_buffer: CVImageBufferRef,
        presentation_time_stamp: CMTime,
        duration: CMTime,
        frame_properties: CFDictionaryRef,
        source_frame_refcon: *mut c_void,
        info_flags_out: *mut VTEncodeFrameFlags,
    ) -> OSStatus;
    fn VTCompressionSessionCompleteFrames(
        session: VTCompressionSessionRef,
        complete_until_presentation_time_stamp: CMTime,
    ) -> OSStatus;
    fn VTCompressionSessionInvalidate(session: VTCompressionSessionRef);
}

#[link(name = "AudioToolbox", kind = "framework")]
unsafe extern "C" {
    fn AudioConverterNew(
        source_format: *const AudioStreamBasicDescription,
        destination_format: *const AudioStreamBasicDescription,
        converter_out: *mut AudioConverterRef,
    ) -> OSStatus;
    fn AudioConverterDispose(converter: AudioConverterRef) -> OSStatus;
    fn AudioConverterSetProperty(
        converter: AudioConverterRef,
        property_id: OSType,
        property_data_size: u32,
        property_data: *const c_void,
    ) -> OSStatus;
    fn AudioConverterReset(converter: AudioConverterRef) -> OSStatus;
    fn AudioConverterFillComplexBuffer(
        converter: AudioConverterRef,
        input_data_proc: AudioConverterComplexInputDataProc,
        input_data_proc_user_data: *mut c_void,
        output_data_packet_size: *mut u32,
        output_data: *mut AudioBufferList,
        output_packet_description: *mut AudioStreamPacketDescription,
    ) -> OSStatus;
}

pub struct VideoDecoderHandle {
    codec: CodecId,
    session: VTDecompressionSessionRef,
    format_description: CMVideoFormatDescriptionRef,
    extra_data: Bytes,
    state: Box<VideoDecoderState>,
}

impl Drop for VideoDecoderHandle {
    fn drop(&mut self) {
        // SAFETY: The handle owns a valid VideoToolbox session and CoreMedia
        // format description created by this module, or null when construction
        // failed before ownership transfer.
        unsafe {
            if !self.session.is_null() {
                let _ = VTDecompressionSessionWaitForAsynchronousFrames(self.session);
                VTDecompressionSessionInvalidate(self.session);
                CFRelease(self.session.cast());
            }
            if !self.format_description.is_null() {
                CFRelease(self.format_description.cast());
            }
        }
    }
}

pub struct VideoEncoderHandle {
    codec: CodecId,
    session: VTCompressionSessionRef,
    width: u32,
    height: u32,
    time_base: TimeBase,
    frame_duration: i64,
    state: Box<VideoEncoderState>,
}

impl Drop for VideoEncoderHandle {
    fn drop(&mut self) {
        // SAFETY: The handle owns a valid VideoToolbox compression session
        // created by this module.
        unsafe {
            if !self.session.is_null() {
                let _ = VTCompressionSessionCompleteFrames(self.session, cm_time_invalid());
                VTCompressionSessionInvalidate(self.session);
                CFRelease(self.session.cast());
            }
        }
    }
}

pub struct AudioConverterHandle {
    codec: CodecId,
    converter: AudioConverterRef,
    source: AudioStreamBasicDescription,
    destination: AudioStreamBasicDescription,
    sample_rate: u32,
    channels: u16,
}

struct VideoDecoderState {
    width: u32,
    height: u32,
    frames: Mutex<Vec<RgbaFrame>>,
    errors: Mutex<Vec<String>>,
}

struct VideoEncoderState {
    codec: CodecId,
    packets: Mutex<Vec<AppleEncodedPacket>>,
    errors: Mutex<Vec<String>>,
    codec_config: Mutex<Option<Vec<u8>>>,
}

impl VideoEncoderState {
    fn new(codec: CodecId) -> Self {
        Self {
            codec,
            packets: Mutex::new(Vec::new()),
            errors: Mutex::new(Vec::new()),
            codec_config: Mutex::new(None),
        }
    }
}

struct AppleEncodedPacket {
    pts: CMTime,
    duration: CMTime,
    data: Vec<u8>,
}

struct AudioConverterOpenOptions<'a> {
    source: AudioStreamBasicDescription,
    destination: AudioStreamBasicDescription,
    sample_rate: u32,
    channels: u16,
    bitrate: Option<u32>,
    magic_cookie: Option<&'a [u8]>,
    operation: &'static str,
}

impl Drop for AudioConverterHandle {
    fn drop(&mut self) {
        // SAFETY: The handle owns a valid AudioConverter created by this module.
        unsafe {
            if !self.converter.is_null() {
                let _ = AudioConverterDispose(self.converter);
            }
        }
    }
}

pub fn probe(
    backend: BackendKind,
    codec: &CodecId,
    direction: CodecDirection,
) -> PlatformCodecProbe {
    let result = match backend {
        BackendKind::AppleVideoToolbox => probe_video(codec, direction),
        BackendKind::AppleAudioToolbox => probe_audio(codec, direction),
        _ => Err("backend is not an Apple platform backend".to_owned()),
    };

    PlatformCodecProbe {
        backend: Some(backend),
        codec: codec.clone(),
        direction,
        supported: result.is_ok(),
        detail: result.unwrap_or_else(|message| message),
    }
}

pub fn open_video_decoder(config: &PlatformVideoDecoderConfig) -> Result<VideoDecoderHandle> {
    let codec_type = video_codec_type(&config.codec).ok_or_else(|| Error::CodecBackend {
        codec: config.codec.clone(),
        operation: "open VideoToolbox decoder",
        message: "codec is not supported by VideoToolbox adapter".to_owned(),
    })?;

    let extra_data = Bytes::copy_from_slice(&config.extra_data);
    let format_description =
        create_video_format_description(&config.codec, codec_type, config, &extra_data)?;
    let attrs = create_bgra_decoder_attributes(&config.codec)?;
    let mut state = Box::new(VideoDecoderState {
        width: config.width,
        height: config.height,
        frames: Mutex::new(Vec::new()),
        errors: Mutex::new(Vec::new()),
    });
    let callback = VTDecompressionOutputCallbackRecord {
        decompression_output_callback: Some(decompression_output_callback),
        decompression_output_ref_con: state.as_mut() as *mut VideoDecoderState as *mut c_void,
    };

    let mut session = ptr::null_mut();
    // SAFETY: The format description was created above and remains owned until
    // the returned handle is dropped. The destination attributes request BGRA
    // output, and the callback refcon points to state owned by the returned
    // handle and kept alive until session invalidation.
    let status = unsafe {
        VTDecompressionSessionCreate(
            ptr::null(),
            format_description,
            ptr::null(),
            attrs,
            &callback,
            &mut session,
        )
    };
    // SAFETY: The attributes dictionary was created by CoreFoundation above and
    // VTDecompressionSessionCreate retains any values it needs.
    unsafe {
        CFRelease(attrs.cast());
    }
    if status != 0 {
        // SAFETY: format_description was created by CoreMedia above.
        unsafe {
            CFRelease(format_description.cast());
        }
        return Err(os_status_error(
            &config.codec,
            "open VideoToolbox decoder",
            "VTDecompressionSessionCreate",
            status,
        ));
    }

    Ok(VideoDecoderHandle {
        codec: config.codec.clone(),
        session,
        format_description,
        extra_data,
        state,
    })
}

pub fn open_video_encoder(config: &PlatformVideoEncoderConfig) -> Result<VideoEncoderHandle> {
    let codec_type = video_codec_type(&config.codec).ok_or_else(|| Error::CodecBackend {
        codec: config.codec.clone(),
        operation: "open VideoToolbox encoder",
        message: "codec is not supported by VideoToolbox adapter".to_owned(),
    })?;
    let mut session = ptr::null_mut();
    let mut state = Box::new(VideoEncoderState::new(config.codec.clone()));
    let state_ptr = state.as_mut() as *mut VideoEncoderState;
    // SAFETY: Null allocator/dictionaries request platform defaults. The output
    // callback stores packet bytes in the state object owned by the returned
    // handle and kept alive until session invalidation.
    let status = unsafe {
        VTCompressionSessionCreate(
            ptr::null(),
            config.width as i32,
            config.height as i32,
            codec_type,
            ptr::null(),
            ptr::null(),
            ptr::null(),
            Some(compression_output_callback),
            state_ptr.cast(),
            &mut session,
        )
    };
    if status != 0 {
        return Err(os_status_error(
            &config.codec,
            "open VideoToolbox encoder",
            "VTCompressionSessionCreate",
            status,
        ));
    }

    Ok(VideoEncoderHandle {
        codec: config.codec.clone(),
        session,
        width: config.width,
        height: config.height,
        time_base: config.time_base,
        frame_duration: config.frame_duration,
        state,
    })
}

pub fn open_audio_decoder(config: &PlatformAudioDecoderConfig) -> Result<AudioConverterHandle> {
    let source = compressed_asbd(&config.codec, config.sample_rate, config.channels)?;
    let destination = pcm_asbd(config.sample_rate, config.channels);
    let synthesized_cookie;
    let magic_cookie = if !config.extra_data.is_empty() {
        Some(config.extra_data.as_slice())
    } else if config.codec == CodecId::Aac {
        synthesized_cookie =
            audio_specific_config_from_format(config.sample_rate, config.channels)?;
        Some(synthesized_cookie.as_ref())
    } else {
        None
    };
    open_audio_converter(
        &config.codec,
        AudioConverterOpenOptions {
            source,
            destination,
            sample_rate: config.sample_rate,
            channels: config.channels,
            bitrate: None,
            magic_cookie,
            operation: "open AudioToolbox decoder",
        },
    )
}

pub fn open_audio_encoder(config: &PlatformAudioEncoderConfig) -> Result<AudioConverterHandle> {
    let source = pcm_asbd(config.sample_rate, config.channels);
    let destination = compressed_asbd(&config.codec, config.sample_rate, config.channels)?;
    open_audio_converter(
        &config.codec,
        AudioConverterOpenOptions {
            source,
            destination,
            sample_rate: config.sample_rate,
            channels: config.channels,
            bitrate: config.bitrate,
            magic_cookie: None,
            operation: "open AudioToolbox encoder",
        },
    )
}

impl VideoDecoderHandle {
    pub fn decode_packet(&mut self, packet: &EncodedPacket) -> Result<Vec<RgbaFrame>> {
        let data = video_decoder_sample_data(&self.codec, packet, &self.extra_data)?;
        let (block, sample) =
            create_video_sample_buffer(&self.codec, self.format_description, &data, packet)?;
        let mut info_flags = 0;
        // SAFETY: sample is a valid CMSampleBuffer backed by data that remains
        // alive until this synchronous decode call returns. Decode flags are 0,
        // so VT calls the output callback before returning.
        let status = unsafe {
            VTDecompressionSessionDecodeFrame(
                self.session,
                sample,
                0,
                ptr::null_mut(),
                &mut info_flags,
            )
        };
        // SAFETY: sample/block were created retained by CoreMedia above.
        unsafe {
            CFRelease(sample.cast());
            CFRelease(block.cast());
        }
        if status != 0 {
            return Err(os_status_error(
                &self.codec,
                "decode VideoToolbox packet",
                "VTDecompressionSessionDecodeFrame",
                status,
            ));
        }
        self.take_frames("decode VideoToolbox packet")
    }

    pub fn flush_video_decoder(&mut self) -> Result<Vec<RgbaFrame>> {
        // SAFETY: The decompression session is live until drop.
        let status = unsafe { VTDecompressionSessionWaitForAsynchronousFrames(self.session) };
        if status != 0 {
            return Err(os_status_error(
                &self.codec,
                "flush VideoToolbox decoder",
                "VTDecompressionSessionWaitForAsynchronousFrames",
                status,
            ));
        }
        self.take_frames("flush VideoToolbox decoder")
    }

    fn take_frames(&mut self, operation: &'static str) -> Result<Vec<RgbaFrame>> {
        let mut errors = self.state.errors.lock().map_err(|_| Error::CodecBackend {
            codec: self.codec.clone(),
            operation,
            message: "decoder callback error lock is poisoned".to_owned(),
        })?;
        if let Some(message) = errors.pop() {
            errors.clear();
            return Err(Error::CodecBackend {
                codec: self.codec.clone(),
                operation,
                message,
            });
        }
        drop(errors);

        let mut frames = self.state.frames.lock().map_err(|_| Error::CodecBackend {
            codec: self.codec.clone(),
            operation,
            message: "decoder frame lock is poisoned".to_owned(),
        })?;
        Ok(frames.drain(..).collect())
    }
}

impl VideoEncoderHandle {
    pub fn encode_frame(
        &mut self,
        codec: &CodecId,
        frame: &RgbaFrame,
        pts: i64,
    ) -> Result<Vec<EncodedPacket>> {
        if frame.width != self.width || frame.height != self.height {
            return platform_codec_error(
                codec,
                "encode VideoToolbox frame",
                format!(
                    "frame dimensions {}x{} do not match encoder config {}x{}",
                    frame.width, frame.height, self.width, self.height
                ),
            );
        }
        let pixel_buffer = create_bgra_pixel_buffer(codec, frame)?;
        let mut info_flags = 0;
        // SAFETY: The compression session owns/retains any frame references it
        // needs after VTCompressionSessionEncodeFrame returns. The pixel buffer
        // is released below after the call.
        let status = unsafe {
            VTCompressionSessionEncodeFrame(
                self.session,
                pixel_buffer,
                cm_time(pts, self.time_base)?,
                cm_time(self.frame_duration.max(0), self.time_base)?,
                ptr::null(),
                ptr::null_mut(),
                &mut info_flags,
            )
        };
        // SAFETY: The pixel buffer was created by CVPixelBufferCreate above.
        unsafe {
            CFRelease(pixel_buffer.cast());
        }
        if status != 0 {
            return Err(os_status_error(
                codec,
                "encode VideoToolbox frame",
                "VTCompressionSessionEncodeFrame",
                status,
            ));
        }
        self.complete_frames(cm_time(pts, self.time_base)?)?;
        self.take_packets(codec)
    }

    pub fn finish_video_encoder(&mut self, codec: &CodecId) -> Result<Vec<EncodedPacket>> {
        self.complete_frames(cm_time_invalid())?;
        self.take_packets(codec)
    }

    fn complete_frames(&mut self, until: CMTime) -> Result<()> {
        // SAFETY: The compression session is live until drop.
        let status = unsafe { VTCompressionSessionCompleteFrames(self.session, until) };
        if status != 0 {
            return Err(os_status_error(
                &self.codec,
                "finish VideoToolbox frames",
                "VTCompressionSessionCompleteFrames",
                status,
            ));
        }
        Ok(())
    }

    fn take_packets(&mut self, codec: &CodecId) -> Result<Vec<EncodedPacket>> {
        let mut errors = self.state.errors.lock().map_err(|_| Error::CodecBackend {
            codec: codec.clone(),
            operation: "read VideoToolbox callback errors",
            message: "callback error lock is poisoned".to_owned(),
        })?;
        if let Some(message) = errors.pop() {
            errors.clear();
            return Err(Error::CodecBackend {
                codec: codec.clone(),
                operation: "encode VideoToolbox frame",
                message,
            });
        }
        drop(errors);

        let mut packets = self.state.packets.lock().map_err(|_| Error::CodecBackend {
            codec: codec.clone(),
            operation: "read VideoToolbox packets",
            message: "packet lock is poisoned".to_owned(),
        })?;
        Ok(packets
            .drain(..)
            .map(|packet| {
                EncodedPacket::new(
                    DEFAULT_TRACK_ID,
                    codec.clone(),
                    cm_time_to_ticks(packet.pts, self.time_base),
                    cm_time_to_ticks(packet.duration, self.time_base),
                    self.time_base,
                    packet.data,
                )
            })
            .collect())
    }

    pub fn codec_config(&self) -> Option<Vec<u8>> {
        self.state
            .codec_config
            .lock()
            .ok()
            .and_then(|config| config.clone())
    }
}

impl AudioConverterHandle {
    pub fn decode_packet(&mut self, packet: &EncodedPacket) -> Result<Vec<AudioFrame>> {
        let data = audio_decoder_input(&self.codec, packet)?;
        let input_packets = 1;
        let mut input_description = AudioStreamPacketDescription {
            start_offset: 0,
            variable_frames_in_packet: self.source.frames_per_packet,
            data_byte_size: u32::try_from(data.len()).map_err(|_| Error::CodecBackend {
                codec: self.codec.clone(),
                operation: "decode AudioToolbox packet",
                message: "encoded audio packet is too large for AudioConverter".to_owned(),
            })?,
        };
        let mut input = AudioInputProcState {
            data: data.as_ref(),
            buffer_channels: 0,
            packet_count: input_packets,
            bytes_per_packet: data.len(),
            packet_description: Some(&mut input_description),
            packet_offset: 0,
        };
        let output_frames = self.source.frames_per_packet.max(1).saturating_mul(4);
        let output_capacity = output_frames as usize * usize::from(self.channels) * 4;
        let mut output = vec![0u8; output_capacity.max(4096)];
        let mut output_packets =
            u32::try_from(output.len() / (usize::from(self.channels) * 4)).unwrap_or(u32::MAX);
        let mut output_list = audio_buffer_list(self.channels, &mut output);

        // SAFETY: The converter is live, and input/output buffers remain valid
        // for the duration of the synchronous FillComplexBuffer call.
        let status = unsafe {
            AudioConverterFillComplexBuffer(
                self.converter,
                Some(audio_input_proc),
                (&mut input as *mut AudioInputProcState<'_>).cast(),
                &mut output_packets,
                &mut output_list,
                ptr::null_mut(),
            )
        };
        if status != 0 {
            return Err(os_status_error(
                &self.codec,
                "decode AudioToolbox packet",
                "AudioConverterFillComplexBuffer",
                status,
            ));
        }
        if output_packets == 0 || output_list.buffers[0].data_byte_size == 0 {
            return Ok(Vec::new());
        }
        let bytes_written = output_list.buffers[0].data_byte_size as usize;
        let samples = f32_samples_from_ne_bytes(&output[..bytes_written]);
        let pts = packet
            .time_base
            .rescale(packet.pts, TimeBase::new(1, self.sample_rate as i32)?);
        Ok(vec![AudioFrame::new(
            self.sample_rate,
            self.channels,
            pts,
            samples,
        )?])
    }

    pub fn encode_frame(
        &mut self,
        codec: &CodecId,
        frame: &AudioFrame,
    ) -> Result<Vec<EncodedPacket>> {
        if frame.sample_rate != self.sample_rate || frame.channels != self.channels {
            return platform_codec_error(
                codec,
                "encode AudioToolbox frame",
                format!(
                    "frame audio format {} Hz/{} ch does not match encoder config {} Hz/{} ch",
                    frame.sample_rate, frame.channels, self.sample_rate, self.channels
                ),
            );
        }
        let input = f32_samples_to_ne_bytes(&frame.samples_f32_interleaved);
        let input_frames =
            u32::try_from(frame.sample_frames()).map_err(|_| Error::CodecBackend {
                codec: codec.clone(),
                operation: "encode AudioToolbox frame",
                message: "audio frame has too many sample frames for AudioConverter".to_owned(),
            })?;
        self.fill_audio_encoder(
            codec,
            &input,
            input_frames,
            frame.pts,
            frame.sample_frames() as i64,
            TimeBase::new(1, self.sample_rate as i32)?,
        )
    }

    pub fn finish_audio_encoder(&mut self, codec: &CodecId) -> Result<Vec<EncodedPacket>> {
        self.fill_audio_encoder(
            codec,
            &[],
            0,
            0,
            self.destination.frames_per_packet as i64,
            TimeBase::new(1, self.sample_rate as i32)?,
        )
    }

    fn fill_audio_encoder(
        &mut self,
        codec: &CodecId,
        input_bytes: &[u8],
        input_frames: u32,
        pts: i64,
        fallback_duration: i64,
        time_base: TimeBase,
    ) -> Result<Vec<EncodedPacket>> {
        let mut input = AudioInputProcState {
            data: input_bytes,
            buffer_channels: u32::from(self.channels),
            packet_count: input_frames,
            bytes_per_packet: usize::from(self.channels) * 4,
            packet_description: None,
            packet_offset: 0,
        };
        let output_packet_capacity = input_frames
            .div_ceil(self.destination.frames_per_packet.max(1))
            .saturating_add(8)
            .max(8);
        let mut output = vec![0u8; (output_packet_capacity as usize * 8192).max(65_536)];
        let mut output_packets = output_packet_capacity;
        let mut output_descriptions = zeroed_packet_descriptions(output_packet_capacity as usize);
        let mut output_list = audio_buffer_list(0, &mut output);

        // SAFETY: The converter is live, and input/output buffers remain valid
        // for the duration of the synchronous FillComplexBuffer call.
        let status = unsafe {
            AudioConverterFillComplexBuffer(
                self.converter,
                Some(audio_input_proc),
                (&mut input as *mut AudioInputProcState<'_>).cast(),
                &mut output_packets,
                &mut output_list,
                output_descriptions.as_mut_ptr(),
            )
        };
        if status != 0 {
            return Err(os_status_error(
                codec,
                "encode AudioToolbox frame",
                "AudioConverterFillComplexBuffer",
                status,
            ));
        }
        Ok(encoded_audio_packets_from_output(
            codec,
            &output[..output_list.buffers[0].data_byte_size as usize],
            &output_descriptions[..output_packets as usize],
            output_packets,
            pts,
            fallback_duration,
            time_base,
        ))
    }

    pub fn reset(&mut self) -> Result<()> {
        // SAFETY: The converter is live until drop.
        let status = unsafe { AudioConverterReset(self.converter) };
        if status != 0 {
            return Err(os_status_error(
                &self.codec,
                "reset AudioToolbox converter",
                "AudioConverterReset",
                status,
            ));
        }
        Ok(())
    }
}

fn probe_video(codec: &CodecId, direction: CodecDirection) -> std::result::Result<String, String> {
    let codec_type =
        video_codec_type(codec).ok_or_else(|| "codec has no VideoToolbox mapping".to_owned())?;
    match direction {
        CodecDirection::Decode => {
            let config = PlatformVideoDecoderConfig::new(codec.clone(), 16, 16);
            open_video_decoder(&config)
                .map(|_| {
                    let hardware = unsafe { VTIsHardwareDecodeSupported(codec_type) } != 0;
                    format!("VideoToolbox decoder session created; hardware_decode={hardware}")
                })
                .map_err(|error| error.to_string())
        }
        CodecDirection::Encode => {
            let config = PlatformVideoEncoderConfig::new(
                codec.clone(),
                16,
                16,
                crate::time::TimeBase::milliseconds(),
                33,
            );
            open_video_encoder(&config)
                .map(|_| "VideoToolbox compression session created".to_owned())
                .map_err(|error| error.to_string())
        }
    }
}

fn probe_audio(codec: &CodecId, direction: CodecDirection) -> std::result::Result<String, String> {
    match direction {
        CodecDirection::Decode => {
            let config = PlatformAudioDecoderConfig::new(codec.clone(), 48_000, 2);
            open_audio_decoder(&config)
                .map(|_| "AudioToolbox AudioConverter decoder created".to_owned())
                .map_err(|error| error.to_string())
        }
        CodecDirection::Encode => {
            let config = PlatformAudioEncoderConfig::new(codec.clone(), 48_000, 2);
            open_audio_encoder(&config)
                .map(|_| "AudioToolbox AudioConverter encoder created".to_owned())
                .map_err(|error| error.to_string())
        }
    }
}

fn open_audio_converter(
    codec: &CodecId,
    options: AudioConverterOpenOptions<'_>,
) -> Result<AudioConverterHandle> {
    let mut converter = ptr::null_mut();
    // SAFETY: Source and destination ASBD pointers are valid for the duration of
    // the call and the output pointer is valid for one AudioConverterRef.
    let status =
        unsafe { AudioConverterNew(&options.source, &options.destination, &mut converter) };
    if status != 0 {
        return Err(os_status_error(
            codec,
            options.operation,
            "AudioConverterNew",
            status,
        ));
    }
    if let Some(cookie) = options.magic_cookie
        && !cookie.is_empty()
    {
        set_audio_converter_property(
            codec,
            converter,
            options.operation,
            K_AUDIO_CONVERTER_DECOMPRESSION_MAGIC_COOKIE,
            cookie,
            "AudioConverterSetProperty(kAudioConverterDecompressionMagicCookie)",
        )?;
    }
    if let Some(bitrate) = options.bitrate {
        set_audio_converter_property(
            codec,
            converter,
            options.operation,
            K_AUDIO_CONVERTER_ENCODE_BIT_RATE,
            &bitrate.to_ne_bytes(),
            "AudioConverterSetProperty(kAudioConverterEncodeBitRate)",
        )?;
    }
    Ok(AudioConverterHandle {
        codec: codec.clone(),
        converter,
        source: options.source,
        destination: options.destination,
        sample_rate: options.sample_rate,
        channels: options.channels,
    })
}

fn set_audio_converter_property(
    codec: &CodecId,
    converter: AudioConverterRef,
    operation: &'static str,
    property_id: OSType,
    value: &[u8],
    api: &'static str,
) -> Result<()> {
    let size = u32::try_from(value.len()).map_err(|_| Error::CodecBackend {
        codec: codec.clone(),
        operation,
        message: "AudioConverter property value is too large".to_owned(),
    })?;
    // SAFETY: The converter is live and value points to size bytes for this call.
    let status =
        unsafe { AudioConverterSetProperty(converter, property_id, size, value.as_ptr().cast()) };
    if status != 0 {
        return Err(os_status_error(codec, operation, api, status));
    }
    Ok(())
}

fn create_video_format_description(
    codec: &CodecId,
    codec_type: OSType,
    config: &PlatformVideoDecoderConfig,
    extra_data: &Bytes,
) -> Result<CMVideoFormatDescriptionRef> {
    match codec {
        CodecId::H264 if !extra_data.is_empty() => {
            let parameter_sets = avcc_parameter_sets(extra_data)?;
            let length_size = avcc_length_size(extra_data)?;
            create_h264_format_description(codec, &parameter_sets, length_size)
        }
        CodecId::H265 if !extra_data.is_empty() => {
            let parameter_sets = hvcc_parameter_sets(extra_data)?;
            let length_size = hvcc_length_size(extra_data)?;
            create_hevc_format_description(codec, &parameter_sets, length_size)
        }
        _ => {
            let mut format_description = ptr::null_mut();
            // SAFETY: Pointers are null for default allocator/extensions per
            // CoreMedia conventions. The out pointer is valid.
            let status = unsafe {
                CMVideoFormatDescriptionCreate(
                    ptr::null(),
                    codec_type,
                    config.width as i32,
                    config.height as i32,
                    ptr::null(),
                    &mut format_description,
                )
            };
            if status != 0 {
                return Err(os_status_error(
                    codec,
                    "open VideoToolbox decoder",
                    "CMVideoFormatDescriptionCreate",
                    status,
                ));
            }
            Ok(format_description)
        }
    }
}

fn create_h264_format_description(
    codec: &CodecId,
    parameter_sets: &[Bytes],
    length_size: usize,
) -> Result<CMVideoFormatDescriptionRef> {
    if !parameter_sets
        .iter()
        .any(|nal| !nal.is_empty() && nal[0] & 0x1f == 7)
        || !parameter_sets
            .iter()
            .any(|nal| !nal.is_empty() && nal[0] & 0x1f == 8)
    {
        return platform_codec_error(
            codec,
            "open VideoToolbox decoder",
            "avcC extra_data must contain at least one SPS and one PPS for VideoToolbox",
        );
    }
    let pointers: Vec<*const u8> = parameter_sets.iter().map(|nal| nal.as_ptr()).collect();
    let sizes: Vec<usize> = parameter_sets.iter().map(Bytes::len).collect();
    let mut format_description = ptr::null_mut();
    // SAFETY: parameter set pointers/sizes refer to Bytes values that live for
    // the duration of this call. CoreMedia copies/parses the parameter sets.
    let status = unsafe {
        CMVideoFormatDescriptionCreateFromH264ParameterSets(
            ptr::null(),
            parameter_sets.len(),
            pointers.as_ptr(),
            sizes.as_ptr(),
            length_size as i32,
            &mut format_description,
        )
    };
    if status != 0 {
        return Err(os_status_error(
            codec,
            "open VideoToolbox decoder",
            "CMVideoFormatDescriptionCreateFromH264ParameterSets",
            status,
        ));
    }
    Ok(format_description)
}

fn create_hevc_format_description(
    codec: &CodecId,
    parameter_sets: &[Bytes],
    length_size: usize,
) -> Result<CMVideoFormatDescriptionRef> {
    for required in 32..=34 {
        if !parameter_sets
            .iter()
            .any(|nal| nal.len() >= 2 && ((nal[0] >> 1) & 0x3f) == required)
        {
            return platform_codec_error(
                codec,
                "open VideoToolbox decoder",
                "hvcC extra_data must contain VPS, SPS, and PPS for VideoToolbox",
            );
        }
    }
    let pointers: Vec<*const u8> = parameter_sets.iter().map(|nal| nal.as_ptr()).collect();
    let sizes: Vec<usize> = parameter_sets.iter().map(Bytes::len).collect();
    let mut format_description = ptr::null_mut();
    // SAFETY: parameter set pointers/sizes refer to Bytes values that live for
    // the duration of this call. CoreMedia copies/parses the parameter sets.
    let status = unsafe {
        CMVideoFormatDescriptionCreateFromHEVCParameterSets(
            ptr::null(),
            parameter_sets.len(),
            pointers.as_ptr(),
            sizes.as_ptr(),
            length_size as i32,
            ptr::null(),
            &mut format_description,
        )
    };
    if status != 0 {
        return Err(os_status_error(
            codec,
            "open VideoToolbox decoder",
            "CMVideoFormatDescriptionCreateFromHEVCParameterSets",
            status,
        ));
    }
    Ok(format_description)
}

fn create_bgra_decoder_attributes(codec: &CodecId) -> Result<CFDictionaryRef> {
    let pixel_format = K_CV_PIXEL_FORMAT_TYPE_32_BGRA as i32;
    // SAFETY: CFNumberCreate reads a valid SInt32 value.
    let number = unsafe {
        CFNumberCreate(
            ptr::null(),
            K_CF_NUMBER_SINT32_TYPE,
            (&pixel_format as *const i32).cast(),
        )
    };
    if number.is_null() {
        return platform_codec_error(
            codec,
            "open VideoToolbox decoder",
            "CFNumberCreate(kCVPixelBufferPixelFormatTypeKey) returned null",
        );
    }
    // SAFETY: kCVPixelBufferPixelFormatTypeKey is a process-lifetime CFString.
    let key = unsafe { kCVPixelBufferPixelFormatTypeKey };
    let keys = [key];
    let values = [number];
    // SAFETY: keys/values are valid for this call; CFType callbacks retain the
    // values inside the returned dictionary.
    let attrs = unsafe {
        CFDictionaryCreate(
            ptr::null(),
            keys.as_ptr(),
            values.as_ptr(),
            1,
            &kCFTypeDictionaryKeyCallBacks,
            &kCFTypeDictionaryValueCallBacks,
        )
    };
    // SAFETY: The dictionary retained the number when creation succeeded.
    unsafe {
        CFRelease(number);
    }
    if attrs.is_null() {
        return platform_codec_error(
            codec,
            "open VideoToolbox decoder",
            "CFDictionaryCreate(pixel buffer attributes) returned null",
        );
    }
    Ok(attrs)
}

fn video_decoder_sample_data(
    codec: &CodecId,
    packet: &EncodedPacket,
    extra_data: &Bytes,
) -> Result<Bytes> {
    match codec {
        CodecId::H264 => {
            if extra_data.is_empty() {
                return platform_codec_error(
                    codec,
                    "decode VideoToolbox packet",
                    "H.264 VideoToolbox decode requires avcC extra_data so packets can be length-prefixed",
                );
            }
            h264_packet_to_length_prefixed(packet, extra_data)
        }
        CodecId::H265 => {
            if extra_data.is_empty() {
                return platform_codec_error(
                    codec,
                    "decode VideoToolbox packet",
                    "HEVC VideoToolbox decode requires hvcC extra_data so packets can be length-prefixed",
                );
            }
            h265_packet_to_length_prefixed(packet, extra_data)
        }
        CodecId::ProRes => Ok(packet.data.clone()),
        _ => platform_codec_error(
            codec,
            "decode VideoToolbox packet",
            "codec is not supported by VideoToolbox decoder",
        ),
    }
}

fn create_video_sample_buffer(
    codec: &CodecId,
    format_description: CMVideoFormatDescriptionRef,
    data: &Bytes,
    packet: &EncodedPacket,
) -> Result<(CMBlockBufferRef, CMSampleBufferRef)> {
    if data.is_empty() {
        return platform_codec_error(
            codec,
            "decode VideoToolbox packet",
            "encoded video packet is empty",
        );
    }
    let mut block = ptr::null_mut();
    // SAFETY: data points to immutable packet bytes that remain alive until the
    // synchronous decode call completes. kCFAllocatorNull tells CoreMedia that
    // Rust owns the backing memory and CoreMedia must not free it.
    let status = unsafe {
        CMBlockBufferCreateWithMemoryBlock(
            ptr::null(),
            data.as_ptr() as *mut c_void,
            data.len(),
            kCFAllocatorNull,
            ptr::null(),
            0,
            data.len(),
            0,
            &mut block,
        )
    };
    if status != 0 {
        return Err(os_status_error(
            codec,
            "decode VideoToolbox packet",
            "CMBlockBufferCreateWithMemoryBlock",
            status,
        ));
    }
    let timing = CMSampleTimingInfo {
        duration: cm_time(packet.duration.max(0), packet.time_base)?,
        presentation_time_stamp: cm_time(packet.pts, packet.time_base)?,
        decode_time_stamp: packet
            .dts
            .map(|dts| cm_time(dts, packet.time_base))
            .transpose()?
            .unwrap_or_else(cm_time_invalid),
    };
    let sample_size = data.len();
    let mut sample = ptr::null_mut();
    // SAFETY: block/format_description are valid CoreMedia objects, and timing
    // and sample size pointers are valid for the duration of the call.
    let status = unsafe {
        CMSampleBufferCreateReady(
            ptr::null(),
            block,
            format_description,
            1,
            1,
            &timing,
            1,
            &sample_size,
            &mut sample,
        )
    };
    if status != 0 {
        // SAFETY: block was created above and not transferred.
        unsafe {
            CFRelease(block.cast());
        }
        return Err(os_status_error(
            codec,
            "decode VideoToolbox packet",
            "CMSampleBufferCreateReady",
            status,
        ));
    }
    Ok((block, sample))
}

fn video_codec_type(codec: &CodecId) -> Option<OSType> {
    match codec {
        CodecId::H264 => Some(K_CM_VIDEO_CODEC_TYPE_H264),
        CodecId::H265 => Some(K_CM_VIDEO_CODEC_TYPE_HEVC),
        CodecId::ProRes => Some(K_CM_VIDEO_CODEC_TYPE_APPLE_PRO_RES_422),
        _ => None,
    }
}

fn audio_format_id(codec: &CodecId) -> Option<OSType> {
    match codec {
        CodecId::Aac => Some(K_AUDIO_FORMAT_MPEG4_AAC),
        CodecId::Eac3 => Some(K_AUDIO_FORMAT_ENHANCED_AC3),
        _ => None,
    }
}

fn compressed_asbd(
    codec: &CodecId,
    sample_rate: u32,
    channels: u16,
) -> Result<AudioStreamBasicDescription> {
    let format_id = audio_format_id(codec).ok_or_else(|| Error::CodecBackend {
        codec: codec.clone(),
        operation: "open AudioToolbox converter",
        message: "codec has no AudioToolbox mapping".to_owned(),
    })?;

    Ok(AudioStreamBasicDescription {
        sample_rate: f64::from(sample_rate),
        format_id,
        format_flags: 0,
        bytes_per_packet: 0,
        frames_per_packet: if *codec == CodecId::Eac3 { 1536 } else { 1024 },
        bytes_per_frame: 0,
        channels_per_frame: u32::from(channels),
        bits_per_channel: 0,
        reserved: 0,
    })
}

fn pcm_asbd(sample_rate: u32, channels: u16) -> AudioStreamBasicDescription {
    let bytes_per_frame = u32::from(channels) * 4;
    AudioStreamBasicDescription {
        sample_rate: f64::from(sample_rate),
        format_id: K_AUDIO_FORMAT_LINEAR_PCM,
        format_flags: K_AUDIO_FORMAT_FLAG_IS_FLOAT | K_AUDIO_FORMAT_FLAG_IS_PACKED,
        bytes_per_packet: bytes_per_frame,
        frames_per_packet: 1,
        bytes_per_frame,
        channels_per_frame: u32::from(channels),
        bits_per_channel: 32,
        reserved: 0,
    }
}

extern "C" fn decompression_output_callback(
    decompression_output_ref_con: *mut c_void,
    _source_frame_ref_con: *mut c_void,
    status: OSStatus,
    _info_flags: VTDecodeInfoFlags,
    image_buffer: CVImageBufferRef,
    _presentation_time_stamp: CMTime,
    _presentation_duration: CMTime,
) {
    if decompression_output_ref_con.is_null() {
        return;
    }
    // SAFETY: open_video_decoder passes a pointer to the VideoDecoderState
    // boxed inside VideoDecoderHandle. The session is invalidated before that
    // state is dropped.
    let state = unsafe { &*(decompression_output_ref_con as *mut VideoDecoderState) };
    if status != 0 {
        push_decoder_error(
            state,
            format!("VideoToolbox callback returned OSStatus {status}"),
        );
        return;
    }
    if image_buffer.is_null() {
        return;
    }
    match copy_bgra_image_buffer_to_rgba_frame(state, image_buffer.cast()) {
        Ok(frame) => match state.frames.lock() {
            Ok(mut frames) => frames.push(frame),
            Err(_) => push_decoder_error(state, "frame lock is poisoned".to_owned()),
        },
        Err(message) => push_decoder_error(state, message),
    }
}

fn push_decoder_error(state: &VideoDecoderState, message: String) {
    if let Ok(mut errors) = state.errors.lock() {
        errors.push(message);
    }
}

fn copy_bgra_image_buffer_to_rgba_frame(
    state: &VideoDecoderState,
    pixel_buffer: CVPixelBufferRef,
) -> std::result::Result<RgbaFrame, String> {
    // SAFETY: pixel_buffer is provided by VideoToolbox for the callback duration.
    let format = unsafe { CVPixelBufferGetPixelFormatType(pixel_buffer) };
    if format != K_CV_PIXEL_FORMAT_TYPE_32_BGRA {
        return Err(format!(
            "VideoToolbox returned pixel format 0x{format:08x}, expected 32BGRA"
        ));
    }
    // SAFETY: Lock grants read access to the base address until unlock.
    let status =
        unsafe { CVPixelBufferLockBaseAddress(pixel_buffer, K_CV_PIXEL_BUFFER_LOCK_READ_ONLY) };
    if status != 0 {
        return Err(format!(
            "CVPixelBufferLockBaseAddress returned CVReturn {status}"
        ));
    }

    let result = (|| {
        // SAFETY: The pixel buffer is locked by this function.
        let base = unsafe { CVPixelBufferGetBaseAddress(pixel_buffer) };
        if base.is_null() {
            return Err("CVPixelBufferGetBaseAddress returned null".to_owned());
        }
        // SAFETY: The pixel buffer is locked by this function.
        let src_stride = unsafe { CVPixelBufferGetBytesPerRow(pixel_buffer) };
        // SAFETY: The pixel buffer is valid for the callback duration.
        let width = unsafe { CVPixelBufferGetWidth(pixel_buffer) } as u32;
        // SAFETY: The pixel buffer is valid for the callback duration.
        let height = unsafe { CVPixelBufferGetHeight(pixel_buffer) } as u32;
        let width = if width == 0 { state.width } else { width };
        let height = if height == 0 { state.height } else { height };
        let row_bytes = width as usize * 4;
        if src_stride < row_bytes {
            return Err(format!(
                "pixel buffer stride {src_stride} is smaller than {row_bytes}"
            ));
        }
        let mut data = vec![0u8; row_bytes * height as usize];
        for row in 0..height as usize {
            let src_offset = row * src_stride;
            let dst_offset = row * row_bytes;
            // SAFETY: The source row lies inside the locked pixel buffer.
            let src =
                unsafe { slice::from_raw_parts((base as *const u8).add(src_offset), row_bytes) };
            for (bgra, rgba) in src
                .chunks_exact(4)
                .zip(data[dst_offset..dst_offset + row_bytes].chunks_exact_mut(4))
            {
                rgba.copy_from_slice(&[bgra[2], bgra[1], bgra[0], bgra[3]]);
            }
        }
        RgbaFrame::new(width, height, row_bytes, data).map_err(|error| error.to_string())
    })();

    // SAFETY: Balances CVPixelBufferLockBaseAddress above.
    let unlock_status =
        unsafe { CVPixelBufferUnlockBaseAddress(pixel_buffer, K_CV_PIXEL_BUFFER_LOCK_READ_ONLY) };
    if unlock_status != 0 {
        return Err(format!(
            "CVPixelBufferUnlockBaseAddress returned CVReturn {unlock_status}"
        ));
    }
    result
}

extern "C" fn compression_output_callback(
    output_callback_ref_con: *mut c_void,
    _source_frame_ref_con: *mut c_void,
    status: OSStatus,
    _info_flags: VTEncodeInfoFlags,
    sample_buffer: CMSampleBufferRef,
) {
    if output_callback_ref_con.is_null() {
        return;
    }
    // SAFETY: open_video_encoder passes a pointer to the VideoEncoderState
    // boxed inside VideoEncoderHandle. The session is invalidated before that
    // state is dropped.
    let state = unsafe { &*(output_callback_ref_con as *mut VideoEncoderState) };
    if status != 0 {
        push_encoder_error(
            state,
            format!("VideoToolbox callback returned OSStatus {status}"),
        );
        return;
    }
    if sample_buffer.is_null() {
        return;
    }

    match sample_buffer_data(sample_buffer) {
        Ok(data) => {
            if let Ok(Some(config)) = sample_buffer_codec_config(&state.codec, sample_buffer)
                && let Ok(mut cached) = state.codec_config.lock()
                && cached.is_none()
            {
                *cached = Some(config);
            }
            // SAFETY: The sample buffer pointer is provided by VideoToolbox for
            // the duration of the callback.
            let (pts, duration) = unsafe {
                (
                    CMSampleBufferGetPresentationTimeStamp(sample_buffer),
                    CMSampleBufferGetDuration(sample_buffer),
                )
            };
            match state.packets.lock() {
                Ok(mut packets) => packets.push(AppleEncodedPacket {
                    pts,
                    duration,
                    data,
                }),
                Err(_) => push_encoder_error(state, "packet lock is poisoned".to_owned()),
            }
        }
        Err(message) => push_encoder_error(state, message),
    }
}

fn push_encoder_error(state: &VideoEncoderState, message: String) {
    if let Ok(mut errors) = state.errors.lock() {
        errors.push(message);
    }
}

fn create_bgra_pixel_buffer(codec: &CodecId, frame: &RgbaFrame) -> Result<CVPixelBufferRef> {
    let mut pixel_buffer = ptr::null_mut();
    // SAFETY: Creates a CoreVideo pixel buffer with default allocator and no
    // extra attributes. The returned reference is released by the caller.
    let status = unsafe {
        CVPixelBufferCreate(
            ptr::null(),
            frame.width as usize,
            frame.height as usize,
            K_CV_PIXEL_FORMAT_TYPE_32_BGRA,
            ptr::null(),
            &mut pixel_buffer,
        )
    };
    if status != 0 || pixel_buffer.is_null() {
        return Err(Error::CodecBackend {
            codec: codec.clone(),
            operation: "create VideoToolbox pixel buffer",
            message: format!("CVPixelBufferCreate returned CVReturn {status}"),
        });
    }

    // SAFETY: Lock grants mutable access to the base address until unlock.
    let status = unsafe { CVPixelBufferLockBaseAddress(pixel_buffer, 0) };
    if status != 0 {
        // SAFETY: The pixel buffer was created above and not transferred.
        unsafe {
            CFRelease(pixel_buffer.cast());
        }
        return Err(Error::CodecBackend {
            codec: codec.clone(),
            operation: "lock VideoToolbox pixel buffer",
            message: format!("CVPixelBufferLockBaseAddress returned CVReturn {status}"),
        });
    }

    let result = copy_rgba_to_bgra_pixel_buffer(codec, frame, pixel_buffer);
    // SAFETY: Balances CVPixelBufferLockBaseAddress above.
    let unlock_status = unsafe { CVPixelBufferUnlockBaseAddress(pixel_buffer, 0) };
    if let Err(error) = result {
        // SAFETY: The pixel buffer was created above and not transferred.
        unsafe {
            CFRelease(pixel_buffer.cast());
        }
        return Err(error);
    }
    if unlock_status != 0 {
        // SAFETY: The pixel buffer was created above and not transferred.
        unsafe {
            CFRelease(pixel_buffer.cast());
        }
        return Err(Error::CodecBackend {
            codec: codec.clone(),
            operation: "unlock VideoToolbox pixel buffer",
            message: format!("CVPixelBufferUnlockBaseAddress returned CVReturn {unlock_status}"),
        });
    }

    Ok(pixel_buffer)
}

fn copy_rgba_to_bgra_pixel_buffer(
    codec: &CodecId,
    frame: &RgbaFrame,
    pixel_buffer: CVPixelBufferRef,
) -> Result<()> {
    // SAFETY: The pixel buffer is locked by the caller.
    let base = unsafe { CVPixelBufferGetBaseAddress(pixel_buffer) };
    if base.is_null() {
        return Err(Error::CodecBackend {
            codec: codec.clone(),
            operation: "write VideoToolbox pixel buffer",
            message: "CVPixelBufferGetBaseAddress returned null".to_owned(),
        });
    }
    // SAFETY: The pixel buffer is locked by the caller.
    let dst_stride = unsafe { CVPixelBufferGetBytesPerRow(pixel_buffer) };
    let src_row_bytes = frame.width as usize * 4;
    if dst_stride < src_row_bytes {
        return Err(Error::CodecBackend {
            codec: codec.clone(),
            operation: "write VideoToolbox pixel buffer",
            message: format!("pixel buffer stride {dst_stride} is smaller than {src_row_bytes}"),
        });
    }
    for row in 0..frame.height as usize {
        let src_offset = row * frame.stride;
        let dst_offset = row * dst_stride;
        // SAFETY: The destination row lies inside the locked pixel buffer, and
        // the source row was validated when RgbaFrame was constructed.
        let dst =
            unsafe { slice::from_raw_parts_mut((base as *mut u8).add(dst_offset), dst_stride) };
        for (src, dst) in frame.data[src_offset..src_offset + src_row_bytes]
            .chunks_exact(4)
            .zip(dst[..src_row_bytes].chunks_exact_mut(4))
        {
            dst.copy_from_slice(&[src[2], src[1], src[0], src[3]]);
        }
    }
    Ok(())
}

fn sample_buffer_data(sample_buffer: CMSampleBufferRef) -> std::result::Result<Vec<u8>, String> {
    // SAFETY: The sample buffer is supplied by VideoToolbox for the callback duration.
    let block = unsafe { CMSampleBufferGetDataBuffer(sample_buffer) };
    if block.is_null() {
        return Err("CMSampleBufferGetDataBuffer returned null".to_owned());
    }

    let mut total_len = 0usize;
    let mut data_ptr = ptr::null_mut();
    // SAFETY: Requests a contiguous pointer into the callback-owned block buffer.
    let status = unsafe {
        CMBlockBufferGetDataPointer(block, 0, ptr::null_mut(), &mut total_len, &mut data_ptr)
    };
    if status != 0 {
        return Err(format!(
            "CMBlockBufferGetDataPointer returned OSStatus {status}"
        ));
    }
    if data_ptr.is_null() || total_len == 0 {
        return Ok(Vec::new());
    }

    // SAFETY: CoreMedia reported a contiguous readable block of total_len bytes.
    Ok(unsafe { slice::from_raw_parts(data_ptr.cast::<u8>(), total_len) }.to_vec())
}

fn sample_buffer_codec_config(
    codec: &CodecId,
    sample_buffer: CMSampleBufferRef,
) -> std::result::Result<Option<Vec<u8>>, String> {
    // SAFETY: The sample buffer is supplied by VideoToolbox for the callback duration.
    let format_description = unsafe { CMSampleBufferGetFormatDescription(sample_buffer) };
    if format_description.is_null() {
        return Ok(None);
    }
    match codec {
        CodecId::H264 => h264_avcc_from_format_description(format_description).map(Some),
        CodecId::H265 => hevc_hvcc_from_format_description(format_description).map(Some),
        _ => Ok(None),
    }
}

fn h264_avcc_from_format_description(
    format_description: CMFormatDescriptionRef,
) -> std::result::Result<Vec<u8>, String> {
    let (parameter_sets, length_size) =
        h264_parameter_sets_from_format_description(format_description)?;
    let sps: Vec<_> = parameter_sets
        .iter()
        .filter(|nal| !nal.is_empty() && nal[0] & 0x1f == 7)
        .collect();
    let pps: Vec<_> = parameter_sets
        .iter()
        .filter(|nal| !nal.is_empty() && nal[0] & 0x1f == 8)
        .collect();
    let first_sps = sps
        .first()
        .ok_or_else(|| "H.264 format description did not contain an SPS".to_owned())?;
    if pps.is_empty() {
        return Err("H.264 format description did not contain a PPS".to_owned());
    }
    if first_sps.len() < 4 {
        return Err("H.264 SPS is too short to build avcC".to_owned());
    }
    let length_size_minus_one = nal_length_size_minus_one(length_size)?;
    let mut out = vec![
        1,
        first_sps[1],
        first_sps[2],
        first_sps[3],
        0xfc | length_size_minus_one,
        0xe0 | u8::try_from(sps.len()).unwrap_or(31).min(31),
    ];
    for nal in sps {
        write_config_nal(&mut out, nal)?;
    }
    out.push(u8::try_from(pps.len()).unwrap_or(u8::MAX));
    for nal in pps {
        write_config_nal(&mut out, nal)?;
    }
    Ok(out)
}

fn hevc_hvcc_from_format_description(
    format_description: CMFormatDescriptionRef,
) -> std::result::Result<Vec<u8>, String> {
    let (parameter_sets, length_size) =
        hevc_parameter_sets_from_format_description(format_description)?;
    let mut grouped = Vec::<(u8, Vec<&Vec<u8>>)>::new();
    for target in [32, 33, 34] {
        let nals: Vec<_> = parameter_sets
            .iter()
            .filter(|nal| nal.len() >= 2 && ((nal[0] >> 1) & 0x3f) == target)
            .collect();
        if nals.is_empty() {
            return Err(format!(
                "HEVC format description did not contain NAL type {target}"
            ));
        }
        grouped.push((target, nals));
    }
    let mut out = vec![0u8; 23];
    out[0] = 1;
    out[21] = 0xfc | nal_length_size_minus_one(length_size)?;
    out[22] = grouped.len() as u8;
    for (nal_type, nals) in grouped {
        out.push(0x80 | nal_type);
        out.extend_from_slice(&(nals.len() as u16).to_be_bytes());
        for nal in nals {
            write_config_nal(&mut out, nal)?;
        }
    }
    Ok(out)
}

fn h264_parameter_sets_from_format_description(
    format_description: CMFormatDescriptionRef,
) -> std::result::Result<(Vec<Vec<u8>>, i32), String> {
    let mut count = 0usize;
    let mut length_size = 0i32;
    // SAFETY: Asking CoreMedia for count/length only; null pointer/size outputs
    // are allowed by the API when parameterSetIndex is ignored.
    let status = unsafe {
        CMVideoFormatDescriptionGetH264ParameterSetAtIndex(
            format_description,
            0,
            ptr::null_mut(),
            ptr::null_mut(),
            &mut count,
            &mut length_size,
        )
    };
    if status != 0 {
        return Err(format!(
            "CMVideoFormatDescriptionGetH264ParameterSetAtIndex(count) returned OSStatus {status}"
        ));
    }
    let mut parameter_sets = Vec::with_capacity(count);
    for index in 0..count {
        let mut ptr_out = ptr::null();
        let mut size = 0usize;
        // SAFETY: CoreMedia returns a pointer valid while the format description
        // is alive; we copy it immediately.
        let status = unsafe {
            CMVideoFormatDescriptionGetH264ParameterSetAtIndex(
                format_description,
                index,
                &mut ptr_out,
                &mut size,
                ptr::null_mut(),
                ptr::null_mut(),
            )
        };
        if status != 0 {
            return Err(format!(
                "CMVideoFormatDescriptionGetH264ParameterSetAtIndex({index}) returned OSStatus {status}"
            ));
        }
        if !ptr_out.is_null() && size > 0 {
            // SAFETY: CoreMedia reported a readable parameter set of size bytes.
            parameter_sets.push(unsafe { slice::from_raw_parts(ptr_out, size) }.to_vec());
        }
    }
    Ok((parameter_sets, length_size))
}

fn hevc_parameter_sets_from_format_description(
    format_description: CMFormatDescriptionRef,
) -> std::result::Result<(Vec<Vec<u8>>, i32), String> {
    let mut count = 0usize;
    let mut length_size = 0i32;
    // SAFETY: Asking CoreMedia for count/length only; null pointer/size outputs
    // are allowed by the API when parameterSetIndex is ignored.
    let status = unsafe {
        CMVideoFormatDescriptionGetHEVCParameterSetAtIndex(
            format_description,
            0,
            ptr::null_mut(),
            ptr::null_mut(),
            &mut count,
            &mut length_size,
        )
    };
    if status != 0 {
        return Err(format!(
            "CMVideoFormatDescriptionGetHEVCParameterSetAtIndex(count) returned OSStatus {status}"
        ));
    }
    let mut parameter_sets = Vec::with_capacity(count);
    for index in 0..count {
        let mut ptr_out = ptr::null();
        let mut size = 0usize;
        // SAFETY: CoreMedia returns a pointer valid while the format description
        // is alive; we copy it immediately.
        let status = unsafe {
            CMVideoFormatDescriptionGetHEVCParameterSetAtIndex(
                format_description,
                index,
                &mut ptr_out,
                &mut size,
                ptr::null_mut(),
                ptr::null_mut(),
            )
        };
        if status != 0 {
            return Err(format!(
                "CMVideoFormatDescriptionGetHEVCParameterSetAtIndex({index}) returned OSStatus {status}"
            ));
        }
        if !ptr_out.is_null() && size > 0 {
            // SAFETY: CoreMedia reported a readable parameter set of size bytes.
            parameter_sets.push(unsafe { slice::from_raw_parts(ptr_out, size) }.to_vec());
        }
    }
    Ok((parameter_sets, length_size))
}

fn nal_length_size_minus_one(length_size: i32) -> std::result::Result<u8, String> {
    match length_size {
        1 | 2 | 4 => Ok((length_size as u8) - 1),
        _ => Err(format!("unsupported NAL length size {length_size}")),
    }
}

fn write_config_nal(out: &mut Vec<u8>, nal: &[u8]) -> std::result::Result<(), String> {
    let len = u16::try_from(nal.len())
        .map_err(|_| "parameter set is too large for codec config".to_owned())?;
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(nal);
    Ok(())
}

struct AudioInputProcState<'a> {
    data: &'a [u8],
    buffer_channels: u32,
    packet_count: u32,
    bytes_per_packet: usize,
    packet_description: Option<&'a mut AudioStreamPacketDescription>,
    packet_offset: u32,
}

extern "C" fn audio_input_proc(
    _converter: AudioConverterRef,
    io_number_data_packets: *mut u32,
    io_data: *mut AudioBufferList,
    out_data_packet_description: *mut *mut AudioStreamPacketDescription,
    user_data: *mut c_void,
) -> OSStatus {
    if user_data.is_null() || io_number_data_packets.is_null() || io_data.is_null() {
        return -50;
    }
    // SAFETY: AudioConverterFillComplexBuffer passes the state pointer supplied
    // by decode/encode for the duration of this callback.
    let state = unsafe { &mut *(user_data as *mut AudioInputProcState<'_>) };
    let requested_packets = unsafe { *io_number_data_packets };
    let remaining_packets = state.packet_count.saturating_sub(state.packet_offset);
    if requested_packets == 0
        || remaining_packets == 0
        || state.bytes_per_packet == 0
        || state.data.is_empty()
    {
        // SAFETY: Output pointers are checked above.
        unsafe {
            *io_number_data_packets = 0;
            (*io_data).number_buffers = 1;
            (*io_data).buffers[0] = AudioBuffer {
                number_channels: state.buffer_channels,
                data_byte_size: 0,
                data: ptr::null_mut(),
            };
            if !out_data_packet_description.is_null() {
                *out_data_packet_description = ptr::null_mut();
            }
        }
        return 0;
    }

    let packets_to_provide = requested_packets.min(remaining_packets);
    let start = match usize::try_from(state.packet_offset)
        .ok()
        .and_then(|packets| packets.checked_mul(state.bytes_per_packet))
    {
        Some(start) => start,
        None => return -50,
    };
    let byte_len = match usize::try_from(packets_to_provide)
        .ok()
        .and_then(|packets| packets.checked_mul(state.bytes_per_packet))
    {
        Some(byte_len) => byte_len,
        None => return -50,
    };
    if start > state.data.len() || state.data.len().saturating_sub(start) < byte_len {
        return -50;
    }
    let data_byte_size = match u32::try_from(byte_len) {
        Ok(data_byte_size) => data_byte_size,
        Err(_) => return -50,
    };
    state.packet_offset = state.packet_offset.saturating_add(packets_to_provide);

    // SAFETY: Output pointers are checked above. The data slice lives until
    // FillComplexBuffer returns.
    unsafe {
        *io_number_data_packets = packets_to_provide;
        (*io_data).number_buffers = 1;
        (*io_data).buffers[0] = AudioBuffer {
            number_channels: state.buffer_channels,
            data_byte_size,
            data: state.data[start..start + byte_len].as_ptr() as *mut c_void,
        };
        if !out_data_packet_description.is_null() {
            *out_data_packet_description = state
                .packet_description
                .as_deref_mut()
                .map_or(ptr::null_mut(), |description| description as *mut _);
        }
    }
    0
}

fn audio_buffer_list(channels: u16, data: &mut [u8]) -> AudioBufferList {
    AudioBufferList {
        number_buffers: 1,
        buffers: [AudioBuffer {
            number_channels: u32::from(channels),
            data_byte_size: data.len() as u32,
            data: data.as_mut_ptr().cast(),
        }],
    }
}

fn audio_decoder_input(codec: &CodecId, packet: &EncodedPacket) -> Result<Bytes> {
    match codec {
        CodecId::Aac => aac_packet_to_raw(packet),
        CodecId::Eac3 => Ok(packet.data.clone()),
        _ => platform_codec_error(
            codec,
            "decode AudioToolbox packet",
            "codec is not supported by AudioToolbox decoder",
        ),
    }
}

fn f32_samples_from_ne_bytes(bytes: &[u8]) -> Vec<f32> {
    bytes
        .chunks_exact(4)
        .map(|chunk| f32::from_ne_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
        .collect()
}

fn f32_samples_to_ne_bytes(samples: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(samples.len() * 4);
    for sample in samples {
        out.extend_from_slice(&sample.to_ne_bytes());
    }
    out
}

fn zeroed_packet_descriptions(count: usize) -> Vec<AudioStreamPacketDescription> {
    vec![
        AudioStreamPacketDescription {
            start_offset: 0,
            variable_frames_in_packet: 0,
            data_byte_size: 0,
        };
        count.max(1)
    ]
}

fn encoded_audio_packets_from_output(
    codec: &CodecId,
    output: &[u8],
    descriptions: &[AudioStreamPacketDescription],
    packet_count: u32,
    pts: i64,
    fallback_duration: i64,
    time_base: TimeBase,
) -> Vec<EncodedPacket> {
    if output.is_empty() || packet_count == 0 {
        return Vec::new();
    }
    let described_packets: Vec<_> = descriptions
        .iter()
        .take(packet_count as usize)
        .filter(|description| description.data_byte_size > 0)
        .collect();
    if described_packets.is_empty() {
        return vec![EncodedPacket::new(
            DEFAULT_TRACK_ID,
            codec.clone(),
            pts,
            fallback_duration,
            time_base,
            output.to_vec(),
        )];
    }

    let mut packets = Vec::with_capacity(described_packets.len());
    let mut next_pts = pts;
    for description in described_packets {
        let start = description.start_offset.max(0) as usize;
        let len = description.data_byte_size as usize;
        if start >= output.len() || output.len().saturating_sub(start) < len {
            continue;
        }
        let duration = if description.variable_frames_in_packet == 0 {
            fallback_duration
        } else {
            i64::from(description.variable_frames_in_packet)
        };
        packets.push(EncodedPacket::new(
            DEFAULT_TRACK_ID,
            codec.clone(),
            next_pts,
            duration,
            time_base,
            output[start..start + len].to_vec(),
        ));
        next_pts += duration;
    }
    packets
}

fn cm_time(ticks: i64, time_base: TimeBase) -> Result<CMTime> {
    Ok(CMTime {
        value: time_base.rescale(ticks, TimeBase::new(1, CM_TIME_SCALE)?),
        timescale: CM_TIME_SCALE,
        flags: K_CM_TIME_FLAGS_VALID,
        epoch: 0,
    })
}

fn cm_time_invalid() -> CMTime {
    CMTime {
        value: 0,
        timescale: 0,
        flags: 0,
        epoch: 0,
    }
}

fn cm_time_to_ticks(time: CMTime, time_base: TimeBase) -> i64 {
    if time.flags & K_CM_TIME_FLAGS_VALID == 0 || time.timescale <= 0 {
        return 0;
    }
    TimeBase::new(1, time.timescale)
        .map(|source| source.rescale(time.value, time_base))
        .unwrap_or(0)
}

fn os_status_error(
    codec: &CodecId,
    operation: &'static str,
    api: &'static str,
    status: OSStatus,
) -> Error {
    Error::CodecBackend {
        codec: codec.clone(),
        operation,
        message: format!("{api} returned OSStatus {status}"),
    }
}
