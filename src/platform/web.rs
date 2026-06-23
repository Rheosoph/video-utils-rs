use super::{
    PlatformAudioDecoderConfig, PlatformAudioEncoderConfig, PlatformCodecProbe,
    PlatformVideoDecoderConfig, PlatformVideoEncoderConfig, platform_codec_error,
};
use crate::{
    audio::AudioFrame,
    backend::BackendKind,
    bitstream::{
        aac::aac_packet_to_raw, h264::h264_packet_to_length_prefixed,
        h265::h265_packet_to_length_prefixed,
    },
    codec::{CodecDirection, CodecId},
    error::{Error, Result},
    frame::RgbaFrame,
    packet::EncodedPacket,
    time::TimeBase,
};
use bytes::Bytes;
use js_sys::{Array, Float32Array, Function, Object, Promise, Reflect, Uint8Array};
use std::{cell::RefCell, rc::Rc};
use wasm_bindgen::{JsCast, JsValue, closure::Closure};
use wasm_bindgen_futures::JsFuture;

pub struct DecoderHandle {
    codec: CodecId,
}

pub struct EncoderHandle {
    codec: CodecId,
}

/// Async WebCodecs video decoder for wasm/browser callers.
pub struct AsyncWebCodecsVideoDecoder {
    codec: CodecId,
    decoder: JsValue,
    extra_data: Bytes,
    output_frames: Rc<RefCell<Vec<JsValue>>>,
    errors: Rc<RefCell<Vec<String>>>,
    _output_callback: Closure<dyn FnMut(JsValue)>,
    _error_callback: Closure<dyn FnMut(JsValue)>,
}

/// Async WebCodecs video encoder for wasm/browser callers.
pub struct AsyncWebCodecsVideoEncoder {
    codec: CodecId,
    encoder: JsValue,
    chunks: Rc<RefCell<Vec<WebEncodedChunk>>>,
    errors: Rc<RefCell<Vec<String>>>,
    time_base: TimeBase,
    frame_duration: i64,
    _output_callback: Closure<dyn FnMut(JsValue)>,
    _error_callback: Closure<dyn FnMut(JsValue)>,
}

/// Async WebCodecs audio decoder for wasm/browser callers.
pub struct AsyncWebCodecsAudioDecoder {
    codec: CodecId,
    decoder: JsValue,
    output_audio: Rc<RefCell<Vec<JsValue>>>,
    errors: Rc<RefCell<Vec<String>>>,
    sample_rate: u32,
    channels: u16,
    _output_callback: Closure<dyn FnMut(JsValue)>,
    _error_callback: Closure<dyn FnMut(JsValue)>,
}

/// Async WebCodecs audio encoder for wasm/browser callers.
pub struct AsyncWebCodecsAudioEncoder {
    codec: CodecId,
    encoder: JsValue,
    chunks: Rc<RefCell<Vec<WebEncodedChunk>>>,
    errors: Rc<RefCell<Vec<String>>>,
    sample_rate: u32,
    channels: u16,
    _output_callback: Closure<dyn FnMut(JsValue)>,
    _error_callback: Closure<dyn FnMut(JsValue)>,
}

struct WebEncodedChunk {
    timestamp_us: i64,
    duration_us: Option<i64>,
    data: Vec<u8>,
    keyframe: bool,
}

const DEFAULT_TRACK_ID: u32 = 1;

pub fn probe(codec: &CodecId, direction: CodecDirection) -> PlatformCodecProbe {
    let result = web_codecs_class(codec, direction)
        .ok_or_else(|| "codec has no WebCodecs mapping".to_owned())
        .and_then(|class| {
            if class_available(class) {
                Ok(format!(
                    "{class} constructor is present; exact codec config support requires async isConfigSupported/configure on this runtime"
                ))
            } else {
                Err(format!("{class} constructor is not present on this browser runtime"))
            }
        });

    PlatformCodecProbe {
        backend: Some(BackendKind::WebCodecs),
        codec: codec.clone(),
        direction,
        supported: result.is_ok(),
        detail: result.unwrap_or_else(|message| message),
    }
}

pub fn open_decoder(codec: &CodecId) -> Result<DecoderHandle> {
    ensure_available(codec, CodecDirection::Decode)?;
    Ok(DecoderHandle {
        codec: codec.clone(),
    })
}

pub fn open_encoder(codec: &CodecId) -> Result<EncoderHandle> {
    ensure_available(codec, CodecDirection::Encode)?;
    Ok(EncoderHandle {
        codec: codec.clone(),
    })
}

impl DecoderHandle {
    pub fn decode_video_packet(&mut self, _packet: &EncodedPacket) -> Result<Vec<RgbaFrame>> {
        platform_codec_error(
            &self.codec,
            "decode WebCodecs video packet",
            "WebCodecs output is async; use AsyncWebCodecsVideoDecoder on wasm/browser targets",
        )
    }

    pub fn decode_audio_packet(&mut self, _packet: &EncodedPacket) -> Result<Vec<AudioFrame>> {
        platform_codec_error(
            &self.codec,
            "decode WebCodecs audio packet",
            "WebCodecs output is async; use AsyncWebCodecsAudioDecoder on wasm/browser targets",
        )
    }

    pub fn flush_video_decoder(&mut self) -> Result<Vec<RgbaFrame>> {
        platform_codec_error(
            &self.codec,
            "flush WebCodecs video decoder",
            "WebCodecs flush returns a Promise; the synchronous VideoDecoder trait cannot safely wait for browser callback output",
        )
    }

    pub fn flush_audio_decoder(&mut self) -> Result<Vec<AudioFrame>> {
        platform_codec_error(
            &self.codec,
            "flush WebCodecs audio decoder",
            "WebCodecs flush returns a Promise; the synchronous AudioDecoder trait cannot safely wait for browser callback output",
        )
    }
}

impl EncoderHandle {
    pub fn encode_video_frame(
        &mut self,
        _codec: &CodecId,
        _frame: &RgbaFrame,
        _pts: i64,
    ) -> Result<Vec<EncodedPacket>> {
        platform_codec_error(
            &self.codec,
            "encode WebCodecs video frame",
            "WebCodecs output is async; use AsyncWebCodecsVideoEncoder on wasm/browser targets",
        )
    }

    pub fn encode_audio_frame(
        &mut self,
        _codec: &CodecId,
        _frame: &AudioFrame,
    ) -> Result<Vec<EncodedPacket>> {
        platform_codec_error(
            &self.codec,
            "encode WebCodecs audio frame",
            "WebCodecs output is async; use AsyncWebCodecsAudioEncoder on wasm/browser targets",
        )
    }

    pub fn finish_video_encoder(&mut self, _codec: &CodecId) -> Result<Vec<EncodedPacket>> {
        platform_codec_error(
            &self.codec,
            "finish WebCodecs video encoder",
            "WebCodecs flush returns a Promise; the synchronous VideoEncoder trait cannot safely wait for browser callback output",
        )
    }

    pub fn finish_audio_encoder(&mut self, _codec: &CodecId) -> Result<Vec<EncodedPacket>> {
        platform_codec_error(
            &self.codec,
            "finish WebCodecs audio encoder",
            "WebCodecs flush returns a Promise; the synchronous AudioEncoder trait cannot safely wait for browser callback output",
        )
    }
}

impl AsyncWebCodecsVideoDecoder {
    /// Open and configure a browser WebCodecs `VideoDecoder`.
    pub fn open(config: &PlatformVideoDecoderConfig) -> Result<Self> {
        ensure_available(&config.codec, CodecDirection::Decode)?;
        let output_frames = Rc::new(RefCell::new(Vec::new()));
        let errors = Rc::new(RefCell::new(Vec::new()));
        let output_queue = Rc::clone(&output_frames);
        let error_queue = Rc::clone(&errors);
        let output_callback = Closure::wrap(Box::new(move |frame: JsValue| {
            output_queue.borrow_mut().push(frame);
        }) as Box<dyn FnMut(JsValue)>);
        let error_callback = Closure::wrap(Box::new(move |error: JsValue| {
            error_queue.borrow_mut().push(js_value_message(&error));
        }) as Box<dyn FnMut(JsValue)>);

        let init = Object::new();
        set(&init, "output", output_callback.as_ref())?;
        set(&init, "error", error_callback.as_ref())?;
        let decoder = construct("VideoDecoder", &Array::of1(&init.into()))?;
        let js_config = video_decoder_config(config)?;
        call1(&decoder, "configure", &js_config)?;
        Ok(Self {
            codec: config.codec.clone(),
            decoder,
            extra_data: Bytes::copy_from_slice(&config.extra_data),
            output_frames,
            errors,
            _output_callback: output_callback,
            _error_callback: error_callback,
        })
    }

    /// Decode one packet and await all currently queued decoder output.
    pub async fn decode_packet(&mut self, packet: &EncodedPacket) -> Result<Vec<RgbaFrame>> {
        let chunk = encoded_video_chunk(&self.codec, packet, &self.extra_data)?;
        call1(&self.decoder, "decode", &chunk)?;
        self.flush().await
    }

    /// Await `VideoDecoder.flush()` and drain callback output frames.
    pub async fn flush(&mut self) -> Result<Vec<RgbaFrame>> {
        await_promise(call0(&self.decoder, "flush")?).await?;
        take_web_errors(&self.codec, "flush WebCodecs video decoder", &self.errors)?;
        let frames = self
            .output_frames
            .borrow_mut()
            .drain(..)
            .collect::<Vec<_>>();
        let mut out = Vec::with_capacity(frames.len());
        for frame in frames {
            out.push(video_frame_to_rgba(&self.codec, frame).await?);
        }
        Ok(out)
    }

    /// Reset the browser decoder and discard queued callback output.
    pub fn reset(&mut self) -> Result<()> {
        call0(&self.decoder, "reset")?;
        self.output_frames.borrow_mut().clear();
        self.errors.borrow_mut().clear();
        Ok(())
    }

    /// Close the browser decoder.
    pub fn close(&mut self) -> Result<()> {
        call0(&self.decoder, "close")?;
        Ok(())
    }
}

impl AsyncWebCodecsVideoEncoder {
    /// Open and configure a browser WebCodecs `VideoEncoder`.
    pub fn open(config: &PlatformVideoEncoderConfig) -> Result<Self> {
        ensure_available(&config.codec, CodecDirection::Encode)?;
        let chunks = Rc::new(RefCell::new(Vec::new()));
        let errors = Rc::new(RefCell::new(Vec::new()));
        let chunk_queue = Rc::clone(&chunks);
        let error_queue = Rc::clone(&errors);
        let copy_errors = Rc::clone(&errors);
        let output_callback =
            Closure::wrap(Box::new(
                move |chunk: JsValue| match web_encoded_chunk_from_js(&chunk) {
                    Ok(chunk) => chunk_queue.borrow_mut().push(chunk),
                    Err(message) => copy_errors.borrow_mut().push(message),
                },
            ) as Box<dyn FnMut(JsValue)>);
        let error_callback = Closure::wrap(Box::new(move |error: JsValue| {
            error_queue.borrow_mut().push(js_value_message(&error));
        }) as Box<dyn FnMut(JsValue)>);

        let init = Object::new();
        set(&init, "output", output_callback.as_ref())?;
        set(&init, "error", error_callback.as_ref())?;
        let encoder = construct("VideoEncoder", &Array::of1(&init.into()))?;
        let js_config = video_encoder_config(config)?;
        call1(&encoder, "configure", &js_config)?;
        Ok(Self {
            codec: config.codec.clone(),
            encoder,
            chunks,
            errors,
            time_base: config.time_base,
            frame_duration: config.frame_duration,
            _output_callback: output_callback,
            _error_callback: error_callback,
        })
    }

    /// Encode one RGBA frame and await all currently queued encoder output.
    pub async fn encode_frame(
        &mut self,
        frame: &RgbaFrame,
        pts: i64,
    ) -> Result<Vec<EncodedPacket>> {
        let video_frame = rgba_frame_to_video_frame(
            &self.codec,
            frame,
            self.time_base.rescale(pts, TimeBase::microseconds()),
            self.time_base
                .rescale(self.frame_duration.max(0), TimeBase::microseconds()),
        )?;
        call1(&self.encoder, "encode", &video_frame)?;
        let _ = call0(&video_frame, "close");
        self.finish().await
    }

    /// Await `VideoEncoder.flush()` and drain encoded chunks.
    pub async fn finish(&mut self) -> Result<Vec<EncodedPacket>> {
        await_promise(call0(&self.encoder, "flush")?).await?;
        take_web_errors(&self.codec, "finish WebCodecs video encoder", &self.errors)?;
        Ok(web_chunks_to_packets(
            &self.codec,
            self.chunks.borrow_mut().drain(..).collect(),
            self.time_base,
            self.frame_duration,
        ))
    }

    /// Close the browser encoder.
    pub fn close(&mut self) -> Result<()> {
        call0(&self.encoder, "close")?;
        Ok(())
    }
}

impl AsyncWebCodecsAudioDecoder {
    /// Open and configure a browser WebCodecs `AudioDecoder`.
    pub fn open(config: &PlatformAudioDecoderConfig) -> Result<Self> {
        ensure_available(&config.codec, CodecDirection::Decode)?;
        let output_audio = Rc::new(RefCell::new(Vec::new()));
        let errors = Rc::new(RefCell::new(Vec::new()));
        let output_queue = Rc::clone(&output_audio);
        let error_queue = Rc::clone(&errors);
        let output_callback = Closure::wrap(Box::new(move |audio: JsValue| {
            output_queue.borrow_mut().push(audio);
        }) as Box<dyn FnMut(JsValue)>);
        let error_callback = Closure::wrap(Box::new(move |error: JsValue| {
            error_queue.borrow_mut().push(js_value_message(&error));
        }) as Box<dyn FnMut(JsValue)>);

        let init = Object::new();
        set(&init, "output", output_callback.as_ref())?;
        set(&init, "error", error_callback.as_ref())?;
        let decoder = construct("AudioDecoder", &Array::of1(&init.into()))?;
        let js_config = audio_decoder_config(config)?;
        call1(&decoder, "configure", &js_config)?;
        Ok(Self {
            codec: config.codec.clone(),
            decoder,
            output_audio,
            errors,
            sample_rate: config.sample_rate,
            channels: config.channels,
            _output_callback: output_callback,
            _error_callback: error_callback,
        })
    }

    /// Decode one packet and await all currently queued decoder output.
    pub async fn decode_packet(&mut self, packet: &EncodedPacket) -> Result<Vec<AudioFrame>> {
        let chunk = encoded_audio_chunk(&self.codec, packet)?;
        call1(&self.decoder, "decode", &chunk)?;
        self.flush().await
    }

    /// Await `AudioDecoder.flush()` and drain callback output frames.
    pub async fn flush(&mut self) -> Result<Vec<AudioFrame>> {
        await_promise(call0(&self.decoder, "flush")?).await?;
        take_web_errors(&self.codec, "flush WebCodecs audio decoder", &self.errors)?;
        let outputs = self.output_audio.borrow_mut().drain(..).collect::<Vec<_>>();
        let mut frames = Vec::with_capacity(outputs.len());
        for audio in outputs {
            frames.push(audio_data_to_frame(
                &self.codec,
                audio,
                self.sample_rate,
                self.channels,
            )?);
        }
        Ok(frames)
    }

    /// Reset the browser decoder and discard queued callback output.
    pub fn reset(&mut self) -> Result<()> {
        call0(&self.decoder, "reset")?;
        self.output_audio.borrow_mut().clear();
        self.errors.borrow_mut().clear();
        Ok(())
    }

    /// Close the browser decoder.
    pub fn close(&mut self) -> Result<()> {
        call0(&self.decoder, "close")?;
        Ok(())
    }
}

impl AsyncWebCodecsAudioEncoder {
    /// Open and configure a browser WebCodecs `AudioEncoder`.
    pub fn open(config: &PlatformAudioEncoderConfig) -> Result<Self> {
        ensure_available(&config.codec, CodecDirection::Encode)?;
        let chunks = Rc::new(RefCell::new(Vec::new()));
        let errors = Rc::new(RefCell::new(Vec::new()));
        let chunk_queue = Rc::clone(&chunks);
        let error_queue = Rc::clone(&errors);
        let copy_errors = Rc::clone(&errors);
        let output_callback =
            Closure::wrap(Box::new(
                move |chunk: JsValue| match web_encoded_chunk_from_js(&chunk) {
                    Ok(chunk) => chunk_queue.borrow_mut().push(chunk),
                    Err(message) => copy_errors.borrow_mut().push(message),
                },
            ) as Box<dyn FnMut(JsValue)>);
        let error_callback = Closure::wrap(Box::new(move |error: JsValue| {
            error_queue.borrow_mut().push(js_value_message(&error));
        }) as Box<dyn FnMut(JsValue)>);

        let init = Object::new();
        set(&init, "output", output_callback.as_ref())?;
        set(&init, "error", error_callback.as_ref())?;
        let encoder = construct("AudioEncoder", &Array::of1(&init.into()))?;
        let js_config = audio_encoder_config(config)?;
        call1(&encoder, "configure", &js_config)?;
        Ok(Self {
            codec: config.codec.clone(),
            encoder,
            chunks,
            errors,
            sample_rate: config.sample_rate,
            channels: config.channels,
            _output_callback: output_callback,
            _error_callback: error_callback,
        })
    }

    /// Encode one interleaved f32 frame and await currently queued output.
    pub async fn encode_frame(&mut self, frame: &AudioFrame) -> Result<Vec<EncodedPacket>> {
        if frame.sample_rate != self.sample_rate || frame.channels != self.channels {
            return platform_codec_error(
                &self.codec,
                "encode WebCodecs audio frame",
                format!(
                    "frame audio format {} Hz/{} ch does not match encoder config {} Hz/{} ch",
                    frame.sample_rate, frame.channels, self.sample_rate, self.channels
                ),
            );
        }
        let audio_data = audio_frame_to_audio_data(&self.codec, frame)?;
        call1(&self.encoder, "encode", &audio_data)?;
        let _ = call0(&audio_data, "close");
        self.finish().await
    }

    /// Await `AudioEncoder.flush()` and drain encoded chunks.
    pub async fn finish(&mut self) -> Result<Vec<EncodedPacket>> {
        await_promise(call0(&self.encoder, "flush")?).await?;
        take_web_errors(&self.codec, "finish WebCodecs audio encoder", &self.errors)?;
        Ok(web_chunks_to_packets(
            &self.codec,
            self.chunks.borrow_mut().drain(..).collect(),
            TimeBase::new(1, self.sample_rate as i32)?,
            0,
        ))
    }

    /// Close the browser encoder.
    pub fn close(&mut self) -> Result<()> {
        call0(&self.encoder, "close")?;
        Ok(())
    }
}

fn ensure_available(codec: &CodecId, direction: CodecDirection) -> Result<()> {
    let class = web_codecs_class(codec, direction).ok_or_else(|| Error::CodecBackend {
        codec: codec.clone(),
        operation: "open WebCodecs adapter",
        message: "codec has no WebCodecs mapping".to_owned(),
    })?;
    if class_available(class) {
        Ok(())
    } else {
        Err(Error::CodecBackend {
            codec: codec.clone(),
            operation: "open WebCodecs adapter",
            message: format!("{class} constructor is not present on this browser runtime"),
        })
    }
}

fn class_available(name: &str) -> bool {
    Reflect::get(&js_sys::global(), &JsValue::from_str(name))
        .ok()
        .is_some_and(|value| value.is_function())
}

fn web_codecs_class(codec: &CodecId, direction: CodecDirection) -> Option<&'static str> {
    match (codec.media_type(), direction) {
        (Some(crate::codec::MediaType::Video), CodecDirection::Decode) => Some("VideoDecoder"),
        (Some(crate::codec::MediaType::Video), CodecDirection::Encode) => Some("VideoEncoder"),
        (Some(crate::codec::MediaType::Audio), CodecDirection::Decode) => Some("AudioDecoder"),
        (Some(crate::codec::MediaType::Audio), CodecDirection::Encode) => Some("AudioEncoder"),
        _ => None,
    }
}

fn video_decoder_config(config: &PlatformVideoDecoderConfig) -> Result<JsValue> {
    let object = Object::new();
    set_str(
        &object,
        "codec",
        &video_codec_string(&config.codec, &config.extra_data)?,
    )?;
    set_u32(&object, "codedWidth", config.width)?;
    set_u32(&object, "codedHeight", config.height)?;
    if !config.extra_data.is_empty() {
        let description = Uint8Array::from(config.extra_data.as_slice());
        set(&object, "description", description.as_ref())?;
    }
    Ok(object.into())
}

fn video_encoder_config(config: &PlatformVideoEncoderConfig) -> Result<JsValue> {
    let object = Object::new();
    set_str(&object, "codec", &video_codec_string(&config.codec, &[])?)?;
    set_u32(&object, "width", config.width)?;
    set_u32(&object, "height", config.height)?;
    if let Some(bitrate) = config.bitrate {
        set_u32(&object, "bitrate", bitrate)?;
    }
    Ok(object.into())
}

fn audio_decoder_config(config: &PlatformAudioDecoderConfig) -> Result<JsValue> {
    let object = Object::new();
    set_str(
        &object,
        "codec",
        &audio_codec_string(&config.codec, &config.extra_data)?,
    )?;
    set_u32(&object, "sampleRate", config.sample_rate)?;
    set_u32(&object, "numberOfChannels", u32::from(config.channels))?;
    if !config.extra_data.is_empty() {
        let description = Uint8Array::from(config.extra_data.as_slice());
        set(&object, "description", description.as_ref())?;
    }
    Ok(object.into())
}

fn audio_encoder_config(config: &PlatformAudioEncoderConfig) -> Result<JsValue> {
    let object = Object::new();
    set_str(&object, "codec", &audio_codec_string(&config.codec, &[])?)?;
    set_u32(&object, "sampleRate", config.sample_rate)?;
    set_u32(&object, "numberOfChannels", u32::from(config.channels))?;
    if let Some(bitrate) = config.bitrate {
        set_u32(&object, "bitrate", bitrate)?;
    }
    Ok(object.into())
}

fn encoded_video_chunk(
    codec: &CodecId,
    packet: &EncodedPacket,
    extra_data: &Bytes,
) -> Result<JsValue> {
    let data = match codec {
        CodecId::H264 if !extra_data.is_empty() => {
            h264_packet_to_length_prefixed(packet, extra_data)?
        }
        CodecId::H265 if !extra_data.is_empty() => {
            h265_packet_to_length_prefixed(packet, extra_data)?
        }
        _ => packet.data.clone(),
    };
    encoded_chunk(
        "EncodedVideoChunk",
        if packet.is_keyframe { "key" } else { "delta" },
        packet
            .time_base
            .rescale(packet.pts, TimeBase::microseconds()),
        packet
            .time_base
            .rescale(packet.duration.max(0), TimeBase::microseconds()),
        data.as_ref(),
    )
}

fn encoded_audio_chunk(codec: &CodecId, packet: &EncodedPacket) -> Result<JsValue> {
    let data = match codec {
        CodecId::Aac => aac_packet_to_raw(packet)?,
        _ => packet.data.clone(),
    };
    encoded_chunk(
        "EncodedAudioChunk",
        "key",
        packet
            .time_base
            .rescale(packet.pts, TimeBase::microseconds()),
        packet
            .time_base
            .rescale(packet.duration.max(0), TimeBase::microseconds()),
        data.as_ref(),
    )
}

fn encoded_chunk(
    class_name: &'static str,
    chunk_type: &'static str,
    timestamp_us: i64,
    duration_us: i64,
    data: &[u8],
) -> Result<JsValue> {
    let init = Object::new();
    set_str(&init, "type", chunk_type)?;
    set_f64(&init, "timestamp", timestamp_us as f64)?;
    set_f64(&init, "duration", duration_us as f64)?;
    let bytes = Uint8Array::from(data);
    set(&init, "data", bytes.as_ref())?;
    construct(class_name, &Array::of1(&init.into()))
}

async fn video_frame_to_rgba(codec: &CodecId, frame: JsValue) -> Result<RgbaFrame> {
    let width = js_u32_property(&frame, "displayWidth")
        .or_else(|_| js_u32_property(&frame, "codedWidth"))?;
    let height = js_u32_property(&frame, "displayHeight")
        .or_else(|_| js_u32_property(&frame, "codedHeight"))?;
    let options = Object::new();
    set_str(&options, "format", "RGBA")?;
    let allocation = call1(&frame, "allocationSize", &options.clone().into())?
        .as_f64()
        .ok_or_else(|| Error::CodecBackend {
            codec: codec.clone(),
            operation: "copy WebCodecs VideoFrame",
            message: "VideoFrame.allocationSize did not return a number".to_owned(),
        })? as u32;
    let bytes = Uint8Array::new_with_length(allocation);
    await_promise(call2(&frame, "copyTo", bytes.as_ref(), &options.into())?).await?;
    let _ = call0(&frame, "close");
    let stride = width as usize * 4;
    RgbaFrame::new(width, height, stride, bytes.to_vec()).map_err(|error| Error::CodecBackend {
        codec: codec.clone(),
        operation: "copy WebCodecs VideoFrame",
        message: error.to_string(),
    })
}

fn rgba_frame_to_video_frame(
    codec: &CodecId,
    frame: &RgbaFrame,
    timestamp_us: i64,
    duration_us: i64,
) -> Result<JsValue> {
    let rgba = tight_rgba_bytes(frame);
    let data = Uint8Array::from(rgba.as_slice());
    let init = Object::new();
    set_str(&init, "format", "RGBA")?;
    set_u32(&init, "codedWidth", frame.width)?;
    set_u32(&init, "codedHeight", frame.height)?;
    set_f64(&init, "timestamp", timestamp_us as f64)?;
    set_f64(&init, "duration", duration_us as f64)?;
    construct("VideoFrame", &Array::of2(data.as_ref(), &init.into())).map_err(|error| {
        Error::CodecBackend {
            codec: codec.clone(),
            operation: "create WebCodecs VideoFrame",
            message: error.to_string(),
        }
    })
}

fn audio_data_to_frame(
    codec: &CodecId,
    audio: JsValue,
    fallback_sample_rate: u32,
    fallback_channels: u16,
) -> Result<AudioFrame> {
    let format = js_string_property(&audio, "format").unwrap_or_else(|| "f32".to_owned());
    if format != "f32" && format != "f32-planar" {
        let _ = call0(&audio, "close");
        return platform_codec_error(
            codec,
            "copy WebCodecs AudioData",
            format!("unsupported AudioData format {format}; expected f32 or f32-planar"),
        );
    }
    let sample_rate = js_u32_property(&audio, "sampleRate").unwrap_or(fallback_sample_rate);
    let channels =
        js_u32_property(&audio, "numberOfChannels").unwrap_or(u32::from(fallback_channels)) as u16;
    let frames = js_u32_property(&audio, "numberOfFrames").unwrap_or(0) as usize;
    let timestamp_us = js_i64_property(&audio, "timestamp").unwrap_or(0);
    let pts = TimeBase::microseconds().rescale(timestamp_us, TimeBase::new(1, sample_rate as i32)?);
    let samples = if format == "f32-planar" {
        let mut interleaved = vec![0.0; frames * usize::from(channels)];
        for channel in 0..channels {
            let plane = Float32Array::new_with_length(frames as u32);
            let options = Object::new();
            set_u32(&options, "planeIndex", u32::from(channel))?;
            call2(&audio, "copyTo", plane.as_ref(), &options.into())?;
            let plane = plane.to_vec();
            for frame_index in 0..frames {
                interleaved[frame_index * usize::from(channels) + usize::from(channel)] =
                    plane[frame_index];
            }
        }
        interleaved
    } else {
        let data = Float32Array::new_with_length((frames * usize::from(channels)) as u32);
        let options = Object::new();
        set_u32(&options, "planeIndex", 0)?;
        call2(&audio, "copyTo", data.as_ref(), &options.into())?;
        data.to_vec()
    };
    let _ = call0(&audio, "close");
    AudioFrame::new(sample_rate, channels, pts, samples)
}

fn audio_frame_to_audio_data(codec: &CodecId, frame: &AudioFrame) -> Result<JsValue> {
    let data = Float32Array::new_with_length(frame.samples_f32_interleaved.len() as u32);
    data.copy_from(&frame.samples_f32_interleaved);
    let init = Object::new();
    set_str(&init, "format", "f32")?;
    set_u32(&init, "sampleRate", frame.sample_rate)?;
    set_u32(&init, "numberOfFrames", frame.sample_frames() as u32)?;
    set_u32(&init, "numberOfChannels", u32::from(frame.channels))?;
    set_f64(
        &init,
        "timestamp",
        TimeBase::new(1, frame.sample_rate as i32)?.rescale(frame.pts, TimeBase::microseconds())
            as f64,
    )?;
    set(&init, "data", data.as_ref())?;
    construct("AudioData", &Array::of1(&init.into())).map_err(|error| Error::CodecBackend {
        codec: codec.clone(),
        operation: "create WebCodecs AudioData",
        message: error.to_string(),
    })
}

fn web_encoded_chunk_from_js(chunk: &JsValue) -> std::result::Result<WebEncodedChunk, String> {
    Ok(WebEncodedChunk {
        timestamp_us: js_i64_property(chunk, "timestamp").unwrap_or(0),
        duration_us: js_optional_i64_property(chunk, "duration"),
        keyframe: js_string_property(chunk, "type").as_deref() == Some("key"),
        data: encoded_chunk_bytes(chunk)?,
    })
}

fn encoded_chunk_bytes(chunk: &JsValue) -> std::result::Result<Vec<u8>, String> {
    let len = js_u32_property(chunk, "byteLength").map_err(|error| error.to_string())?;
    let bytes = Uint8Array::new_with_length(len);
    call1_string_error(chunk, "copyTo", bytes.as_ref())?;
    Ok(bytes.to_vec())
}

fn web_chunks_to_packets(
    codec: &CodecId,
    chunks: Vec<WebEncodedChunk>,
    time_base: TimeBase,
    fallback_duration: i64,
) -> Vec<EncodedPacket> {
    chunks
        .into_iter()
        .map(|chunk| {
            let pts = TimeBase::microseconds().rescale(chunk.timestamp_us, time_base);
            let duration = chunk
                .duration_us
                .map(|duration| TimeBase::microseconds().rescale(duration, time_base))
                .unwrap_or(fallback_duration);
            EncodedPacket::new(
                DEFAULT_TRACK_ID,
                codec.clone(),
                pts,
                duration,
                time_base,
                chunk.data,
            )
            .with_keyframe(chunk.keyframe)
        })
        .collect()
}

fn video_codec_string(codec: &CodecId, extra_data: &[u8]) -> Result<String> {
    match codec {
        CodecId::H264 => {
            if extra_data.len() >= 4 {
                Ok(format!(
                    "avc1.{:02X}{:02X}{:02X}",
                    extra_data[1], extra_data[2], extra_data[3]
                ))
            } else {
                Ok("avc1.42E01E".to_owned())
            }
        }
        CodecId::H265 => Ok("hev1".to_owned()),
        CodecId::ProRes => Ok("apcn".to_owned()),
        _ => platform_codec_error(
            codec,
            "configure WebCodecs video",
            "codec has no WebCodecs video codec string mapping",
        ),
    }
}

fn audio_codec_string(codec: &CodecId, extra_data: &[u8]) -> Result<String> {
    match codec {
        CodecId::Aac => Ok(format!(
            "mp4a.40.{}",
            aac_object_type(extra_data).unwrap_or(2)
        )),
        CodecId::Eac3 => Ok("ec-3".to_owned()),
        _ => platform_codec_error(
            codec,
            "configure WebCodecs audio",
            "codec has no WebCodecs audio codec string mapping",
        ),
    }
}

fn aac_object_type(extra_data: &[u8]) -> Option<u8> {
    if extra_data.is_empty() {
        return None;
    }
    let object_type = extra_data[0] >> 3;
    (object_type != 0).then_some(object_type)
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

async fn await_promise(value: JsValue) -> Result<JsValue> {
    let promise: Promise = value.dyn_into().map_err(|value| Error::CodecBackend {
        codec: CodecId::RawVideo,
        operation: "await WebCodecs promise",
        message: format!(
            "method did not return a Promise: {}",
            js_value_message(&value)
        ),
    })?;
    JsFuture::from(promise)
        .await
        .map_err(|value| Error::CodecBackend {
            codec: CodecId::RawVideo,
            operation: "await WebCodecs promise",
            message: js_value_message(&value),
        })
}

fn take_web_errors(
    codec: &CodecId,
    operation: &'static str,
    errors: &Rc<RefCell<Vec<String>>>,
) -> Result<()> {
    let mut errors = errors.borrow_mut();
    if errors.is_empty() {
        return Ok(());
    }
    let message = errors.join("; ");
    errors.clear();
    Err(Error::CodecBackend {
        codec: codec.clone(),
        operation,
        message,
    })
}

fn construct(class_name: &'static str, args: &Array) -> Result<JsValue> {
    let constructor = Reflect::get(&js_sys::global(), &JsValue::from_str(class_name))
        .map_err(|error| js_backend_error(CodecId::RawVideo, "load WebCodecs constructor", error))?
        .dyn_into::<Function>()
        .map_err(|value| Error::CodecBackend {
            codec: CodecId::RawVideo,
            operation: "load WebCodecs constructor",
            message: format!(
                "{class_name} is not a constructor: {}",
                js_value_message(&value)
            ),
        })?;
    Reflect::construct(&constructor, args)
        .map_err(|error| js_backend_error(CodecId::RawVideo, "construct WebCodecs object", error))
}

fn call0(target: &JsValue, method: &'static str) -> Result<JsValue> {
    method_function(target, method)?
        .call0(target)
        .map_err(|error| js_backend_error(CodecId::RawVideo, method, error))
}

fn call1(target: &JsValue, method: &'static str, arg: &JsValue) -> Result<JsValue> {
    method_function(target, method)?
        .call1(target, arg)
        .map_err(|error| js_backend_error(CodecId::RawVideo, method, error))
}

fn call2(
    target: &JsValue,
    method: &'static str,
    first: &JsValue,
    second: &JsValue,
) -> Result<JsValue> {
    method_function(target, method)?
        .call2(target, first, second)
        .map_err(|error| js_backend_error(CodecId::RawVideo, method, error))
}

fn call1_string_error(
    target: &JsValue,
    method: &'static str,
    arg: &JsValue,
) -> std::result::Result<JsValue, String> {
    method_function(target, method)
        .map_err(|error| error.to_string())?
        .call1(target, arg)
        .map_err(|error| js_value_message(&error))
}

fn method_function(target: &JsValue, method: &'static str) -> Result<Function> {
    Reflect::get(target, &JsValue::from_str(method))
        .map_err(|error| js_backend_error(CodecId::RawVideo, method, error))?
        .dyn_into::<Function>()
        .map_err(|value| Error::CodecBackend {
            codec: CodecId::RawVideo,
            operation: method,
            message: format!("{method} is not a function: {}", js_value_message(&value)),
        })
}

fn set(target: &Object, key: &'static str, value: &JsValue) -> Result<()> {
    Reflect::set(target, &JsValue::from_str(key), value)
        .map_err(|error| js_backend_error(CodecId::RawVideo, "set WebCodecs config", error))?;
    Ok(())
}

fn set_str(target: &Object, key: &'static str, value: &str) -> Result<()> {
    set(target, key, &JsValue::from_str(value))
}

fn set_u32(target: &Object, key: &'static str, value: u32) -> Result<()> {
    set(target, key, &JsValue::from_f64(f64::from(value)))
}

fn set_f64(target: &Object, key: &'static str, value: f64) -> Result<()> {
    set(target, key, &JsValue::from_f64(value))
}

fn js_string_property(target: &JsValue, key: &'static str) -> Option<String> {
    Reflect::get(target, &JsValue::from_str(key))
        .ok()
        .and_then(|value| value.as_string())
}

fn js_u32_property(target: &JsValue, key: &'static str) -> Result<u32> {
    Reflect::get(target, &JsValue::from_str(key))
        .map_err(|error| js_backend_error(CodecId::RawVideo, "read WebCodecs property", error))?
        .as_f64()
        .map(|value| value as u32)
        .ok_or_else(|| Error::CodecBackend {
            codec: CodecId::RawVideo,
            operation: "read WebCodecs property",
            message: format!("{key} is not a number"),
        })
}

fn js_i64_property(target: &JsValue, key: &'static str) -> Result<i64> {
    Reflect::get(target, &JsValue::from_str(key))
        .map_err(|error| js_backend_error(CodecId::RawVideo, "read WebCodecs property", error))?
        .as_f64()
        .map(|value| value as i64)
        .ok_or_else(|| Error::CodecBackend {
            codec: CodecId::RawVideo,
            operation: "read WebCodecs property",
            message: format!("{key} is not a number"),
        })
}

fn js_optional_i64_property(target: &JsValue, key: &'static str) -> Option<i64> {
    Reflect::get(target, &JsValue::from_str(key))
        .ok()
        .and_then(|value| value.as_f64())
        .map(|value| value as i64)
}

fn js_backend_error(codec: CodecId, operation: &'static str, value: JsValue) -> Error {
    Error::CodecBackend {
        codec,
        operation,
        message: js_value_message(&value),
    }
}

fn js_value_message(value: &JsValue) -> String {
    if let Some(message) = value.as_string() {
        return message;
    }
    if let Ok(message) = Reflect::get(value, &JsValue::from_str("message"))
        && let Some(message) = message.as_string()
    {
        return message;
    }
    "JavaScript exception".to_owned()
}
