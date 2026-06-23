use crate::{
    audio::AudioFrame,
    backend::{BackendSource, recommended_backends_for_current_target},
    error::Result,
    frame::RgbaFrame,
    packet::EncodedPacket,
};
use bytes::Bytes;
use std::fmt;

/// High-level media kind carried by a codec.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum MediaType {
    /// Encoded video packets or decoded video frames.
    Video,
    /// Encoded audio packets or decoded PCM.
    Audio,
    /// Still image codecs decoded into RGBA frames.
    Image,
    /// Timed text or sidecar subtitles.
    Subtitle,
    /// Data/metadata tracks.
    Data,
}

/// Codec identifier used by packet-copy and backend dispatch.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum CodecId {
    /// H.264 / AVC.
    H264,
    /// H.265 / HEVC.
    H265,
    /// AV1.
    AV1,
    /// VP8.
    VP8,
    /// VP9.
    VP9,
    /// MPEG-1 video.
    Mpeg1Video,
    /// MPEG-2 video.
    Mpeg2Video,
    /// MPEG-4 Part 2 video.
    Mpeg4Part2,
    /// Apple ProRes.
    ProRes,
    /// Theora video.
    Theora,
    /// Dirac video.
    Dirac,
    /// Raw/uncompressed video frames carried in packets.
    RawVideo,
    /// AAC audio.
    Aac,
    /// AC-3 audio.
    Ac3,
    /// Enhanced AC-3 audio.
    Eac3,
    /// Adaptive differential PCM audio.
    Adpcm,
    /// Apple Lossless Audio Codec.
    Alac,
    /// Opus audio.
    Opus,
    /// FLAC audio.
    Flac,
    /// MPEG Layer I audio.
    Mp1,
    /// MPEG Layer II audio.
    Mp2,
    /// MPEG Layer III audio.
    Mp3,
    /// PCM audio.
    Pcm,
    /// Vorbis audio.
    Vorbis,
    /// Speex audio.
    Speex,
    /// DTS Coherent Acoustics audio.
    Dts,
    /// Windows Media Audio.
    Wma,
    /// WavPack audio.
    WavPack,
    /// PNG still image.
    Png,
    /// JPEG still image.
    Jpeg,
    /// GIF still image.
    Gif,
    /// WebP still image.
    WebP,
    /// AVIF still image.
    Avif,
    /// SubRip text.
    Srt,
    /// WebVTT text.
    WebVtt,
    /// Unknown or not-yet-modeled codec identifier.
    Unknown(String),
}

impl CodecId {
    /// Return the broad media type for known codecs.
    #[must_use]
    pub fn media_type(&self) -> Option<MediaType> {
        match self {
            Self::H264
            | Self::H265
            | Self::AV1
            | Self::VP8
            | Self::VP9
            | Self::Mpeg1Video
            | Self::Mpeg2Video
            | Self::Mpeg4Part2
            | Self::ProRes
            | Self::Theora
            | Self::Dirac
            | Self::RawVideo => Some(MediaType::Video),
            Self::Aac
            | Self::Ac3
            | Self::Eac3
            | Self::Adpcm
            | Self::Alac
            | Self::Opus
            | Self::Flac
            | Self::Mp1
            | Self::Mp2
            | Self::Mp3
            | Self::Pcm
            | Self::Vorbis
            | Self::Speex
            | Self::Dts
            | Self::Wma
            | Self::WavPack => Some(MediaType::Audio),
            Self::Png | Self::Jpeg | Self::Gif | Self::WebP | Self::Avif => Some(MediaType::Image),
            Self::Srt | Self::WebVtt => Some(MediaType::Subtitle),
            Self::Unknown(_) => None,
        }
    }

    /// True when this is a known video codec.
    #[must_use]
    pub fn is_video(&self) -> bool {
        self.media_type() == Some(MediaType::Video)
    }

    /// True when this is a known audio codec.
    #[must_use]
    pub fn is_audio(&self) -> bool {
        self.media_type() == Some(MediaType::Audio)
    }

    /// Stable string representation suitable for logs and metadata.
    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            Self::H264 => "h264",
            Self::H265 => "h265",
            Self::AV1 => "av1",
            Self::VP8 => "vp8",
            Self::VP9 => "vp9",
            Self::Mpeg1Video => "mpeg1video",
            Self::Mpeg2Video => "mpeg2video",
            Self::Mpeg4Part2 => "mpeg4-part2",
            Self::ProRes => "prores",
            Self::Theora => "theora",
            Self::Dirac => "dirac",
            Self::RawVideo => "rawvideo",
            Self::Aac => "aac",
            Self::Ac3 => "ac3",
            Self::Eac3 => "eac3",
            Self::Adpcm => "adpcm",
            Self::Alac => "alac",
            Self::Opus => "opus",
            Self::Flac => "flac",
            Self::Mp1 => "mp1",
            Self::Mp2 => "mp2",
            Self::Mp3 => "mp3",
            Self::Pcm => "pcm",
            Self::Vorbis => "vorbis",
            Self::Speex => "speex",
            Self::Dts => "dts",
            Self::Wma => "wma",
            Self::WavPack => "wavpack",
            Self::Png => "png",
            Self::Jpeg => "jpeg",
            Self::Gif => "gif",
            Self::WebP => "webp",
            Self::Avif => "avif",
            Self::Srt => "srt",
            Self::WebVtt => "webvtt",
            Self::Unknown(value) => value.as_str(),
        }
    }
}

/// Known compressed or raw video codec identifiers modeled by this crate.
#[must_use]
pub fn known_video_codecs() -> Vec<CodecId> {
    vec![
        CodecId::H264,
        CodecId::H265,
        CodecId::AV1,
        CodecId::VP8,
        CodecId::VP9,
        CodecId::Mpeg1Video,
        CodecId::Mpeg2Video,
        CodecId::Mpeg4Part2,
        CodecId::ProRes,
        CodecId::Theora,
        CodecId::Dirac,
        CodecId::RawVideo,
    ]
}

/// Known audio codec identifiers modeled by this crate.
#[must_use]
pub fn known_audio_codecs() -> Vec<CodecId> {
    vec![
        CodecId::Aac,
        CodecId::Ac3,
        CodecId::Eac3,
        CodecId::Adpcm,
        CodecId::Alac,
        CodecId::Opus,
        CodecId::Flac,
        CodecId::Mp1,
        CodecId::Mp2,
        CodecId::Mp3,
        CodecId::Pcm,
        CodecId::Vorbis,
        CodecId::Speex,
        CodecId::Dts,
        CodecId::Wma,
        CodecId::WavPack,
    ]
}

/// Known still-image codec identifiers modeled by this crate.
#[must_use]
pub fn known_image_codecs() -> Vec<CodecId> {
    vec![
        CodecId::Png,
        CodecId::Jpeg,
        CodecId::Gif,
        CodecId::WebP,
        CodecId::Avif,
    ]
}

/// Codec IDs whose encoded packets can be validated and passed through.
#[must_use]
pub fn packet_copy_codec_ids() -> Vec<CodecId> {
    let mut codecs = known_video_codecs();
    codecs.extend(known_audio_codecs());
    codecs
}

/// Audio codec IDs covered by the compiled Symphonia decoder feature set.
#[must_use]
pub fn symphonia_audio_decode_codec_ids() -> Vec<CodecId> {
    #[cfg(feature = "audio-io")]
    {
        vec![
            CodecId::Aac,
            CodecId::Adpcm,
            CodecId::Flac,
            CodecId::Mp1,
            CodecId::Mp2,
            CodecId::Mp3,
            CodecId::Pcm,
            CodecId::Vorbis,
        ]
    }

    #[cfg(not(feature = "audio-io"))]
    {
        Vec::new()
    }
}

impl fmt::Display for CodecId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Codec-private initialization data such as AVCDecoderConfigurationRecord.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CodecConfig {
    /// Codec the config belongs to.
    pub codec: CodecId,
    /// Opaque codec-specific bytes.
    pub extra_data: Bytes,
    /// Optional human-readable description.
    pub description: Option<String>,
}

impl CodecConfig {
    /// Create a codec config without a description.
    #[must_use]
    pub fn new(codec: CodecId, extra_data: impl Into<Bytes>) -> Self {
        Self {
            codec,
            extra_data: extra_data.into(),
            description: None,
        }
    }

    /// Attach a human-readable description.
    #[must_use]
    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }
}

/// Direction of a codec operation.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum CodecDirection {
    /// Decode/read an external representation into crate data.
    Decode,
    /// Encode/write crate data into an external representation.
    Encode,
}

/// Implementation category for a codec surface.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum CodecImplementationKind {
    /// Text sidecar parser/writer such as SRT or WebVTT.
    TextSubtitle,
    /// Encoded packets can be copied/remuxed without decoding.
    PacketCopy,
    /// File/container bytes can be decoded or encoded through an audio I/O backend.
    AudioFile,
    /// Still-image bytes can be decoded or encoded through an image backend.
    ImageStill,
    /// Packet-to-frame or frame-to-packet implementation is expected from a backend.
    Backend,
    /// Codec operation is delegated to a target platform or system codec API.
    PlatformBackend,
}

/// Support record for a codec in this crate.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CodecSupport {
    /// Codec this record describes.
    pub codec: CodecId,
    /// Media type when known.
    pub media_type: Option<MediaType>,
    /// Implementation category.
    pub kind: CodecImplementationKind,
    /// Whether the implementation can decode/read.
    pub can_decode: bool,
    /// Whether the implementation can encode/write.
    pub can_encode: bool,
    /// Human-readable note about the current support level.
    pub note: &'static str,
}

impl CodecSupport {
    /// True when this support record satisfies a requested direction.
    #[must_use]
    pub fn supports(&self, direction: CodecDirection) -> bool {
        match direction {
            CodecDirection::Decode => self.can_decode,
            CodecDirection::Encode => self.can_encode,
        }
    }
}

/// Registry of implemented and packet-copy codec surfaces.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CodecRegistry {
    support: Vec<CodecSupport>,
}

impl CodecRegistry {
    /// Create a registry from explicit support records.
    #[must_use]
    pub fn new(support: Vec<CodecSupport>) -> Self {
        Self { support }
    }

    /// Built-in support in the portable crate.
    #[must_use]
    pub fn builtin() -> Self {
        Self::new(builtin_codec_support())
    }

    /// All support records.
    #[must_use]
    pub fn support(&self) -> &[CodecSupport] {
        &self.support
    }

    /// Find support records for a codec.
    pub fn for_codec(&self, codec: &CodecId) -> impl Iterator<Item = &CodecSupport> {
        self.support
            .iter()
            .filter(move |support| &support.codec == codec)
    }

    /// True when a codec has a concrete decoder/reader for the requested kind.
    #[must_use]
    pub fn supports_decode(&self, codec: &CodecId, kind: CodecImplementationKind) -> bool {
        self.for_codec(codec)
            .any(|support| support.kind == kind && support.can_decode)
    }

    /// True when a codec has a concrete encoder/writer for the requested kind.
    #[must_use]
    pub fn supports_encode(&self, codec: &CodecId, kind: CodecImplementationKind) -> bool {
        self.for_codec(codec)
            .any(|support| support.kind == kind && support.can_encode)
    }
}

/// Return the concrete codec surfaces implemented by this crate today.
#[must_use]
pub fn builtin_codec_support() -> Vec<CodecSupport> {
    let mut support = vec![
        CodecSupport {
            codec: CodecId::Srt,
            media_type: Some(MediaType::Subtitle),
            kind: CodecImplementationKind::TextSubtitle,
            can_decode: true,
            can_encode: true,
            note: "SRT sidecar text parse/write plus frame burn-in helpers",
        },
        CodecSupport {
            codec: CodecId::WebVtt,
            media_type: Some(MediaType::Subtitle),
            kind: CodecImplementationKind::TextSubtitle,
            can_decode: true,
            can_encode: true,
            note: "WebVTT sidecar text parse/write plus frame burn-in helpers",
        },
    ];

    for codec in packet_copy_codec_ids() {
        support.push(CodecSupport {
            media_type: codec.media_type(),
            codec,
            kind: CodecImplementationKind::PacketCopy,
            can_decode: true,
            can_encode: true,
            note: "encoded packets can be modeled and copied/remuxed; frame decode/encode is backend work",
        });
    }

    support.push(CodecSupport {
        codec: CodecId::RawVideo,
        media_type: Some(MediaType::Video),
        kind: CodecImplementationKind::Backend,
        can_decode: true,
        can_encode: true,
        note: "raw RGBA video packets can be decoded to and encoded from RgbaFrame without external codecs",
    });

    for backend in recommended_backends_for_current_target()
        .into_iter()
        .filter(|backend| backend.source == BackendSource::Platform)
    {
        let mut codecs = backend.decodes.clone();
        for codec in &backend.encodes {
            if !codecs.iter().any(|known| known == codec) {
                codecs.push(codec.clone());
            }
        }

        for codec in codecs {
            let can_decode = backend.decodes.iter().any(|known| known == &codec);
            let can_encode = backend.encodes.iter().any(|known| known == &codec);
            support.push(CodecSupport {
                media_type: codec.media_type(),
                codec,
                kind: CodecImplementationKind::PlatformBackend,
                can_decode,
                can_encode,
                note: backend.note,
            });
        }
    }

    #[cfg(feature = "codec-h264-rust")]
    support.push(CodecSupport {
        codec: CodecId::H264,
        media_type: Some(MediaType::Video),
        kind: CodecImplementationKind::Backend,
        can_decode: true,
        can_encode: false,
        note: "H.264 video can be decoded to RgbaFrame through the Rust-native rust_h264 backend",
    });

    #[cfg(feature = "codec-h265-rust")]
    support.push(CodecSupport {
        codec: CodecId::H265,
        media_type: Some(MediaType::Video),
        kind: CodecImplementationKind::Backend,
        can_decode: true,
        can_encode: false,
        note: "H.265/HEVC video can be decoded to RgbaFrame through the Rust-native rust_h265 backend",
    });

    #[cfg(feature = "codec-av1-rust")]
    support.push(CodecSupport {
        codec: CodecId::AV1,
        media_type: Some(MediaType::Video),
        kind: CodecImplementationKind::Backend,
        can_decode: false,
        can_encode: true,
        note: "AV1 video can be encoded from RgbaFrame through the Rust-native rav1e backend",
    });

    #[cfg(feature = "audio-io")]
    {
        support.push(CodecSupport {
            codec: CodecId::Pcm,
            media_type: Some(MediaType::Audio),
            kind: CodecImplementationKind::AudioFile,
            can_decode: true,
            can_encode: true,
            note: "WAV PCM decode/write through the hound backend",
        });

        for codec in symphonia_audio_decode_codec_ids() {
            support.push(CodecSupport {
                media_type: codec.media_type(),
                codec,
                kind: CodecImplementationKind::AudioFile,
                can_decode: true,
                can_encode: false,
                note: "audio-file decode through the Symphonia backend",
            });
        }

        for codec in [
            CodecId::Aac,
            CodecId::Flac,
            CodecId::Mp3,
            CodecId::Opus,
            CodecId::Vorbis,
        ] {
            support.push(CodecSupport {
                media_type: codec.media_type(),
                codec,
                kind: CodecImplementationKind::Backend,
                can_decode: true,
                can_encode: false,
                note: "compressed audio packets can be decoded to AudioFrame through SymphoniaPacketAudioDecoder",
            });
        }
    }

    #[cfg(feature = "image-io")]
    {
        for codec in known_image_codecs() {
            support.push(CodecSupport {
                media_type: codec.media_type(),
                codec,
                kind: CodecImplementationKind::ImageStill,
                can_decode: true,
                can_encode: true,
                note: "still-image decode/write through the image backend",
            });
        }
    }

    support
}

/// Common descriptor shared by codec adapters.
pub trait CodecDescriptor {
    /// Human-readable adapter name.
    fn name(&self) -> &'static str;

    /// Codec handled by this adapter.
    fn codec_id(&self) -> CodecId;

    /// Media type handled by this adapter.
    fn media_type(&self) -> Option<MediaType> {
        self.codec_id().media_type()
    }
}

/// Generic decode/read trait.
pub trait Decoder: CodecDescriptor {
    /// External input representation.
    type Input: ?Sized;
    /// Decoded output representation.
    type Output;

    /// Decode/read input into output.
    fn decode(&mut self, input: &Self::Input) -> Result<Self::Output>;

    /// Flush delayed output.
    fn flush(&mut self) -> Result<Option<Self::Output>> {
        Ok(None)
    }
}

/// Generic encode/write trait.
pub trait Encoder: CodecDescriptor {
    /// Input representation to encode/write.
    type Input: ?Sized;
    /// Encoded output representation.
    type Output;

    /// Encode/write input into output.
    fn encode(&mut self, input: &Self::Input) -> Result<Self::Output>;

    /// Finish delayed output.
    fn finish(&mut self) -> Result<Option<Self::Output>> {
        Ok(None)
    }
}

/// Packet-to-video-frame decoder trait for future backend adapters.
pub trait VideoDecoder: CodecDescriptor {
    /// Decode one encoded video packet into zero or more RGBA frames.
    fn decode_packet(&mut self, packet: &EncodedPacket) -> Result<Vec<RgbaFrame>>;

    /// Flush delayed video frames.
    fn flush(&mut self) -> Result<Vec<RgbaFrame>> {
        Ok(Vec::new())
    }
}

/// Video-frame-to-packet encoder trait for future backend adapters.
pub trait VideoEncoder: CodecDescriptor {
    /// Encode one RGBA frame into zero or more encoded packets.
    fn encode_frame(&mut self, frame: &RgbaFrame, pts: i64) -> Result<Vec<EncodedPacket>>;

    /// Finish delayed video packets.
    fn finish(&mut self) -> Result<Vec<EncodedPacket>> {
        Ok(Vec::new())
    }
}

/// Packet-to-PCM decoder trait for future audio codec adapters.
pub trait AudioDecoder: CodecDescriptor {
    /// Decode one encoded audio packet into zero or more PCM frames.
    fn decode_packet(&mut self, packet: &EncodedPacket) -> Result<Vec<AudioFrame>>;

    /// Flush delayed audio frames.
    fn flush(&mut self) -> Result<Vec<AudioFrame>> {
        Ok(Vec::new())
    }
}

/// PCM-to-packet encoder trait for future audio codec adapters.
pub trait AudioEncoder: CodecDescriptor {
    /// Encode one PCM frame into zero or more encoded packets.
    fn encode_frame(&mut self, frame: &AudioFrame) -> Result<Vec<EncodedPacket>>;

    /// Finish delayed audio packets.
    fn finish(&mut self) -> Result<Vec<EncodedPacket>> {
        Ok(Vec::new())
    }
}

#[cfg(test)]
mod tests {
    use super::{CodecImplementationKind, CodecRegistry, CodecSupport};
    use crate::codec::CodecId;

    #[test]
    fn registry_reports_builtin_support_precisely() {
        let registry = CodecRegistry::builtin();

        assert!(registry.supports_decode(&CodecId::Srt, CodecImplementationKind::TextSubtitle));
        assert!(registry.supports_encode(&CodecId::WebVtt, CodecImplementationKind::TextSubtitle));
        assert!(registry.supports_decode(&CodecId::H264, CodecImplementationKind::PacketCopy));
        #[cfg(feature = "platform-codecs")]
        {
            if cfg!(any(
                target_os = "android",
                target_os = "linux",
                target_os = "macos",
                target_os = "ios",
                target_os = "tvos",
                target_os = "windows",
                target_family = "wasm"
            )) {
                assert!(
                    registry
                        .supports_decode(&CodecId::H264, CodecImplementationKind::PlatformBackend)
                );
            }
        }
        #[cfg(not(feature = "codec-h264-rust"))]
        assert!(!registry.supports_decode(&CodecId::H264, CodecImplementationKind::Backend));
        #[cfg(feature = "codec-h264-rust")]
        assert!(registry.supports_decode(&CodecId::H264, CodecImplementationKind::Backend));
        assert!(registry.supports_decode(&CodecId::RawVideo, CodecImplementationKind::Backend));
        assert!(registry.supports_encode(&CodecId::RawVideo, CodecImplementationKind::Backend));
        #[cfg(feature = "codec-av1-rust")]
        assert!(registry.supports_encode(&CodecId::AV1, CodecImplementationKind::Backend));
    }

    #[test]
    fn support_record_matches_direction() {
        let support = CodecSupport {
            codec: CodecId::Srt,
            media_type: Some(crate::codec::MediaType::Subtitle),
            kind: CodecImplementationKind::TextSubtitle,
            can_decode: true,
            can_encode: false,
            note: "test",
        };

        assert!(support.supports(super::CodecDirection::Decode));
        assert!(!support.supports(super::CodecDirection::Encode));
    }

    #[test]
    fn known_image_codecs_report_image_media_type() {
        assert_eq!(CodecId::Png.media_type(), Some(super::MediaType::Image));
        assert_eq!(CodecId::Jpeg.media_type(), Some(super::MediaType::Image));
        assert_eq!(CodecId::Gif.media_type(), Some(super::MediaType::Image));
        assert_eq!(CodecId::WebP.media_type(), Some(super::MediaType::Image));
        assert_eq!(CodecId::Avif.media_type(), Some(super::MediaType::Image));
    }

    #[test]
    fn codec_ids_have_stable_strings() {
        assert_eq!(CodecId::VP8.as_str(), "vp8");
        assert_eq!(CodecId::Mp3.as_str(), "mp3");
        assert_eq!(CodecId::Vorbis.as_str(), "vorbis");
        assert_eq!(CodecId::Png.as_str(), "png");
        assert_eq!(CodecId::Jpeg.as_str(), "jpeg");
        assert_eq!(CodecId::WebP.as_str(), "webp");
    }

    #[test]
    fn modeled_codec_lists_cover_common_audio_video_and_images() {
        assert!(super::known_video_codecs().contains(&CodecId::VP8));
        assert!(super::known_video_codecs().contains(&CodecId::ProRes));
        assert!(super::known_audio_codecs().contains(&CodecId::Mp3));
        assert!(super::known_audio_codecs().contains(&CodecId::Vorbis));
        assert!(super::known_image_codecs().contains(&CodecId::Avif));
    }
}
