use crate::codec::{CodecDirection, CodecId};

/// Broad runtime target family for codec backends.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TargetFamily {
    /// Apple platforms where VideoToolbox is the preferred video backend.
    Apple,
    /// Android where MediaCodec/AMediaCodec is the preferred video backend.
    Android,
    /// Browser/WASM targets where WebCodecs is the preferred video backend.
    Web,
    /// Windows targets where Media Foundation is the preferred system backend.
    Windows,
    /// Linux targets where GStreamer is the preferred system backend.
    Linux,
    /// Server/desktop Rust software path.
    Native,
}

/// Codec backend identifier.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BackendKind {
    /// Apple VideoToolbox.
    AppleVideoToolbox,
    /// Apple AudioToolbox/CoreAudio codec conversion.
    AppleAudioToolbox,
    /// Android MediaCodec / AMediaCodec.
    AndroidMediaCodec,
    /// Browser WebCodecs.
    WebCodecs,
    /// Windows Media Foundation.
    WindowsMediaFoundation,
    /// GStreamer system plugin backend.
    GStreamer,
    /// Rust-native software codec.
    RustSoftware,
    /// OpenH264 FFI backend.
    OpenH264Ffi,
}

/// Where a codec implementation comes from.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BackendSource {
    /// Platform or system codec APIs selected at runtime.
    Platform,
    /// Codec implementation bundled directly through Rust dependencies.
    BundledNative,
    /// External native library reached through FFI.
    ExternalFfi,
}

/// Whether support is known statically or must be probed on the running host.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BackendProbe {
    /// The crate compiled a concrete implementation for this codec operation.
    Static,
    /// The backend is available as an API lane, but codec support must be probed.
    Runtime,
}

/// Backend capability descriptor.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BackendCapability {
    /// Backend kind.
    pub kind: BackendKind,
    /// Runtime target family.
    pub target: TargetFamily,
    /// Where this backend comes from.
    pub source: BackendSource,
    /// Whether codec support is static or runtime-probed.
    pub probe: BackendProbe,
    /// Whether the backend usually uses hardware acceleration.
    pub hardware_accelerated: bool,
    /// Codecs this backend can decode.
    pub decodes: Vec<CodecId>,
    /// Codecs this backend can encode.
    pub encodes: Vec<CodecId>,
    /// Human-readable implementation note.
    pub note: &'static str,
}

impl BackendCapability {
    /// True when this backend advertises the requested codec operation.
    #[must_use]
    pub fn supports(&self, codec: &CodecId, direction: CodecDirection) -> bool {
        match direction {
            CodecDirection::Decode => self.decodes.iter().any(|known| known == codec),
            CodecDirection::Encode => self.encodes.iter().any(|known| known == codec),
        }
    }
}

/// Common descriptor trait for platform, bundled, and FFI codec backends.
pub trait CodecBackendDescriptor {
    /// Backend kind.
    fn kind(&self) -> BackendKind;

    /// Runtime target family.
    fn target(&self) -> TargetFamily;

    /// Where the backend implementation comes from.
    fn source(&self) -> BackendSource;

    /// Whether codec support is static or runtime-probed.
    fn probe(&self) -> BackendProbe;

    /// Whether the backend usually uses hardware acceleration.
    fn hardware_accelerated(&self) -> bool;

    /// Codecs this backend can decode.
    fn decodes(&self) -> &[CodecId];

    /// Codecs this backend can encode.
    fn encodes(&self) -> &[CodecId];

    /// True when this backend advertises the requested codec operation.
    fn supports(&self, codec: &CodecId, direction: CodecDirection) -> bool {
        match direction {
            CodecDirection::Decode => self.decodes().iter().any(|known| known == codec),
            CodecDirection::Encode => self.encodes().iter().any(|known| known == codec),
        }
    }
}

impl CodecBackendDescriptor for BackendCapability {
    fn kind(&self) -> BackendKind {
        self.kind
    }

    fn target(&self) -> TargetFamily {
        self.target
    }

    fn source(&self) -> BackendSource {
        self.source
    }

    fn probe(&self) -> BackendProbe {
        self.probe
    }

    fn hardware_accelerated(&self) -> bool {
        self.hardware_accelerated
    }

    fn decodes(&self) -> &[CodecId] {
        &self.decodes
    }

    fn encodes(&self) -> &[CodecId] {
        &self.encodes
    }
}

/// Platform-specific API names used by native adapters.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PlatformCodecApiNames {
    /// Apple CoreMedia/VideoToolbox or CoreAudio constant.
    pub apple: Option<&'static str>,
    /// Android `MediaFormat` MIME type.
    pub android_mime: Option<&'static str>,
    /// WebCodecs codec-string family.
    pub web_codecs: Option<&'static str>,
    /// Windows Media Foundation subtype.
    pub windows_media_foundation: Option<&'static str>,
    /// GStreamer caps family.
    pub gstreamer_caps: Option<&'static str>,
}

/// Return platform API identifiers for codecs intentionally delegated to system codecs.
#[must_use]
pub fn platform_codec_api_names(codec: &CodecId) -> PlatformCodecApiNames {
    match codec {
        CodecId::H264 => PlatformCodecApiNames {
            apple: Some("kCMVideoCodecType_H264"),
            android_mime: Some("video/avc"),
            web_codecs: Some("avc1.* or avc3.*"),
            windows_media_foundation: Some("MFVideoFormat_H264"),
            gstreamer_caps: Some("video/x-h264"),
        },
        CodecId::H265 => PlatformCodecApiNames {
            apple: Some("kCMVideoCodecType_HEVC"),
            android_mime: Some("video/hevc"),
            web_codecs: Some("hev1.* or hvc1.*"),
            windows_media_foundation: Some("MFVideoFormat_HEVC"),
            gstreamer_caps: Some("video/x-h265"),
        },
        CodecId::ProRes => PlatformCodecApiNames {
            apple: Some("kCMVideoCodecType_AppleProRes422*"),
            android_mime: None,
            web_codecs: None,
            windows_media_foundation: None,
            gstreamer_caps: Some("video/x-prores"),
        },
        CodecId::Aac => PlatformCodecApiNames {
            apple: Some("kAudioFormatMPEG4AAC"),
            android_mime: Some("audio/mp4a-latm"),
            web_codecs: Some("mp4a.*"),
            windows_media_foundation: Some("MFAudioFormat_AAC"),
            gstreamer_caps: Some("audio/mpeg, mpegversion=(int)4"),
        },
        CodecId::Eac3 => PlatformCodecApiNames {
            apple: Some("kAudioFormatEnhancedAC3"),
            android_mime: Some("audio/eac3"),
            web_codecs: None,
            windows_media_foundation: Some("MFAudioFormat_Dolby_DDPlus"),
            gstreamer_caps: Some("audio/x-eac3"),
        },
        CodecId::Dts => PlatformCodecApiNames {
            apple: None,
            android_mime: Some("audio/vnd.dts"),
            web_codecs: None,
            windows_media_foundation: Some("MFAudioFormat_DTS"),
            gstreamer_caps: Some("audio/x-dts"),
        },
        CodecId::Wma => PlatformCodecApiNames {
            apple: None,
            android_mime: None,
            web_codecs: None,
            windows_media_foundation: Some("MFAudioFormat_WMAudioV8/MFAudioFormat_WMAudioV9"),
            gstreamer_caps: Some("audio/x-wma"),
        },
        _ => PlatformCodecApiNames {
            apple: None,
            android_mime: None,
            web_codecs: None,
            windows_media_foundation: None,
            gstreamer_caps: None,
        },
    }
}

/// Codecs Flow-Like should prefer through platform/system backends by default.
#[must_use]
pub fn platform_delegated_codec_ids() -> Vec<CodecId> {
    vec![
        CodecId::Dts,
        CodecId::Wma,
        CodecId::ProRes,
        CodecId::Aac,
        CodecId::Eac3,
        CodecId::H264,
        CodecId::H265,
    ]
}

/// Compile-time feature set.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FeatureSet {
    /// Packet/container model and packet-copy helpers.
    pub packet_ops: bool,
    /// Audio DSP helpers.
    pub audio_core: bool,
    /// Optional audio IO/decode libraries.
    pub audio_io: bool,
    /// RGBA frame helpers.
    pub frame_core: bool,
    /// Optional image IO libraries.
    pub image_io: bool,
    /// Preview encoders.
    pub preview: bool,
    /// Subtitle helpers.
    pub subtitles: bool,
    /// Streaming helpers.
    pub streaming: bool,
    /// Default platform codec abstraction lane.
    pub platform_codecs: bool,
    /// Apple codec backend lane.
    pub codec_apple: bool,
    /// Android codec backend lane.
    pub codec_android: bool,
    /// Windows Media Foundation backend lane.
    pub codec_windows: bool,
    /// GStreamer backend lane.
    pub codec_gstreamer: bool,
    /// WebCodecs backend lane.
    pub codec_web: bool,
    /// Rust H.264 backend lane.
    pub codec_h264_rust: bool,
    /// Rust H.265 backend lane.
    pub codec_h265_rust: bool,
    /// Rust AV1 backend lane.
    pub codec_av1_rust: bool,
    /// OpenH264 FFI backend lane.
    pub codec_openh264_ffi: bool,
}

/// Return the feature set compiled into this crate.
#[must_use]
pub const fn compiled_features() -> FeatureSet {
    FeatureSet {
        packet_ops: cfg!(feature = "packet-ops"),
        audio_core: cfg!(feature = "audio-core"),
        audio_io: cfg!(feature = "audio-io"),
        frame_core: cfg!(feature = "frame-core"),
        image_io: cfg!(feature = "image-io"),
        preview: cfg!(feature = "preview"),
        subtitles: cfg!(feature = "subtitles"),
        streaming: cfg!(feature = "streaming"),
        platform_codecs: cfg!(feature = "platform-codecs"),
        codec_apple: cfg!(feature = "codec-apple"),
        codec_android: cfg!(feature = "codec-android"),
        codec_windows: cfg!(feature = "codec-windows"),
        codec_gstreamer: cfg!(feature = "codec-gstreamer"),
        codec_web: cfg!(feature = "codec-web"),
        codec_h264_rust: cfg!(feature = "codec-h264-rust"),
        codec_h265_rust: cfg!(feature = "codec-h265-rust"),
        codec_av1_rust: cfg!(feature = "codec-av1-rust"),
        codec_openh264_ffi: cfg!(feature = "codec-openh264-ffi"),
    }
}

fn platform_or(feature_enabled: bool) -> bool {
    cfg!(feature = "platform-codecs") || feature_enabled
}

/// Recommend target-native codec backends for the current compilation target.
#[must_use]
pub fn recommended_backends_for_current_target() -> Vec<BackendCapability> {
    let mut backends = Vec::new();

    if cfg!(any(
        target_os = "macos",
        target_os = "ios",
        target_os = "tvos"
    )) && platform_or(cfg!(feature = "codec-apple"))
    {
        backends.push(BackendCapability {
            kind: BackendKind::AppleVideoToolbox,
            target: TargetFamily::Apple,
            source: BackendSource::Platform,
            probe: BackendProbe::Runtime,
            hardware_accelerated: true,
            decodes: vec![CodecId::H264, CodecId::H265, CodecId::ProRes],
            encodes: vec![CodecId::H264, CodecId::H265, CodecId::ProRes],
            note: "Apple VideoToolbox lane; codec/profile support must be checked on the running OS/device",
        });
        backends.push(BackendCapability {
            kind: BackendKind::AppleAudioToolbox,
            target: TargetFamily::Apple,
            source: BackendSource::Platform,
            probe: BackendProbe::Runtime,
            hardware_accelerated: false,
            decodes: vec![CodecId::Aac, CodecId::Eac3],
            encodes: vec![CodecId::Aac],
            note: "Apple AudioToolbox/CoreAudio lane; concrete codec availability must be checked through AudioConverter",
        });
    }

    if cfg!(target_os = "android") && platform_or(cfg!(feature = "codec-android")) {
        backends.push(BackendCapability {
            kind: BackendKind::AndroidMediaCodec,
            target: TargetFamily::Android,
            source: BackendSource::Platform,
            probe: BackendProbe::Runtime,
            hardware_accelerated: true,
            decodes: vec![
                CodecId::H264,
                CodecId::H265,
                CodecId::AV1,
                CodecId::VP8,
                CodecId::VP9,
                CodecId::Mpeg2Video,
                CodecId::Mpeg4Part2,
                CodecId::Aac,
                CodecId::Eac3,
                CodecId::Dts,
            ],
            encodes: vec![
                CodecId::H264,
                CodecId::H265,
                CodecId::VP8,
                CodecId::VP9,
                CodecId::Aac,
            ],
            note: "Android MediaCodec lane; use MediaCodecList/MediaCodecInfo on-device before constructing nodes",
        });
    }

    if cfg!(target_family = "wasm") && platform_or(cfg!(feature = "codec-web")) {
        backends.push(BackendCapability {
            kind: BackendKind::WebCodecs,
            target: TargetFamily::Web,
            source: BackendSource::Platform,
            probe: BackendProbe::Runtime,
            hardware_accelerated: true,
            decodes: vec![
                CodecId::H264,
                CodecId::H265,
                CodecId::AV1,
                CodecId::VP8,
                CodecId::VP9,
                CodecId::Aac,
            ],
            encodes: vec![
                CodecId::H264,
                CodecId::H265,
                CodecId::AV1,
                CodecId::VP8,
                CodecId::VP9,
                CodecId::Aac,
            ],
            note: "WebCodecs lane; browser support is checked with isConfigSupported before use",
        });
    }

    if cfg!(target_os = "windows") && platform_or(cfg!(feature = "codec-windows")) {
        backends.push(BackendCapability {
            kind: BackendKind::WindowsMediaFoundation,
            target: TargetFamily::Windows,
            source: BackendSource::Platform,
            probe: BackendProbe::Runtime,
            hardware_accelerated: true,
            decodes: vec![
                CodecId::H264,
                CodecId::H265,
                CodecId::Aac,
                CodecId::Eac3,
                CodecId::Dts,
                CodecId::Wma,
            ],
            encodes: vec![CodecId::H264, CodecId::Aac, CodecId::Wma],
            note: "Windows Media Foundation lane; enumerate MFTs on the running host before use",
        });
    }

    if cfg!(target_os = "linux") && platform_or(cfg!(feature = "codec-gstreamer")) {
        backends.push(BackendCapability {
            kind: BackendKind::GStreamer,
            target: TargetFamily::Linux,
            source: BackendSource::Platform,
            probe: BackendProbe::Runtime,
            hardware_accelerated: false,
            decodes: platform_delegated_codec_ids(),
            encodes: vec![
                CodecId::H264,
                CodecId::H265,
                CodecId::ProRes,
                CodecId::Aac,
                CodecId::Eac3,
            ],
            note: "GStreamer system-plugin lane for Linux; exact codec coverage depends on installed plugins",
        });
    }

    if cfg!(any(
        feature = "codec-h264-rust",
        feature = "codec-h265-rust",
        feature = "codec-av1-rust"
    )) {
        let mut decodes = Vec::new();
        let mut encodes = Vec::new();
        if cfg!(feature = "codec-h264-rust") {
            decodes.push(CodecId::H264);
        }
        if cfg!(feature = "codec-h265-rust") {
            decodes.push(CodecId::H265);
        }
        if cfg!(feature = "codec-av1-rust") {
            encodes.push(CodecId::AV1);
        }

        backends.push(BackendCapability {
            kind: BackendKind::RustSoftware,
            target: TargetFamily::Native,
            source: BackendSource::BundledNative,
            probe: BackendProbe::Static,
            hardware_accelerated: false,
            decodes,
            encodes,
            note: "Rust-native codec crates compiled into this library",
        });
    }

    if cfg!(feature = "codec-openh264-ffi") {
        backends.push(BackendCapability {
            kind: BackendKind::OpenH264Ffi,
            target: TargetFamily::Native,
            source: BackendSource::ExternalFfi,
            probe: BackendProbe::Runtime,
            hardware_accelerated: false,
            decodes: vec![CodecId::H264],
            encodes: vec![CodecId::H264],
            note: "OpenH264 external-library lane; exact library availability is checked at runtime",
        });
    }

    backends
}

/// Preferred backend kind for a codec operation on the current compilation target.
#[must_use]
pub fn preferred_backend_for_codec(
    codec: &CodecId,
    direction: CodecDirection,
) -> Option<BackendKind> {
    recommended_backends_for_current_target()
        .into_iter()
        .find(|backend| backend.supports(codec, direction))
        .map(|backend| backend.kind)
}
