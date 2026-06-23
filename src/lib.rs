//! Portable media utility primitives.
//!
//! The crate is organized around three lanes:
//! encoded packet/container operations, decoded audio frames, and decoded video
//! frames. Codec backends are modeled explicitly so applications can keep the
//! default build free of FFmpeg/libav-family dependencies.

#![deny(unsafe_op_in_unsafe_fn)]

pub mod audio;
pub mod audio_transform;
pub mod backend;
pub mod bitstream;
pub mod codec;
pub mod codecs;
pub mod container;
#[cfg(feature = "containers")]
pub mod containers;
pub mod error;
pub mod frame;
pub mod media;
pub mod object_store_io;
pub mod packet;
#[cfg(any(
    feature = "platform-codecs",
    feature = "codec-apple",
    feature = "codec-android",
    feature = "codec-windows",
    feature = "codec-gstreamer",
    feature = "codec-web"
))]
pub mod platform;
pub mod streaming;
pub mod subtitle;
#[cfg(feature = "containers")]
pub mod subtitle_object;
pub mod time;
pub mod transcode;
pub mod transform;

pub use audio::{
    AudioFrame, FadeShape, SilenceRange, WaveformBucket, apply_gain, apply_gain_db, detect_silence,
    fade, mix, normalize_peak, waveform_peaks,
};
pub use audio_transform::{
    AudioTransform, AudioTransformPipeline, ObjectAudioTransformJob, ObjectAudioTransformReport,
};
#[cfg(feature = "containers")]
pub use audio_transform::{
    transform_object_audio_between_stores, transform_object_audio_same_store,
};
#[cfg(all(feature = "containers", feature = "audio-io"))]
pub use audio_transform::{
    transform_object_audio_file_to_wav_between_stores,
    transform_object_audio_file_to_wav_same_store,
};
pub use backend::{
    BackendCapability, BackendKind, BackendProbe, BackendSource, CodecBackendDescriptor,
    FeatureSet, PlatformCodecApiNames, TargetFamily, compiled_features, platform_codec_api_names,
    platform_delegated_codec_ids, preferred_backend_for_codec,
    recommended_backends_for_current_target,
};
pub use codec::{
    AudioDecoder, AudioEncoder, CodecConfig, CodecDescriptor, CodecDirection, CodecId,
    CodecImplementationKind, CodecRegistry, CodecSupport, Decoder, Encoder, MediaType,
    VideoDecoder, VideoEncoder, builtin_codec_support, known_audio_codecs, known_image_codecs,
    known_video_codecs, packet_copy_codec_ids, symphonia_audio_decode_codec_ids,
};
#[cfg(feature = "codec-h264-rust")]
pub use codecs::RustH264Decoder;
#[cfg(feature = "codec-h265-rust")]
pub use codecs::RustH265Decoder;
#[cfg(feature = "image-io")]
pub use codecs::{ImageRgbaDecoder, ImageRgbaEncoder, ImageStillFormat};
pub use codecs::{
    PacketCopyCodec, RawRgbaVideoDecoder, RawRgbaVideoEncoder, SubtitleTextCodec,
    UnsupportedAudioDecoder, UnsupportedAudioEncoder, UnsupportedVideoDecoder,
    UnsupportedVideoEncoder,
};
#[cfg(feature = "codec-av1-rust")]
pub use codecs::{Rav1eAv1Encoder, Rav1eAv1EncoderOptions};
#[cfg(feature = "audio-io")]
pub use codecs::{
    SymphoniaAudioDecoder, SymphoniaPacketAudioDecoder, WavPcmDecoder, WavPcmEncoder,
    WavPcmSampleFormat,
};
pub use container::{
    ContainerAdapter, ContainerDemuxer, ContainerFormat, ContainerMuxer, ContainerPolicy,
    DemuxedMedia, MuxedMedia, RemuxAction, RemuxPlan, RemuxStreamPlan, plan_container_remux,
};
#[cfg(feature = "containers")]
pub use containers::{
    AiffDemuxer, AiffMuxer, ElementaryDemuxer, ElementaryMuxer, FlvDemuxer, FlvMuxer,
    FragmentedMp4Demuxer, FragmentedMp4Output, IsoBmffDemuxer, IsoBmffMuxer, MatroskaDemuxer,
    MatroskaMuxer, MpegTsDemuxer, MpegTsMuxer, OggDemuxer, OggMuxer, PcmEncoding, PcmSampleFormat,
    WavDemuxer, WavMuxer, decode_pcm_packet, demux_aiff_bytes, demux_elementary_bytes,
    demux_elementary_bytes_from_path, demux_flv_bytes, demux_fragmented_mp4_bytes,
    demux_fragmented_mp4_segments, demux_iso_bmff_bytes, demux_matroska_bytes, demux_mpeg_ts_bytes,
    demux_ogg_bytes, demux_wav_bytes, detect_elementary_codec_from_extension,
    detect_elementary_codec_from_path, encode_pcm_packet, mux_aiff_bytes, mux_elementary_bytes,
    mux_flv_bytes, mux_fragmented_mp4_segments, mux_iso_bmff_bytes, mux_matroska_bytes,
    mux_mpeg_ts_bytes, mux_ogg_bytes, mux_wav_bytes, pcm_encoding_from_stream, probe_aiff_bytes,
    probe_flv_bytes, probe_fragmented_mp4_bytes, probe_iso_bmff_bytes, probe_matroska_bytes,
    probe_ogg_bytes, probe_wav_bytes, set_pcm_tags,
};
pub use error::{Error, Result};
pub use frame::{
    BlackBars, ColorFilter, CropRect, FrameTransform, FrameTransformPipeline, RgbaFrame, Watermark,
    WatermarkAnchor,
};
pub use media::{MediaInfo, StreamInfo};
pub use object_store_io::{
    ObjectChunkReadOptions, ObjectReadChunk, ObjectRemuxOperation, ObjectRemuxReport,
    ObjectTransferReport, copy_object_between_stores, copy_object_same_store,
    detect_object_container_format, plan_object_remux, read_object_bytes, read_object_chunks,
    read_object_range, remux_object_between_stores, remux_object_same_store, write_object_bytes,
};
#[cfg(feature = "containers")]
pub use object_store_io::{
    ObjectMuxReport, demux_object, mux_object, plan_object_remux_from_probe,
    probe_object_media_info,
};
pub use packet::{
    EncodedPacket, PacketSlice, concat_copy, filter_track, normalize_timestamps,
    select_keyframe_range, validate_concat_compatible, validate_monotonic_by_track,
};
#[cfg(all(
    any(feature = "platform-codecs", feature = "codec-web"),
    target_family = "wasm"
))]
pub use platform::{
    AsyncWebCodecsAudioDecoder, AsyncWebCodecsAudioEncoder, AsyncWebCodecsVideoDecoder,
    AsyncWebCodecsVideoEncoder,
};
#[cfg(any(
    feature = "platform-codecs",
    feature = "codec-apple",
    feature = "codec-android",
    feature = "codec-windows",
    feature = "codec-gstreamer",
    feature = "codec-web"
))]
pub use platform::{
    PlatformAudioDecoder, PlatformAudioDecoderConfig, PlatformAudioEncoder,
    PlatformAudioEncoderConfig, PlatformCodecProbe, PlatformVideoDecoder,
    PlatformVideoDecoderConfig, PlatformVideoEncoder, PlatformVideoEncoderConfig,
    probe_platform_codec,
};
pub use streaming::{HlsPlaylist, HlsSegment, plan_keyframe_segments};
#[cfg(feature = "containers")]
pub use streaming::{
    HlsSegmentContainer, ObjectHlsVodJob, ObjectHlsVodReport,
    package_object_hls_vod_between_stores, package_object_hls_vod_same_store,
    write_hls_vod_between_stores, write_hls_vod_same_store,
};
pub use subtitle::{
    SubtitleEvent, SubtitleFormat, SubtitleRenderResult, SubtitleStyle, active_events_at,
    burn_subtitles_onto_frame, parse_subtitles, render_subtitle_overlay, shift_events,
    subtitle_codec_for_format, subtitle_events_to_packets, subtitle_format_for_codec,
    subtitle_packets_to_events, write_srt, write_webvtt,
};
#[cfg(feature = "containers")]
pub use subtitle_object::{
    ObjectSubtitleBurnInJob, ObjectSubtitleBurnInObjects, ObjectSubtitleBurnInReport,
    ObjectSubtitleExtractReport, ObjectSubtitleTrackJob, ObjectSubtitleTrackReport,
    add_subtitle_sidecar_to_object_between_stores, add_subtitle_sidecar_to_object_same_store,
    burn_subtitle_sidecar_into_object_between_stores, burn_subtitle_sidecar_into_object_same_store,
    extract_subtitle_track_to_sidecar_between_stores, extract_subtitle_track_to_sidecar_same_store,
};
pub use time::TimeBase;
pub use transcode::{
    ObjectTranscodeJob, ObjectTranscodeOperation, ObjectTranscodeReport,
    missing_transcode_stage_error,
};
#[cfg(feature = "containers")]
pub use transcode::{transcode_object_between_stores, transcode_object_same_store};
pub use transform::{
    ObjectVideoTransformJob, ObjectVideoTransformReport, missing_video_backend_error,
};
#[cfg(feature = "containers")]
pub use transform::{transform_object_video_between_stores, transform_object_video_same_store};

/// Short crate identity used by the CLI and downstream diagnostics.
#[must_use]
pub fn crate_profile() -> &'static str {
    "video-utils-rs portable media core"
}
