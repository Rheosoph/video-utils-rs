use crate::{
    codec::{CodecId, MediaType},
    error::{Error, Result},
    media::{MediaInfo, StreamInfo},
    packet::EncodedPacket,
};
use bytes::Bytes;
use object_store::path::Path;
use std::fmt;

/// Media container formats modeled by the portable remux planner.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ContainerFormat {
    /// ISO BMFF / MPEG-4 Part 14 files (`.mp4`, `.m4v`, `.m4a`).
    Mp4,
    /// QuickTime movie files (`.mov`, `.qt`).
    QuickTime,
    /// WebM files.
    WebM,
    /// Matroska files (`.mkv`, `.mka`).
    Matroska,
    /// MPEG transport stream.
    MpegTs,
    /// MPEG program stream.
    MpegPs,
    /// AVI.
    Avi,
    /// Flash Video.
    Flv,
    /// Ogg container.
    Ogg,
    /// RIFF/WAVE audio.
    Wav,
    /// AIFF/AIFC audio.
    Aiff,
    /// Codec elementary stream with no rich container metadata.
    RawElementary,
}

impl ContainerFormat {
    /// Detect a container from an object key extension.
    #[must_use]
    pub fn from_path(path: &Path) -> Option<Self> {
        path.extension().and_then(Self::from_extension)
    }

    /// Detect a container from a file extension, with or without a leading dot.
    #[must_use]
    pub fn from_extension(extension: &str) -> Option<Self> {
        let extension = extension.trim_start_matches('.').to_ascii_lowercase();
        match extension.as_str() {
            "mp4" | "m4v" | "m4a" | "m4b" => Some(Self::Mp4),
            "mov" | "qt" => Some(Self::QuickTime),
            "webm" => Some(Self::WebM),
            "mkv" | "mka" | "mks" => Some(Self::Matroska),
            "ts" | "m2ts" | "mts" => Some(Self::MpegTs),
            "mpg" | "mpeg" | "vob" => Some(Self::MpegPs),
            "avi" => Some(Self::Avi),
            "flv" => Some(Self::Flv),
            "ogg" | "ogv" | "oga" | "opus" => Some(Self::Ogg),
            "wav" => Some(Self::Wav),
            "aif" | "aiff" | "aifc" => Some(Self::Aiff),
            "h264" | "avc" | "h265" | "hevc" | "av1" | "ivf" | "aac" | "ac3" | "eac3" | "mp3"
            | "flac" => Some(Self::RawElementary),
            _ => None,
        }
    }

    /// Detect a container from leading bytes.
    #[must_use]
    pub fn from_magic(bytes: &[u8]) -> Option<Self> {
        if bytes.len() >= 12 && &bytes[4..8] == b"ftyp" {
            return if &bytes[8..12] == b"qt  " {
                Some(Self::QuickTime)
            } else {
                Some(Self::Mp4)
            };
        }

        if bytes.starts_with(b"FLV") {
            return Some(Self::Flv);
        }
        if bytes.starts_with(b"OggS") {
            return Some(Self::Ogg);
        }
        if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WAVE" {
            return Some(Self::Wav);
        }
        if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"AVI " {
            return Some(Self::Avi);
        }
        if bytes.len() >= 12
            && &bytes[0..4] == b"FORM"
            && (&bytes[8..12] == b"AIFF" || &bytes[8..12] == b"AIFC")
        {
            return Some(Self::Aiff);
        }
        if bytes.starts_with(&[0x00, 0x00, 0x01, 0xba]) {
            return Some(Self::MpegPs);
        }
        if bytes.first() == Some(&0x47) {
            return Some(Self::MpegTs);
        }
        if bytes.starts_with(&[0x1a, 0x45, 0xdf, 0xa3]) {
            if bytes.windows(4).any(|window| window == b"webm") {
                return Some(Self::WebM);
            }
            return Some(Self::Matroska);
        }

        None
    }

    /// Stable lowercase identifier.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Mp4 => "mp4",
            Self::QuickTime => "mov",
            Self::WebM => "webm",
            Self::Matroska => "matroska",
            Self::MpegTs => "mpeg-ts",
            Self::MpegPs => "mpeg-ps",
            Self::Avi => "avi",
            Self::Flv => "flv",
            Self::Ogg => "ogg",
            Self::Wav => "wav",
            Self::Aiff => "aiff",
            Self::RawElementary => "raw",
        }
    }

    /// Human-readable display name.
    #[must_use]
    pub const fn display_name(self) -> &'static str {
        match self {
            Self::Mp4 => "MPEG-4 / ISO BMFF",
            Self::QuickTime => "QuickTime",
            Self::WebM => "WebM",
            Self::Matroska => "Matroska",
            Self::MpegTs => "MPEG transport stream",
            Self::MpegPs => "MPEG program stream",
            Self::Avi => "AVI",
            Self::Flv => "Flash Video",
            Self::Ogg => "Ogg",
            Self::Wav => "WAVE",
            Self::Aiff => "AIFF",
            Self::RawElementary => "raw elementary stream",
        }
    }

    /// Common object key extensions for this format.
    #[must_use]
    pub const fn extensions(self) -> &'static [&'static str] {
        match self {
            Self::Mp4 => &["mp4", "m4v", "m4a", "m4b"],
            Self::QuickTime => &["mov", "qt"],
            Self::WebM => &["webm"],
            Self::Matroska => &["mkv", "mka", "mks"],
            Self::MpegTs => &["ts", "m2ts", "mts"],
            Self::MpegPs => &["mpg", "mpeg", "vob"],
            Self::Avi => &["avi"],
            Self::Flv => &["flv"],
            Self::Ogg => &["ogg", "ogv", "oga", "opus"],
            Self::Wav => &["wav"],
            Self::Aiff => &["aif", "aiff", "aifc"],
            Self::RawElementary => &[
                "h264", "avc", "h265", "hevc", "av1", "ivf", "aac", "ac3", "eac3", "mp3", "flac",
            ],
        }
    }

    /// True when this container can carry at least one stream of this media type.
    #[must_use]
    pub fn supports_media_type(self, media_type: MediaType) -> bool {
        match media_type {
            MediaType::Video => matches!(
                self,
                Self::Mp4
                    | Self::QuickTime
                    | Self::WebM
                    | Self::Matroska
                    | Self::MpegTs
                    | Self::MpegPs
                    | Self::Avi
                    | Self::Flv
                    | Self::Ogg
                    | Self::RawElementary
            ),
            MediaType::Audio => matches!(
                self,
                Self::Mp4
                    | Self::QuickTime
                    | Self::WebM
                    | Self::Matroska
                    | Self::MpegTs
                    | Self::MpegPs
                    | Self::Avi
                    | Self::Flv
                    | Self::Ogg
                    | Self::Wav
                    | Self::Aiff
                    | Self::RawElementary
            ),
            MediaType::Subtitle => matches!(self, Self::Matroska),
            MediaType::Image | MediaType::Data => false,
        }
    }

    /// True when the container policy allows this codec to be packet-copied into the format.
    #[must_use]
    pub fn supports_codec(self, codec: &CodecId) -> bool {
        use CodecId::{
            AV1, Aac, Ac3, Adpcm, Alac, Dirac, Dts, Eac3, Flac, H264, H265, Mp1, Mp2, Mp3,
            Mpeg1Video, Mpeg2Video, Mpeg4Part2, Opus, Pcm, ProRes, RawVideo, Speex, Srt, Theora,
            VP8, VP9, Vorbis, WavPack, WebVtt, Wma,
        };

        match self {
            Self::Mp4 => matches!(
                codec,
                H264 | H265 | AV1 | VP9 | Mpeg4Part2 | Aac | Ac3 | Eac3 | Alac | Flac | Mp3 | Opus
            ),
            Self::QuickTime => matches!(
                codec,
                H264 | H265 | Mpeg4Part2 | ProRes | RawVideo | Aac | Alac | Mp3 | Pcm
            ),
            Self::WebM => matches!(codec, VP8 | VP9 | AV1 | Opus | Vorbis),
            Self::Matroska => matches!(
                codec,
                H264 | H265
                    | AV1
                    | VP8
                    | VP9
                    | Mpeg1Video
                    | Mpeg2Video
                    | Mpeg4Part2
                    | ProRes
                    | Theora
                    | Dirac
                    | RawVideo
                    | Aac
                    | Ac3
                    | Eac3
                    | Adpcm
                    | Alac
                    | Opus
                    | Flac
                    | Mp1
                    | Mp2
                    | Mp3
                    | Pcm
                    | Vorbis
                    | Speex
                    | Dts
                    | Wma
                    | WavPack
                    | Srt
                    | WebVtt
            ),
            Self::MpegTs => matches!(
                codec,
                H264 | H265 | Mpeg2Video | Aac | Ac3 | Eac3 | Mp2 | Mp3
            ),
            Self::MpegPs => matches!(codec, Mpeg1Video | Mpeg2Video | Mp1 | Mp2 | Mp3 | Ac3 | Pcm),
            Self::Avi => matches!(
                codec,
                H264 | Mpeg1Video | Mpeg2Video | Mpeg4Part2 | RawVideo | Mp3 | Pcm | Ac3
            ),
            Self::Flv => matches!(codec, H264 | Aac | Mp3),
            Self::Ogg => matches!(codec, Theora | Dirac | Vorbis | Opus | Flac | Speex),
            Self::Wav => matches!(codec, Pcm | Adpcm),
            Self::Aiff => matches!(codec, Pcm),
            Self::RawElementary => !matches!(codec, CodecId::Unknown(_)),
        }
    }

    /// True when the declared stream can be carried without transcoding.
    #[must_use]
    pub fn supports_stream(self, stream: &StreamInfo) -> bool {
        if !self.supports_media_type(stream.media_type) {
            return false;
        }
        if let Some(codec_media_type) = stream.codec.media_type()
            && codec_media_type != stream.media_type
        {
            return false;
        }
        self.supports_codec(&stream.codec)
    }
}

impl fmt::Display for ContainerFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Capability surface for a concrete container adapter.
pub trait ContainerAdapter {
    /// Container format handled by this adapter.
    fn container_format(&self) -> ContainerFormat;

    /// True when this adapter can carry at least one stream of this media type.
    fn supports_media_type(&self, media_type: MediaType) -> bool {
        self.container_format().supports_media_type(media_type)
    }

    /// True when this adapter can packet-copy this codec.
    fn supports_codec(&self, codec: &CodecId) -> bool {
        self.container_format().supports_codec(codec)
    }

    /// True when this adapter can packet-copy this declared stream.
    fn supports_stream(&self, stream: &StreamInfo) -> bool {
        self.container_format().supports_stream(stream)
    }
}

impl ContainerAdapter for ContainerFormat {
    fn container_format(&self) -> ContainerFormat {
        *self
    }
}

/// Static policy adapter for a container format.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ContainerPolicy {
    format: ContainerFormat,
}

impl ContainerPolicy {
    /// Create a policy adapter for a container format.
    #[must_use]
    pub const fn new(format: ContainerFormat) -> Self {
        Self { format }
    }
}

impl ContainerAdapter for ContainerPolicy {
    fn container_format(&self) -> ContainerFormat {
        self.format
    }
}

/// Result of demuxing a container into metadata plus encoded packets.
#[derive(Clone, Debug, PartialEq)]
pub struct DemuxedMedia {
    /// Source container format.
    pub format: ContainerFormat,
    /// Container and stream metadata.
    pub media: MediaInfo,
    /// Encoded samples in decode order as reported by the adapter.
    pub packets: Vec<EncodedPacket>,
}

impl DemuxedMedia {
    /// Build a demux result.
    #[must_use]
    pub fn new(format: ContainerFormat, media: MediaInfo, packets: Vec<EncodedPacket>) -> Self {
        Self {
            format,
            media,
            packets,
        }
    }

    /// True when the demuxed container has no encoded packets.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.packets.is_empty()
    }

    /// Number of demuxed encoded packets.
    #[must_use]
    pub fn len(&self) -> usize {
        self.packets.len()
    }
}

/// Decode a container byte object into stream metadata and encoded packets.
pub trait ContainerDemuxer {
    /// Container format handled by this demuxer.
    fn container_format(&self) -> ContainerFormat;

    /// Demux bytes from an object-store object body.
    fn demux_bytes(&self, bytes: &Bytes) -> Result<DemuxedMedia>;

    /// Probe only stream metadata from an object-store object body.
    fn probe_bytes(&self, bytes: &Bytes) -> Result<MediaInfo> {
        self.demux_bytes(bytes).map(|demuxed| demuxed.media)
    }
}

/// Result of muxing stream metadata plus encoded packets into container bytes.
#[derive(Clone, Debug, PartialEq)]
pub struct MuxedMedia {
    /// Target container format.
    pub format: ContainerFormat,
    /// Container and stream metadata used for muxing.
    pub media: MediaInfo,
    /// Encoded container object bytes.
    pub bytes: Bytes,
    /// Number of encoded packets submitted to the muxer.
    pub packet_count: usize,
}

impl MuxedMedia {
    /// Build a mux result.
    #[must_use]
    pub fn new(
        format: ContainerFormat,
        media: MediaInfo,
        bytes: Bytes,
        packet_count: usize,
    ) -> Self {
        Self {
            format,
            media,
            bytes,
            packet_count,
        }
    }

    /// Bytes written by the muxer.
    #[must_use]
    pub fn len(&self) -> usize {
        self.bytes.len()
    }

    /// True when the muxer produced no bytes.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }
}

/// Encode stream metadata and packets into a container byte object.
pub trait ContainerMuxer {
    /// Container format handled by this muxer.
    fn container_format(&self) -> ContainerFormat;

    /// True when this muxer can accept the declared stream.
    fn supports_stream(&self, stream: &StreamInfo) -> bool {
        self.container_format().supports_stream(stream)
    }

    /// Mux packets into object-store-ready bytes.
    fn mux_bytes(&self, media: &MediaInfo, packets: &[EncodedPacket]) -> Result<Bytes>;

    /// Mux a demux result into the muxer's target format.
    fn mux_demuxed(&self, demuxed: &DemuxedMedia) -> Result<MuxedMedia> {
        let bytes = self.mux_bytes(&demuxed.media, &demuxed.packets)?;
        Ok(MuxedMedia::new(
            self.container_format(),
            demuxed.media.clone(),
            bytes,
            demuxed.packets.len(),
        ))
    }
}

/// One stream action in a remux plan.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RemuxAction {
    /// The stream can be copied into the target container as encoded packets.
    PacketCopy,
    /// The target can carry this media type, but not this codec.
    TranscodeRequired {
        /// Human-readable reason.
        reason: String,
    },
    /// The target cannot preserve this stream.
    Unsupported {
        /// Human-readable reason.
        reason: String,
    },
}

impl RemuxAction {
    /// True when the action preserves encoded packets without decoding.
    #[must_use]
    pub const fn is_packet_copy(&self) -> bool {
        matches!(self, Self::PacketCopy)
    }

    /// True when a codec transcode is required before muxing.
    #[must_use]
    pub const fn requires_transcode(&self) -> bool {
        matches!(self, Self::TranscodeRequired { .. })
    }

    /// True when the stream cannot be represented in the target.
    #[must_use]
    pub const fn is_unsupported(&self) -> bool {
        matches!(self, Self::Unsupported { .. })
    }
}

/// Per-stream remux decision.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RemuxStreamPlan {
    /// Source track identifier.
    pub track_id: u32,
    /// Declared stream media type.
    pub media_type: MediaType,
    /// Declared stream codec.
    pub codec: CodecId,
    /// Planned action.
    pub action: RemuxAction,
}

/// Container-to-container remux plan.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RemuxPlan {
    /// Source container format.
    pub source: ContainerFormat,
    /// Target container format.
    pub target: ContainerFormat,
    /// Per-stream remux decisions.
    pub streams: Vec<RemuxStreamPlan>,
}

impl RemuxPlan {
    /// True when every stream can be packet-copied into the target container.
    #[must_use]
    pub fn is_packet_copy_only(&self) -> bool {
        !self.streams.is_empty()
            && self
                .streams
                .iter()
                .all(|stream| stream.action.is_packet_copy())
    }

    /// True when one or more streams require transcoding.
    #[must_use]
    pub fn requires_transcode(&self) -> bool {
        self.streams
            .iter()
            .any(|stream| stream.action.requires_transcode())
    }

    /// True when one or more streams cannot be preserved in the target container.
    #[must_use]
    pub fn has_unsupported_streams(&self) -> bool {
        self.streams
            .iter()
            .any(|stream| stream.action.is_unsupported())
    }

    /// Find the plan for a track id.
    #[must_use]
    pub fn stream(&self, track_id: u32) -> Option<&RemuxStreamPlan> {
        self.streams
            .iter()
            .find(|stream| stream.track_id == track_id)
    }
}

/// Plan whether streams can move from one container to another without decoding.
pub fn plan_container_remux(
    source: ContainerFormat,
    target: ContainerFormat,
    media: &MediaInfo,
) -> Result<RemuxPlan> {
    if media.streams.is_empty() {
        return Err(Error::EmptyInput);
    }

    let streams = media
        .streams
        .iter()
        .map(|stream| RemuxStreamPlan {
            track_id: stream.track_id,
            media_type: stream.media_type,
            codec: stream.codec.clone(),
            action: plan_stream_action(source, target, stream),
        })
        .collect();

    Ok(RemuxPlan {
        source,
        target,
        streams,
    })
}

fn plan_stream_action(
    source: ContainerFormat,
    target: ContainerFormat,
    stream: &StreamInfo,
) -> RemuxAction {
    if !source.supports_media_type(stream.media_type) {
        return RemuxAction::Unsupported {
            reason: format!(
                "{} is not modeled as carrying {:?} streams",
                source.display_name(),
                stream.media_type
            ),
        };
    }

    if target.supports_stream(stream) {
        return RemuxAction::PacketCopy;
    }

    if target.supports_media_type(stream.media_type) {
        return RemuxAction::TranscodeRequired {
            reason: format!(
                "{} cannot packet-copy {} into {}",
                stream.codec,
                target.display_name(),
                target.as_str()
            ),
        };
    }

    RemuxAction::Unsupported {
        reason: format!(
            "{} cannot carry {:?} streams",
            target.display_name(),
            stream.media_type
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ContainerAdapter, ContainerFormat, ContainerPolicy, RemuxAction, plan_container_remux,
    };
    use crate::{CodecId, MediaInfo, MediaType, StreamInfo, TimeBase};
    use object_store::path::Path;

    fn mp4_info() -> MediaInfo {
        let mut info = MediaInfo::default();
        info.push_stream(
            StreamInfo::new(
                1,
                MediaType::Video,
                CodecId::H264,
                TimeBase::new(1, 90_000).unwrap(),
            )
            .with_dimensions(1920, 1080),
        );
        info.push_stream(
            StreamInfo::new(
                2,
                MediaType::Audio,
                CodecId::Aac,
                TimeBase::new(1, 48_000).unwrap(),
            )
            .with_audio_format(48_000, 2),
        );
        info
    }

    #[test]
    fn detects_container_from_extension() {
        assert_eq!(
            ContainerFormat::from_path(&Path::from("clips/input.MP4")),
            Some(ContainerFormat::Mp4)
        );
        assert_eq!(
            ContainerFormat::from_path(&Path::from("clips/input.mov")),
            Some(ContainerFormat::QuickTime)
        );
        assert_eq!(
            ContainerFormat::from_path(&Path::from("clips/input.webm")),
            Some(ContainerFormat::WebM)
        );
        assert_eq!(
            ContainerFormat::from_extension(".mkv"),
            Some(ContainerFormat::Matroska)
        );
    }

    #[test]
    fn detects_container_from_magic() {
        assert_eq!(
            ContainerFormat::from_magic(b"\0\0\0\x18ftypisom\0\0\0\0"),
            Some(ContainerFormat::Mp4)
        );
        assert_eq!(
            ContainerFormat::from_magic(b"\0\0\0\x18ftypqt  \0\0\0\0"),
            Some(ContainerFormat::QuickTime)
        );
        assert_eq!(
            ContainerFormat::from_magic(b"OggSabc"),
            Some(ContainerFormat::Ogg)
        );
        assert_eq!(
            ContainerFormat::from_magic(b"RIFF\0\0\0\0WAVEfmt "),
            Some(ContainerFormat::Wav)
        );
    }

    #[test]
    fn policies_model_common_codec_sets() {
        let webm = ContainerPolicy::new(ContainerFormat::WebM);
        assert!(webm.supports_codec(&CodecId::VP9));
        assert!(webm.supports_codec(&CodecId::Opus));
        assert!(!webm.supports_codec(&CodecId::H264));

        assert!(ContainerFormat::Mp4.supports_codec(&CodecId::H264));
        assert!(ContainerFormat::Mp4.supports_codec(&CodecId::Aac));
        assert!(!ContainerFormat::Mp4.supports_codec(&CodecId::Vorbis));
        assert!(ContainerFormat::Matroska.supports_codec(&CodecId::WavPack));
    }

    #[test]
    fn remux_plan_packet_copies_compatible_streams() {
        let plan = plan_container_remux(
            ContainerFormat::Mp4,
            ContainerFormat::QuickTime,
            &mp4_info(),
        )
        .unwrap();

        assert!(plan.is_packet_copy_only());
        assert!(!plan.requires_transcode());
        assert!(!plan.has_unsupported_streams());
    }

    #[test]
    fn remux_plan_marks_transcode_for_webm_target_codecs() {
        let plan =
            plan_container_remux(ContainerFormat::Mp4, ContainerFormat::WebM, &mp4_info()).unwrap();

        assert!(!plan.is_packet_copy_only());
        assert!(plan.requires_transcode());
        assert!(!plan.has_unsupported_streams());
        assert!(matches!(
            plan.stream(1).unwrap().action,
            RemuxAction::TranscodeRequired { .. }
        ));
        assert!(matches!(
            plan.stream(2).unwrap().action,
            RemuxAction::TranscodeRequired { .. }
        ));
    }

    #[test]
    fn remux_plan_marks_streams_target_container_cannot_carry() {
        let plan =
            plan_container_remux(ContainerFormat::Mp4, ContainerFormat::Wav, &mp4_info()).unwrap();

        assert!(plan.requires_transcode());
        assert!(plan.has_unsupported_streams());
        assert!(matches!(
            plan.stream(1).unwrap().action,
            RemuxAction::Unsupported { .. }
        ));
        assert!(matches!(
            plan.stream(2).unwrap().action,
            RemuxAction::TranscodeRequired { .. }
        ));
    }
}
