//! Target-native codec adapters.
//!
//! This module is the concrete platform lane for patent-sensitive codecs. The
//! common wrappers expose the crate's synchronous codec traits while the
//! target-specific modules own unsafe FFI handles and runtime probing.

use crate::{
    audio::AudioFrame,
    backend::{BackendKind, BackendSource, recommended_backends_for_current_target},
    codec::{
        AudioDecoder, AudioEncoder, CodecDescriptor, CodecDirection, CodecId, VideoDecoder,
        VideoEncoder,
    },
    error::{Error, Result},
    frame::RgbaFrame,
    packet::EncodedPacket,
    time::TimeBase,
};

#[cfg(all(
    any(feature = "platform-codecs", feature = "codec-android"),
    target_os = "android"
))]
mod android;
#[cfg(all(
    any(feature = "platform-codecs", feature = "codec-apple"),
    any(target_os = "macos", target_os = "ios", target_os = "tvos")
))]
mod apple;
#[cfg(all(
    any(feature = "platform-codecs", feature = "codec-gstreamer"),
    target_os = "linux"
))]
mod gstreamer;
#[cfg(all(
    any(feature = "platform-codecs", feature = "codec-web"),
    target_family = "wasm"
))]
mod web;
#[cfg(all(
    any(feature = "platform-codecs", feature = "codec-web"),
    target_family = "wasm"
))]
pub use web::{
    AsyncWebCodecsAudioDecoder, AsyncWebCodecsAudioEncoder, AsyncWebCodecsVideoDecoder,
    AsyncWebCodecsVideoEncoder,
};
#[cfg(all(
    any(feature = "platform-codecs", feature = "codec-windows"),
    target_os = "windows"
))]
mod windows;

/// Runtime support probe for a platform codec operation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlatformCodecProbe {
    /// Backend selected for the current target.
    pub backend: Option<BackendKind>,
    /// Codec that was probed.
    pub codec: CodecId,
    /// Operation that was probed.
    pub direction: CodecDirection,
    /// True when the host API reports that a matching codec path exists.
    pub supported: bool,
    /// Human-readable host/API detail.
    pub detail: String,
}

impl PlatformCodecProbe {
    fn unsupported(codec: &CodecId, direction: CodecDirection, detail: impl Into<String>) -> Self {
        Self {
            backend: None,
            codec: codec.clone(),
            direction,
            supported: false,
            detail: detail.into(),
        }
    }
}

/// Video decoder configuration for platform adapters.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlatformVideoDecoderConfig {
    /// Encoded video codec.
    pub codec: CodecId,
    /// Coded width in pixels.
    pub width: u32,
    /// Coded height in pixels.
    pub height: u32,
    /// Optional codec-private bytes such as `avcC` or `hvcC`.
    pub extra_data: Vec<u8>,
}

impl PlatformVideoDecoderConfig {
    /// Create a video decoder config without codec-private bytes.
    #[must_use]
    pub fn new(codec: CodecId, width: u32, height: u32) -> Self {
        Self {
            codec,
            width,
            height,
            extra_data: Vec::new(),
        }
    }

    /// Attach codec-private bytes.
    #[must_use]
    pub fn with_extra_data(mut self, extra_data: impl Into<Vec<u8>>) -> Self {
        self.extra_data = extra_data.into();
        self
    }
}

/// Video encoder configuration for platform adapters.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlatformVideoEncoderConfig {
    /// Encoded output codec.
    pub codec: CodecId,
    /// Frame width in pixels.
    pub width: u32,
    /// Frame height in pixels.
    pub height: u32,
    /// Output packet time base.
    pub time_base: TimeBase,
    /// Target frame duration in `time_base` ticks.
    pub frame_duration: i64,
    /// Optional target bitrate in bits per second.
    pub bitrate: Option<u32>,
}

impl PlatformVideoEncoderConfig {
    /// Create a video encoder config.
    #[must_use]
    pub fn new(
        codec: CodecId,
        width: u32,
        height: u32,
        time_base: TimeBase,
        frame_duration: i64,
    ) -> Self {
        Self {
            codec,
            width,
            height,
            time_base,
            frame_duration,
            bitrate: None,
        }
    }

    /// Set a target bitrate in bits per second.
    #[must_use]
    pub const fn with_bitrate(mut self, bitrate: u32) -> Self {
        self.bitrate = Some(bitrate);
        self
    }
}

/// Audio decoder configuration for platform adapters.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlatformAudioDecoderConfig {
    /// Encoded audio codec.
    pub codec: CodecId,
    /// Input sample rate.
    pub sample_rate: u32,
    /// Input channel count.
    pub channels: u16,
    /// Optional codec-private bytes such as an AAC magic cookie.
    pub extra_data: Vec<u8>,
}

impl PlatformAudioDecoderConfig {
    /// Create an audio decoder config without codec-private bytes.
    #[must_use]
    pub fn new(codec: CodecId, sample_rate: u32, channels: u16) -> Self {
        Self {
            codec,
            sample_rate,
            channels,
            extra_data: Vec::new(),
        }
    }

    /// Attach codec-private bytes.
    #[must_use]
    pub fn with_extra_data(mut self, extra_data: impl Into<Vec<u8>>) -> Self {
        self.extra_data = extra_data.into();
        self
    }
}

/// Audio encoder configuration for platform adapters.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlatformAudioEncoderConfig {
    /// Encoded output codec.
    pub codec: CodecId,
    /// Output sample rate.
    pub sample_rate: u32,
    /// Output channel count.
    pub channels: u16,
    /// Optional target bitrate in bits per second.
    pub bitrate: Option<u32>,
}

impl PlatformAudioEncoderConfig {
    /// Create an audio encoder config.
    #[must_use]
    pub fn new(codec: CodecId, sample_rate: u32, channels: u16) -> Self {
        Self {
            codec,
            sample_rate,
            channels,
            bitrate: None,
        }
    }

    /// Set a target bitrate in bits per second.
    #[must_use]
    pub const fn with_bitrate(mut self, bitrate: u32) -> Self {
        self.bitrate = Some(bitrate);
        self
    }
}

/// Probe the selected platform backend for a codec operation on this target.
#[must_use]
pub fn probe_platform_codec(codec: &CodecId, direction: CodecDirection) -> PlatformCodecProbe {
    let Some(backend) = selected_platform_backend(codec, direction) else {
        return PlatformCodecProbe::unsupported(
            codec,
            direction,
            "no platform backend is compiled for this target/codec",
        );
    };

    probe_backend(backend, codec, direction)
}

fn selected_platform_backend(codec: &CodecId, direction: CodecDirection) -> Option<BackendKind> {
    recommended_backends_for_current_target()
        .into_iter()
        .find(|backend| {
            backend.source == BackendSource::Platform && backend.supports(codec, direction)
        })
        .map(|backend| backend.kind)
}

fn probe_backend(
    backend: BackendKind,
    codec: &CodecId,
    direction: CodecDirection,
) -> PlatformCodecProbe {
    #[cfg(all(
        any(feature = "platform-codecs", feature = "codec-apple"),
        any(target_os = "macos", target_os = "ios", target_os = "tvos")
    ))]
    if matches!(
        backend,
        BackendKind::AppleVideoToolbox | BackendKind::AppleAudioToolbox
    ) {
        return apple::probe(backend, codec, direction);
    }

    #[cfg(all(
        any(feature = "platform-codecs", feature = "codec-android"),
        target_os = "android"
    ))]
    if backend == BackendKind::AndroidMediaCodec {
        return android::probe(codec, direction);
    }

    #[cfg(all(
        any(feature = "platform-codecs", feature = "codec-gstreamer"),
        target_os = "linux"
    ))]
    if backend == BackendKind::GStreamer {
        return gstreamer::probe(codec, direction);
    }

    #[cfg(all(
        any(feature = "platform-codecs", feature = "codec-web"),
        target_family = "wasm"
    ))]
    if backend == BackendKind::WebCodecs {
        return web::probe(codec, direction);
    }

    #[cfg(all(
        any(feature = "platform-codecs", feature = "codec-windows"),
        target_os = "windows"
    ))]
    if backend == BackendKind::WindowsMediaFoundation {
        return windows::probe(codec, direction);
    }

    PlatformCodecProbe {
        backend: Some(backend),
        codec: codec.clone(),
        direction,
        supported: false,
        detail: "selected backend is not available in this build".to_owned(),
    }
}

enum PlatformVideoDecoderInner {
    #[cfg(all(
        any(feature = "platform-codecs", feature = "codec-apple"),
        any(target_os = "macos", target_os = "ios", target_os = "tvos")
    ))]
    Apple(apple::VideoDecoderHandle),
    #[cfg(all(
        any(feature = "platform-codecs", feature = "codec-android"),
        target_os = "android"
    ))]
    Android(android::CodecHandle),
    #[cfg(all(
        any(feature = "platform-codecs", feature = "codec-gstreamer"),
        target_os = "linux"
    ))]
    GStreamer(gstreamer::ElementHandle),
    #[cfg(all(
        any(feature = "platform-codecs", feature = "codec-web"),
        target_family = "wasm"
    ))]
    Web(web::DecoderHandle),
    #[cfg(all(
        any(feature = "platform-codecs", feature = "codec-windows"),
        target_os = "windows"
    ))]
    Windows(windows::TransformHandle),
}

/// Target-native video decoder.
pub struct PlatformVideoDecoder {
    codec: CodecId,
    backend: BackendKind,
    inner: PlatformVideoDecoderInner,
}

impl PlatformVideoDecoder {
    /// Construct a decoder through the selected platform backend.
    pub fn new(config: PlatformVideoDecoderConfig) -> Result<Self> {
        if !config.codec.is_video() {
            return Err(Error::CodecBackend {
                codec: config.codec,
                operation: "open platform video decoder",
                message: "codec is not a video codec".to_owned(),
            });
        }
        let backend = selected_platform_backend(&config.codec, CodecDirection::Decode).ok_or(
            Error::Unsupported {
                operation: "open platform video decoder",
                reason: "no platform video decoder backend is compiled for this target/codec",
            },
        )?;
        let inner = open_video_decoder(backend, &config)?;
        Ok(Self {
            codec: config.codec,
            backend,
            inner,
        })
    }

    /// Backend selected for this decoder.
    #[must_use]
    pub const fn backend(&self) -> BackendKind {
        self.backend
    }
}

impl CodecDescriptor for PlatformVideoDecoder {
    fn name(&self) -> &'static str {
        "platform-video-decoder"
    }

    fn codec_id(&self) -> CodecId {
        self.codec.clone()
    }
}

impl VideoDecoder for PlatformVideoDecoder {
    fn decode_packet(&mut self, packet: &EncodedPacket) -> Result<Vec<RgbaFrame>> {
        if packet.codec != self.codec {
            return Err(Error::CodecMismatch {
                expected: self.codec.clone(),
                actual: packet.codec.clone(),
            });
        }
        decode_video_packet(&mut self.inner, packet)
    }

    fn flush(&mut self) -> Result<Vec<RgbaFrame>> {
        flush_video_decoder(&mut self.inner)
    }
}

enum PlatformVideoEncoderInner {
    #[cfg(all(
        any(feature = "platform-codecs", feature = "codec-apple"),
        any(target_os = "macos", target_os = "ios", target_os = "tvos")
    ))]
    Apple(apple::VideoEncoderHandle),
    #[cfg(all(
        any(feature = "platform-codecs", feature = "codec-android"),
        target_os = "android"
    ))]
    Android(android::CodecHandle),
    #[cfg(all(
        any(feature = "platform-codecs", feature = "codec-gstreamer"),
        target_os = "linux"
    ))]
    GStreamer(gstreamer::ElementHandle),
    #[cfg(all(
        any(feature = "platform-codecs", feature = "codec-web"),
        target_family = "wasm"
    ))]
    Web(web::EncoderHandle),
    #[cfg(all(
        any(feature = "platform-codecs", feature = "codec-windows"),
        target_os = "windows"
    ))]
    Windows(windows::TransformHandle),
}

/// Target-native video encoder.
pub struct PlatformVideoEncoder {
    codec: CodecId,
    backend: BackendKind,
    inner: PlatformVideoEncoderInner,
}

impl PlatformVideoEncoder {
    /// Construct an encoder through the selected platform backend.
    pub fn new(config: PlatformVideoEncoderConfig) -> Result<Self> {
        if !config.codec.is_video() {
            return Err(Error::CodecBackend {
                codec: config.codec,
                operation: "open platform video encoder",
                message: "codec is not a video codec".to_owned(),
            });
        }
        let backend = selected_platform_backend(&config.codec, CodecDirection::Encode).ok_or(
            Error::Unsupported {
                operation: "open platform video encoder",
                reason: "no platform video encoder backend is compiled for this target/codec",
            },
        )?;
        let inner = open_video_encoder(backend, &config)?;
        Ok(Self {
            codec: config.codec,
            backend,
            inner,
        })
    }

    /// Backend selected for this encoder.
    #[must_use]
    pub const fn backend(&self) -> BackendKind {
        self.backend
    }

    /// Codec-private bytes emitted or discovered by the platform encoder.
    ///
    /// For H.264/H.265 this is typically `avcC`/`hvcC` data needed by MP4/fMP4
    /// muxers or by platform decoders that expect configured length-prefixed
    /// samples. Some backends do not expose this data and return `None`.
    #[must_use]
    pub fn codec_config(&self) -> Option<Vec<u8>> {
        video_encoder_codec_config(&self.inner)
    }
}

impl CodecDescriptor for PlatformVideoEncoder {
    fn name(&self) -> &'static str {
        "platform-video-encoder"
    }

    fn codec_id(&self) -> CodecId {
        self.codec.clone()
    }
}

impl VideoEncoder for PlatformVideoEncoder {
    fn encode_frame(&mut self, frame: &RgbaFrame, pts: i64) -> Result<Vec<EncodedPacket>> {
        encode_video_frame(&mut self.inner, &self.codec, frame, pts)
    }

    fn finish(&mut self) -> Result<Vec<EncodedPacket>> {
        finish_video_encoder(&mut self.inner, &self.codec)
    }
}

enum PlatformAudioDecoderInner {
    #[cfg(all(
        any(feature = "platform-codecs", feature = "codec-apple"),
        any(target_os = "macos", target_os = "ios", target_os = "tvos")
    ))]
    Apple(apple::AudioConverterHandle),
    #[cfg(all(
        any(feature = "platform-codecs", feature = "codec-android"),
        target_os = "android"
    ))]
    Android(android::CodecHandle),
    #[cfg(all(
        any(feature = "platform-codecs", feature = "codec-gstreamer"),
        target_os = "linux"
    ))]
    GStreamer(gstreamer::ElementHandle),
    #[cfg(all(
        any(feature = "platform-codecs", feature = "codec-web"),
        target_family = "wasm"
    ))]
    Web(web::DecoderHandle),
    #[cfg(all(
        any(feature = "platform-codecs", feature = "codec-windows"),
        target_os = "windows"
    ))]
    Windows(windows::TransformHandle),
}

/// Target-native audio decoder.
pub struct PlatformAudioDecoder {
    codec: CodecId,
    backend: BackendKind,
    inner: PlatformAudioDecoderInner,
}

impl PlatformAudioDecoder {
    /// Construct a decoder through the selected platform backend.
    pub fn new(config: PlatformAudioDecoderConfig) -> Result<Self> {
        if !config.codec.is_audio() {
            return Err(Error::CodecBackend {
                codec: config.codec,
                operation: "open platform audio decoder",
                message: "codec is not an audio codec".to_owned(),
            });
        }
        let backend = selected_platform_backend(&config.codec, CodecDirection::Decode).ok_or(
            Error::Unsupported {
                operation: "open platform audio decoder",
                reason: "no platform audio decoder backend is compiled for this target/codec",
            },
        )?;
        let inner = open_audio_decoder(backend, &config)?;
        Ok(Self {
            codec: config.codec,
            backend,
            inner,
        })
    }

    /// Backend selected for this decoder.
    #[must_use]
    pub const fn backend(&self) -> BackendKind {
        self.backend
    }
}

impl CodecDescriptor for PlatformAudioDecoder {
    fn name(&self) -> &'static str {
        "platform-audio-decoder"
    }

    fn codec_id(&self) -> CodecId {
        self.codec.clone()
    }
}

impl AudioDecoder for PlatformAudioDecoder {
    fn decode_packet(&mut self, packet: &EncodedPacket) -> Result<Vec<AudioFrame>> {
        if packet.codec != self.codec {
            return Err(Error::CodecMismatch {
                expected: self.codec.clone(),
                actual: packet.codec.clone(),
            });
        }
        decode_audio_packet(&mut self.inner, packet)
    }

    fn flush(&mut self) -> Result<Vec<AudioFrame>> {
        flush_audio_decoder(&mut self.inner)
    }
}

enum PlatformAudioEncoderInner {
    #[cfg(all(
        any(feature = "platform-codecs", feature = "codec-apple"),
        any(target_os = "macos", target_os = "ios", target_os = "tvos")
    ))]
    Apple(apple::AudioConverterHandle),
    #[cfg(all(
        any(feature = "platform-codecs", feature = "codec-android"),
        target_os = "android"
    ))]
    Android(android::CodecHandle),
    #[cfg(all(
        any(feature = "platform-codecs", feature = "codec-gstreamer"),
        target_os = "linux"
    ))]
    GStreamer(gstreamer::ElementHandle),
    #[cfg(all(
        any(feature = "platform-codecs", feature = "codec-web"),
        target_family = "wasm"
    ))]
    Web(web::EncoderHandle),
    #[cfg(all(
        any(feature = "platform-codecs", feature = "codec-windows"),
        target_os = "windows"
    ))]
    Windows(windows::TransformHandle),
}

/// Target-native audio encoder.
pub struct PlatformAudioEncoder {
    codec: CodecId,
    backend: BackendKind,
    inner: PlatformAudioEncoderInner,
}

impl PlatformAudioEncoder {
    /// Construct an encoder through the selected platform backend.
    pub fn new(config: PlatformAudioEncoderConfig) -> Result<Self> {
        if !config.codec.is_audio() {
            return Err(Error::CodecBackend {
                codec: config.codec,
                operation: "open platform audio encoder",
                message: "codec is not an audio codec".to_owned(),
            });
        }
        let backend = selected_platform_backend(&config.codec, CodecDirection::Encode).ok_or(
            Error::Unsupported {
                operation: "open platform audio encoder",
                reason: "no platform audio encoder backend is compiled for this target/codec",
            },
        )?;
        let inner = open_audio_encoder(backend, &config)?;
        Ok(Self {
            codec: config.codec,
            backend,
            inner,
        })
    }

    /// Backend selected for this encoder.
    #[must_use]
    pub const fn backend(&self) -> BackendKind {
        self.backend
    }
}

impl CodecDescriptor for PlatformAudioEncoder {
    fn name(&self) -> &'static str {
        "platform-audio-encoder"
    }

    fn codec_id(&self) -> CodecId {
        self.codec.clone()
    }
}

impl AudioEncoder for PlatformAudioEncoder {
    fn encode_frame(&mut self, frame: &AudioFrame) -> Result<Vec<EncodedPacket>> {
        encode_audio_frame(&mut self.inner, &self.codec, frame)
    }

    fn finish(&mut self) -> Result<Vec<EncodedPacket>> {
        finish_audio_encoder(&mut self.inner, &self.codec)
    }
}

fn open_video_decoder(
    backend: BackendKind,
    config: &PlatformVideoDecoderConfig,
) -> Result<PlatformVideoDecoderInner> {
    #[cfg(all(
        any(feature = "platform-codecs", feature = "codec-apple"),
        any(target_os = "macos", target_os = "ios", target_os = "tvos")
    ))]
    if backend == BackendKind::AppleVideoToolbox {
        return apple::open_video_decoder(config).map(PlatformVideoDecoderInner::Apple);
    }
    #[cfg(all(
        any(feature = "platform-codecs", feature = "codec-android"),
        target_os = "android"
    ))]
    if backend == BackendKind::AndroidMediaCodec {
        return android::open_video_decoder(config).map(PlatformVideoDecoderInner::Android);
    }
    #[cfg(all(
        any(feature = "platform-codecs", feature = "codec-gstreamer"),
        target_os = "linux"
    ))]
    if backend == BackendKind::GStreamer {
        return gstreamer::open_video_decoder(config).map(PlatformVideoDecoderInner::GStreamer);
    }
    #[cfg(all(
        any(feature = "platform-codecs", feature = "codec-web"),
        target_family = "wasm"
    ))]
    if backend == BackendKind::WebCodecs {
        return web::open_decoder(&config.codec).map(PlatformVideoDecoderInner::Web);
    }
    #[cfg(all(
        any(feature = "platform-codecs", feature = "codec-windows"),
        target_os = "windows"
    ))]
    if backend == BackendKind::WindowsMediaFoundation {
        return windows::open_video_decoder(config).map(PlatformVideoDecoderInner::Windows);
    }
    Err(Error::Unsupported {
        operation: "open platform video decoder",
        reason: "selected platform backend is not available in this build",
    })
}

fn open_video_encoder(
    backend: BackendKind,
    config: &PlatformVideoEncoderConfig,
) -> Result<PlatformVideoEncoderInner> {
    #[cfg(all(
        any(feature = "platform-codecs", feature = "codec-apple"),
        any(target_os = "macos", target_os = "ios", target_os = "tvos")
    ))]
    if backend == BackendKind::AppleVideoToolbox {
        return apple::open_video_encoder(config).map(PlatformVideoEncoderInner::Apple);
    }
    #[cfg(all(
        any(feature = "platform-codecs", feature = "codec-android"),
        target_os = "android"
    ))]
    if backend == BackendKind::AndroidMediaCodec {
        return android::open_video_encoder(config).map(PlatformVideoEncoderInner::Android);
    }
    #[cfg(all(
        any(feature = "platform-codecs", feature = "codec-gstreamer"),
        target_os = "linux"
    ))]
    if backend == BackendKind::GStreamer {
        return gstreamer::open_video_encoder(config).map(PlatformVideoEncoderInner::GStreamer);
    }
    #[cfg(all(
        any(feature = "platform-codecs", feature = "codec-web"),
        target_family = "wasm"
    ))]
    if backend == BackendKind::WebCodecs {
        return web::open_encoder(&config.codec).map(PlatformVideoEncoderInner::Web);
    }
    #[cfg(all(
        any(feature = "platform-codecs", feature = "codec-windows"),
        target_os = "windows"
    ))]
    if backend == BackendKind::WindowsMediaFoundation {
        return windows::open_video_encoder(config).map(PlatformVideoEncoderInner::Windows);
    }
    Err(Error::Unsupported {
        operation: "open platform video encoder",
        reason: "selected platform backend is not available in this build",
    })
}

fn open_audio_decoder(
    backend: BackendKind,
    config: &PlatformAudioDecoderConfig,
) -> Result<PlatformAudioDecoderInner> {
    #[cfg(all(
        any(feature = "platform-codecs", feature = "codec-apple"),
        any(target_os = "macos", target_os = "ios", target_os = "tvos")
    ))]
    if backend == BackendKind::AppleAudioToolbox {
        return apple::open_audio_decoder(config).map(PlatformAudioDecoderInner::Apple);
    }
    #[cfg(all(
        any(feature = "platform-codecs", feature = "codec-android"),
        target_os = "android"
    ))]
    if backend == BackendKind::AndroidMediaCodec {
        return android::open_audio_decoder(config).map(PlatformAudioDecoderInner::Android);
    }
    #[cfg(all(
        any(feature = "platform-codecs", feature = "codec-gstreamer"),
        target_os = "linux"
    ))]
    if backend == BackendKind::GStreamer {
        return gstreamer::open_audio_decoder(config).map(PlatformAudioDecoderInner::GStreamer);
    }
    #[cfg(all(
        any(feature = "platform-codecs", feature = "codec-web"),
        target_family = "wasm"
    ))]
    if backend == BackendKind::WebCodecs {
        return web::open_decoder(&config.codec).map(PlatformAudioDecoderInner::Web);
    }
    #[cfg(all(
        any(feature = "platform-codecs", feature = "codec-windows"),
        target_os = "windows"
    ))]
    if backend == BackendKind::WindowsMediaFoundation {
        return windows::open_audio_decoder(config).map(PlatformAudioDecoderInner::Windows);
    }
    Err(Error::Unsupported {
        operation: "open platform audio decoder",
        reason: "selected platform backend is not available in this build",
    })
}

fn open_audio_encoder(
    backend: BackendKind,
    config: &PlatformAudioEncoderConfig,
) -> Result<PlatformAudioEncoderInner> {
    #[cfg(all(
        any(feature = "platform-codecs", feature = "codec-apple"),
        any(target_os = "macos", target_os = "ios", target_os = "tvos")
    ))]
    if backend == BackendKind::AppleAudioToolbox {
        return apple::open_audio_encoder(config).map(PlatformAudioEncoderInner::Apple);
    }
    #[cfg(all(
        any(feature = "platform-codecs", feature = "codec-android"),
        target_os = "android"
    ))]
    if backend == BackendKind::AndroidMediaCodec {
        return android::open_audio_encoder(config).map(PlatformAudioEncoderInner::Android);
    }
    #[cfg(all(
        any(feature = "platform-codecs", feature = "codec-gstreamer"),
        target_os = "linux"
    ))]
    if backend == BackendKind::GStreamer {
        return gstreamer::open_audio_encoder(config).map(PlatformAudioEncoderInner::GStreamer);
    }
    #[cfg(all(
        any(feature = "platform-codecs", feature = "codec-web"),
        target_family = "wasm"
    ))]
    if backend == BackendKind::WebCodecs {
        return web::open_encoder(&config.codec).map(PlatformAudioEncoderInner::Web);
    }
    #[cfg(all(
        any(feature = "platform-codecs", feature = "codec-windows"),
        target_os = "windows"
    ))]
    if backend == BackendKind::WindowsMediaFoundation {
        return windows::open_audio_encoder(config).map(PlatformAudioEncoderInner::Windows);
    }
    Err(Error::Unsupported {
        operation: "open platform audio encoder",
        reason: "selected platform backend is not available in this build",
    })
}

fn decode_video_packet(
    inner: &mut PlatformVideoDecoderInner,
    packet: &EncodedPacket,
) -> Result<Vec<RgbaFrame>> {
    match inner {
        #[cfg(all(
            any(feature = "platform-codecs", feature = "codec-apple"),
            any(target_os = "macos", target_os = "ios", target_os = "tvos")
        ))]
        PlatformVideoDecoderInner::Apple(handle) => handle.decode_packet(packet),
        #[cfg(all(
            any(feature = "platform-codecs", feature = "codec-android"),
            target_os = "android"
        ))]
        PlatformVideoDecoderInner::Android(handle) => handle.decode_video_packet(packet),
        #[cfg(all(
            any(feature = "platform-codecs", feature = "codec-gstreamer"),
            target_os = "linux"
        ))]
        PlatformVideoDecoderInner::GStreamer(handle) => handle.decode_video_packet(packet),
        #[cfg(all(
            any(feature = "platform-codecs", feature = "codec-web"),
            target_family = "wasm"
        ))]
        PlatformVideoDecoderInner::Web(handle) => handle.decode_video_packet(packet),
        #[cfg(all(
            any(feature = "platform-codecs", feature = "codec-windows"),
            target_os = "windows"
        ))]
        PlatformVideoDecoderInner::Windows(handle) => handle.decode_video_packet(packet),
    }
}

fn encode_video_frame(
    inner: &mut PlatformVideoEncoderInner,
    codec: &CodecId,
    frame: &RgbaFrame,
    pts: i64,
) -> Result<Vec<EncodedPacket>> {
    match inner {
        #[cfg(all(
            any(feature = "platform-codecs", feature = "codec-apple"),
            any(target_os = "macos", target_os = "ios", target_os = "tvos")
        ))]
        PlatformVideoEncoderInner::Apple(handle) => handle.encode_frame(codec, frame, pts),
        #[cfg(all(
            any(feature = "platform-codecs", feature = "codec-android"),
            target_os = "android"
        ))]
        PlatformVideoEncoderInner::Android(handle) => handle.encode_video_frame(codec, frame, pts),
        #[cfg(all(
            any(feature = "platform-codecs", feature = "codec-gstreamer"),
            target_os = "linux"
        ))]
        PlatformVideoEncoderInner::GStreamer(handle) => {
            handle.encode_video_frame(codec, frame, pts)
        }
        #[cfg(all(
            any(feature = "platform-codecs", feature = "codec-web"),
            target_family = "wasm"
        ))]
        PlatformVideoEncoderInner::Web(handle) => handle.encode_video_frame(codec, frame, pts),
        #[cfg(all(
            any(feature = "platform-codecs", feature = "codec-windows"),
            target_os = "windows"
        ))]
        PlatformVideoEncoderInner::Windows(handle) => handle.encode_video_frame(codec, frame, pts),
    }
}

fn decode_audio_packet(
    inner: &mut PlatformAudioDecoderInner,
    packet: &EncodedPacket,
) -> Result<Vec<AudioFrame>> {
    match inner {
        #[cfg(all(
            any(feature = "platform-codecs", feature = "codec-apple"),
            any(target_os = "macos", target_os = "ios", target_os = "tvos")
        ))]
        PlatformAudioDecoderInner::Apple(handle) => handle.decode_packet(packet),
        #[cfg(all(
            any(feature = "platform-codecs", feature = "codec-android"),
            target_os = "android"
        ))]
        PlatformAudioDecoderInner::Android(handle) => handle.decode_audio_packet(packet),
        #[cfg(all(
            any(feature = "platform-codecs", feature = "codec-gstreamer"),
            target_os = "linux"
        ))]
        PlatformAudioDecoderInner::GStreamer(handle) => handle.decode_audio_packet(packet),
        #[cfg(all(
            any(feature = "platform-codecs", feature = "codec-web"),
            target_family = "wasm"
        ))]
        PlatformAudioDecoderInner::Web(handle) => handle.decode_audio_packet(packet),
        #[cfg(all(
            any(feature = "platform-codecs", feature = "codec-windows"),
            target_os = "windows"
        ))]
        PlatformAudioDecoderInner::Windows(handle) => handle.decode_audio_packet(packet),
    }
}

fn encode_audio_frame(
    inner: &mut PlatformAudioEncoderInner,
    codec: &CodecId,
    frame: &AudioFrame,
) -> Result<Vec<EncodedPacket>> {
    match inner {
        #[cfg(all(
            any(feature = "platform-codecs", feature = "codec-apple"),
            any(target_os = "macos", target_os = "ios", target_os = "tvos")
        ))]
        PlatformAudioEncoderInner::Apple(handle) => handle.encode_frame(codec, frame),
        #[cfg(all(
            any(feature = "platform-codecs", feature = "codec-android"),
            target_os = "android"
        ))]
        PlatformAudioEncoderInner::Android(handle) => handle.encode_audio_frame(codec, frame),
        #[cfg(all(
            any(feature = "platform-codecs", feature = "codec-gstreamer"),
            target_os = "linux"
        ))]
        PlatformAudioEncoderInner::GStreamer(handle) => handle.encode_audio_frame(codec, frame),
        #[cfg(all(
            any(feature = "platform-codecs", feature = "codec-web"),
            target_family = "wasm"
        ))]
        PlatformAudioEncoderInner::Web(handle) => handle.encode_audio_frame(codec, frame),
        #[cfg(all(
            any(feature = "platform-codecs", feature = "codec-windows"),
            target_os = "windows"
        ))]
        PlatformAudioEncoderInner::Windows(handle) => handle.encode_audio_frame(codec, frame),
    }
}

fn flush_video_decoder(inner: &mut PlatformVideoDecoderInner) -> Result<Vec<RgbaFrame>> {
    match inner {
        #[cfg(all(
            any(feature = "platform-codecs", feature = "codec-apple"),
            any(target_os = "macos", target_os = "ios", target_os = "tvos")
        ))]
        PlatformVideoDecoderInner::Apple(handle) => handle.flush_video_decoder(),
        #[cfg(all(
            any(feature = "platform-codecs", feature = "codec-android"),
            target_os = "android"
        ))]
        PlatformVideoDecoderInner::Android(handle) => handle.flush_video_decoder(),
        #[cfg(all(
            any(feature = "platform-codecs", feature = "codec-gstreamer"),
            target_os = "linux"
        ))]
        PlatformVideoDecoderInner::GStreamer(handle) => handle.flush_video_decoder(),
        #[cfg(all(
            any(feature = "platform-codecs", feature = "codec-web"),
            target_family = "wasm"
        ))]
        PlatformVideoDecoderInner::Web(handle) => handle.flush_video_decoder(),
        #[cfg(all(
            any(feature = "platform-codecs", feature = "codec-windows"),
            target_os = "windows"
        ))]
        PlatformVideoDecoderInner::Windows(handle) => handle.flush_video_decoder(),
    }
}

fn finish_video_encoder(
    inner: &mut PlatformVideoEncoderInner,
    codec: &CodecId,
) -> Result<Vec<EncodedPacket>> {
    #[cfg(not(any(
        all(
            any(feature = "platform-codecs", feature = "codec-apple"),
            any(target_os = "macos", target_os = "ios", target_os = "tvos")
        ),
        all(
            any(feature = "platform-codecs", feature = "codec-android"),
            target_os = "android"
        ),
        all(
            any(feature = "platform-codecs", feature = "codec-gstreamer"),
            target_os = "linux"
        ),
        all(
            any(feature = "platform-codecs", feature = "codec-web"),
            target_family = "wasm"
        ),
        all(
            any(feature = "platform-codecs", feature = "codec-windows"),
            target_os = "windows"
        )
    )))]
    let _ = codec;

    match inner {
        #[cfg(all(
            any(feature = "platform-codecs", feature = "codec-apple"),
            any(target_os = "macos", target_os = "ios", target_os = "tvos")
        ))]
        PlatformVideoEncoderInner::Apple(handle) => handle.finish_video_encoder(codec),
        #[cfg(all(
            any(feature = "platform-codecs", feature = "codec-android"),
            target_os = "android"
        ))]
        PlatformVideoEncoderInner::Android(handle) => handle.finish_video_encoder(codec),
        #[cfg(all(
            any(feature = "platform-codecs", feature = "codec-gstreamer"),
            target_os = "linux"
        ))]
        PlatformVideoEncoderInner::GStreamer(handle) => handle.finish_video_encoder(codec),
        #[cfg(all(
            any(feature = "platform-codecs", feature = "codec-web"),
            target_family = "wasm"
        ))]
        PlatformVideoEncoderInner::Web(handle) => handle.finish_video_encoder(codec),
        #[cfg(all(
            any(feature = "platform-codecs", feature = "codec-windows"),
            target_os = "windows"
        ))]
        PlatformVideoEncoderInner::Windows(handle) => handle.finish_video_encoder(codec),
    }
}

fn video_encoder_codec_config(inner: &PlatformVideoEncoderInner) -> Option<Vec<u8>> {
    match inner {
        #[cfg(all(
            any(feature = "platform-codecs", feature = "codec-apple"),
            any(target_os = "macos", target_os = "ios", target_os = "tvos")
        ))]
        PlatformVideoEncoderInner::Apple(handle) => handle.codec_config(),
        #[cfg(all(
            any(feature = "platform-codecs", feature = "codec-android"),
            target_os = "android"
        ))]
        PlatformVideoEncoderInner::Android(_) => None,
        #[cfg(all(
            any(feature = "platform-codecs", feature = "codec-gstreamer"),
            target_os = "linux"
        ))]
        PlatformVideoEncoderInner::GStreamer(_) => None,
        #[cfg(all(
            any(feature = "platform-codecs", feature = "codec-web"),
            target_family = "wasm"
        ))]
        PlatformVideoEncoderInner::Web(_) => None,
        #[cfg(all(
            any(feature = "platform-codecs", feature = "codec-windows"),
            target_os = "windows"
        ))]
        PlatformVideoEncoderInner::Windows(_) => None,
    }
}

fn flush_audio_decoder(inner: &mut PlatformAudioDecoderInner) -> Result<Vec<AudioFrame>> {
    match inner {
        #[cfg(all(
            any(feature = "platform-codecs", feature = "codec-apple"),
            any(target_os = "macos", target_os = "ios", target_os = "tvos")
        ))]
        PlatformAudioDecoderInner::Apple(handle) => {
            handle.reset()?;
            Ok(Vec::new())
        }
        #[cfg(all(
            any(feature = "platform-codecs", feature = "codec-android"),
            target_os = "android"
        ))]
        PlatformAudioDecoderInner::Android(handle) => handle.flush_audio_decoder(),
        #[cfg(all(
            any(feature = "platform-codecs", feature = "codec-gstreamer"),
            target_os = "linux"
        ))]
        PlatformAudioDecoderInner::GStreamer(handle) => handle.flush_audio_decoder(),
        #[cfg(all(
            any(feature = "platform-codecs", feature = "codec-web"),
            target_family = "wasm"
        ))]
        PlatformAudioDecoderInner::Web(handle) => handle.flush_audio_decoder(),
        #[cfg(all(
            any(feature = "platform-codecs", feature = "codec-windows"),
            target_os = "windows"
        ))]
        PlatformAudioDecoderInner::Windows(handle) => handle.flush_audio_decoder(),
    }
}

fn finish_audio_encoder(
    inner: &mut PlatformAudioEncoderInner,
    codec: &CodecId,
) -> Result<Vec<EncodedPacket>> {
    #[cfg(not(any(
        all(
            any(feature = "platform-codecs", feature = "codec-android"),
            target_os = "android"
        ),
        all(
            any(feature = "platform-codecs", feature = "codec-gstreamer"),
            target_os = "linux"
        ),
        all(
            any(feature = "platform-codecs", feature = "codec-web"),
            target_family = "wasm"
        ),
        all(
            any(feature = "platform-codecs", feature = "codec-windows"),
            target_os = "windows"
        )
    )))]
    let _ = codec;

    match inner {
        #[cfg(all(
            any(feature = "platform-codecs", feature = "codec-apple"),
            any(target_os = "macos", target_os = "ios", target_os = "tvos")
        ))]
        PlatformAudioEncoderInner::Apple(handle) => handle.finish_audio_encoder(codec),
        #[cfg(all(
            any(feature = "platform-codecs", feature = "codec-android"),
            target_os = "android"
        ))]
        PlatformAudioEncoderInner::Android(handle) => handle.finish_audio_encoder(codec),
        #[cfg(all(
            any(feature = "platform-codecs", feature = "codec-gstreamer"),
            target_os = "linux"
        ))]
        PlatformAudioEncoderInner::GStreamer(handle) => handle.finish_audio_encoder(codec),
        #[cfg(all(
            any(feature = "platform-codecs", feature = "codec-web"),
            target_family = "wasm"
        ))]
        PlatformAudioEncoderInner::Web(handle) => handle.finish_audio_encoder(codec),
        #[cfg(all(
            any(feature = "platform-codecs", feature = "codec-windows"),
            target_os = "windows"
        ))]
        PlatformAudioEncoderInner::Windows(handle) => handle.finish_audio_encoder(codec),
    }
}

#[allow(dead_code)]
pub(crate) fn platform_codec_error<T>(
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
