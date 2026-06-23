# Architecture

`video-utils-rs` keeps media workflows split by the kind of data they operate
on. This is the main design constraint.

## Packet Lane

The packet lane handles already-encoded samples. Operations in this lane should
not decode video or audio frames.

Current public primitives:

- `EncodedPacket`
- `TimeBase`
- `ContainerFormat`
- `ContainerPolicy`
- `DemuxedMedia`
- `ContainerDemuxer`
- `MuxedMedia`
- `ContainerMuxer`
- `RemuxPlan`
- `plan_container_remux`
- `normalize_timestamps`
- `validate_monotonic_by_track`
- `select_keyframe_range`
- `validate_concat_compatible`
- `concat_copy`
- `filter_track`

Expected use cases:

- Probe and inspect container metadata.
- Demux supported containers into encoded packets without decoding frames.
- Mux supported packet-copy streams into container bytes without decoding frames.
- Rebase or repair packet timestamps conservatively.
- Keyframe-aligned trim and split planning.
- Strict packet-copy concatenation.
- Plan whether streams can be packet-copied between containers.
- Segment planning and object-store HLS output.

The feature names `containers`, `packet-ops`, and `mp4e-muxer` compile the
parser/muxer dependencies chosen in the implementation plan. The public packet
model is deliberately stable enough that concrete parser adapters can be added
without changing downstream application code.

`containers` is part of the default portable core. MP4/MOV demux is backed by
`re_mp4`; Matroska/WebM demux is backed by `matroska-demuxer`. MP4/MOV mux
output is backed by `muxide`, with crate-local bitstream filters for MP4
AVC/HEVC samples and raw AAC access units. Matroska/WebM mux output is backed by
a crate-local EBML writer for no-lacing packet-copy tracks. WAV PCM uses a
crate-local RIFF/WAVE parser/writer, and Ogg Opus/Vorbis/FLAC uses a crate-local
page parser/writer. These adapters produce and consume `MediaInfo` and
`EncodedPacket` values from object bytes. MPEG-TS uses a crate-local PAT/PMT/PES
packet-copy writer/parser for common TS audio/video streams. Raw elementary
streams are extension-guided one-stream adapters for packet-copy workflows.

Container planning is intentionally separate from object IO. `ContainerFormat`
models common targets such as MP4, MOV, WebM, Matroska, MPEG-TS/PS, AVI, FLV,
Ogg, WAV, AIFF, and elementary streams. `plan_container_remux` reports
per-stream actions: packet copy, transcode required, or unsupported. It does not
pretend that packet-copy compatibility alone can rewrite container bytes.

## Object Store IO

Media object IO goes through `object_store::ObjectStore`. New media helpers do
not accept native filesystem paths.

Current public primitives:

- `read_object_bytes`
- `read_object_range`
- `read_object_chunks`
- `write_object_bytes`
- `detect_object_container_format`
- `copy_object_same_store`
- `copy_object_between_stores`
- `probe_object_media_info`
- `demux_object`
- `mux_object`
- `plan_object_remux`
- `plan_object_remux_from_probe`
- `remux_object_same_store`
- `remux_object_between_stores`
- `ObjectTranscodeJob`
- `transcode_object_same_store`
- `transcode_object_between_stores`
- `transform_object_video_same_store`
- `transform_object_video_between_stores`
- `transform_object_audio_same_store`
- `transform_object_audio_between_stores`
- `write_hls_vod_same_store`
- `package_object_hls_vod_same_store`
- `add_subtitle_sidecar_to_object_same_store`
- `extract_subtitle_track_to_sidecar_same_store`
- `burn_subtitle_sidecar_into_object_same_store`

The remux helpers perform exact byte copies when the source and target object
keys resolve to the same container format. Cross-container targets first require
a packet-copy-only plan, then use a concrete mux adapter when one exists. Today
that means MP4/MOV output for one supported video stream plus optional supported
audio, Matroska/WebM output for target-compatible packet-copy streams, MPEG-TS
output for common TS audio/video streams, Ogg output for supported
Opus/Vorbis/FLAC streams, WAV output for PCM streams, and one-stream raw
elementary output.
Other targets, or targets that need transcoding, return an explicit unsupported
error rather than copying bytes under a misleading extension.

`ObjectTranscodeJob` is the orchestration layer above remuxing and decoded video
transforms. It keeps packet-copy behavior as the first choice, then runs a
configured decoded-frame video stage only when the caller supplies matching
decoder and encoder backends. This makes the public workflow one call for
copy/remux/transcode decisions while keeping the backend boundary explicit and
object-store-only.

The range/chunk helpers use object-store byte ranges and are intended as the
base for range-aware demuxers. The generic object-store code still writes final
objects through the portable `put` path unless a future store-specific multipart
writer is added.

## Audio Frame Lane

The audio lane operates on decoded interleaved `f32` PCM.

Current public primitives:

- `AudioFrame`
- `apply_gain`
- `apply_gain_db`
- `normalize_peak`
- `fade`
- `mix`
- `waveform_peaks`
- `detect_silence`
- `AudioTransform`
- `AudioTransformPipeline`
- `ObjectAudioTransformJob`

The core helpers are dependency-light and deterministic. Heavier decode/IO work
lives behind `audio-io`, which enables `WavPcmDecoder`, `WavPcmEncoder`,
`SymphoniaAudioDecoder`, and `SymphoniaPacketAudioDecoder`. The Symphonia file
path is decode-only and currently covers AAC, ADPCM, FLAC, MP1/MP2/MP3, PCM,
and Vorbis in AIFF/ISO MP4/MKV/Ogg/WAV style inputs. The packet path decodes
AAC, FLAC, MP3, Opus, and Vorbis packets when stream metadata and required
codec-private data are supplied.

The object-store audio transform path has two modes. The default `containers`
mode targets PCM packet workflows: it decodes PCM packets into `AudioFrame`,
applies an `AudioTransformPipeline`, and writes WAV PCM output. With `audio-io`,
`transform_object_audio_file_to_wav_*` decodes source object bytes through
Symphonia before applying the same pipeline. Compressed audio packet-to-PCM
streaming transforms now have a decode lane for the supported Symphonia packet
codecs; bundled compressed audio packet encoders are still not wired.

## Video Frame Lane

The frame lane operates on decoded `RgbaFrame` values. It is intentionally
separate from video decoding and encoding.

Current public primitives:

- `RgbaFrame`
- `CropRect`
- `BlackBars`
- `ColorFilter`
- `Watermark`
- `WatermarkAnchor`
- `FrameTransform`
- `FrameTransformPipeline`
- crop, pad, flip, rotate, resize, overlay, box blur, color filter, watermark
- `detect_black_bars`
- subtitle burn-in through the subtitle module

The default resize is nearest-neighbor because it is fully local and predictable.
The transform pipeline applies decoded-frame operations in order and can process
one frame or an iterator of frames. Object-store transform jobs can run that
pipeline between a caller-supplied `VideoDecoder` and `VideoEncoder`. The crate
ships a raw RGBA packet backend for deterministic end-to-end transform tests and
an AV1 encoder backend through Rust-native `rav1e`. Already-compressed AV1,
H.264, H.265, VP8, and VP9 input still needs Rust-native decoder
implementations around this lane before those streams can be filtered.
Still-image decode/write through `ImageRgbaDecoder` and `ImageRgbaEncoder`
belongs behind `image-io`; PNG, JPEG, GIF, WebP, and AVIF are wired today.

## Subtitle Lane

The subtitle lane has two parts:

- Sidecar text operations: parse, shift, write.
- Frame placement: active cue lookup and burn-in onto decoded RGBA frames.

The crate can put subtitles onto frames with `burn_subtitles_onto_frame`.
SRT/WebVTT events can also be converted to subtitle packets for Matroska muxing.
Object-store helpers add sidecar subtitles to Matroska, extract Matroska
subtitle tracks back to sidecars, and burn sidecar subtitles into video outputs
when the caller supplies a matching decoder and encoder.

## Backend Lanes

Decode and encode are explicit backend lanes:

- `platform-codecs`
- `codec-apple`
- `codec-android`
- `codec-windows`
- `codec-gstreamer`
- `codec-web`
- `codec-h264-rust`
- `codec-h265-rust`
- `codec-av1-rust`
- `codec-openh264-ffi`

`platform-codecs` is enabled by default and selects runtime-probed platform
backend descriptors plus target-native probe/open adapters for the active
target. It does not bundle codec implementations. The default platform policy
delegates DTS, WMA, ProRes, AAC, E-AC-3, H.264, and H.265/HEVC to OS/system
APIs where available:

- Apple: VideoToolbox and AudioToolbox/CoreAudio.
- Android: MediaCodec.
- Web/WASM: WebCodecs.
- Windows: Media Foundation.
- Linux: GStreamer system plugins.

The platform lane is reported as `CodecImplementationKind::PlatformBackend`,
separate from the concrete bundled `Backend` kind. Every platform descriptor is
runtime-probed because codec/profile availability varies by OS version, device,
browser, installed codec extension, or GStreamer plugin set.

The platform adapters open real native handles where available:
VideoToolbox/AudioToolbox sessions on Apple, AMediaCodec instances on Android,
WebCodecs objects in browser/WASM environments, Media Foundation transforms on
Windows, and GStreamer elements on Linux. Linux/GStreamer, Android MediaCodec,
Windows Media Foundation, Apple VideoToolbox/AudioToolbox, and the wasm
WebCodecs async adapter now marshal bytes through the host APIs. The
synchronous WebCodecs trait handles still return explicit backend errors because
browser output is Promise/callback driven; wasm callers use the async
`AsyncWebCodecs*` surface instead.

`codec-h264-rust` enables `RustH264Decoder`, `codec-h265-rust` enables
`RustH265Decoder`, and `codec-av1-rust` enables `Rav1eAv1Encoder` for AV1 packet
output from `RgbaFrame`. VP8/VP9 remain packet-copy-only because the currently
available Rust crates are parser-only, wrappers, or documented as
bitstream-parser stubs rather than reconstructed-frame backends. The default
crate still avoids bundled codec or FFI dependencies implicitly.

## Codec Traits

The crate now has trait boundaries for concrete implementations:

- `CodecDescriptor`
- `Decoder`
- `Encoder`
- `VideoDecoder`
- `VideoEncoder`
- `AudioDecoder`
- `AudioEncoder`
- `CodecBackendDescriptor`

Implemented adapters:

- `SubtitleTextCodec` for SRT/WebVTT text sidecars.
- `PacketCopyCodec` for encoded packet passthrough.
- `RawRgbaVideoDecoder` and `RawRgbaVideoEncoder` for tightly-packed raw RGBA
  video packets.
- `RustH264Decoder` behind `codec-h264-rust` for H.264 video frame decode.
- `RustH265Decoder` behind `codec-h265-rust` for H.265/HEVC video frame decode.
- `Rav1eAv1Encoder` behind `codec-av1-rust` for AV1 video packet output from
  decoded RGBA frames.
- `WavPcmDecoder` and `WavPcmEncoder` behind `audio-io`.
- `SymphoniaAudioDecoder` behind `audio-io`.
- `SymphoniaPacketAudioDecoder` behind `audio-io`.
- `ImageRgbaDecoder` and `ImageRgbaEncoder` behind `image-io`.
- `BackendCapability` descriptors for platform, bundled, and FFI backend
  selection.
- `PlatformVideoDecoder`, `PlatformVideoEncoder`, `PlatformAudioDecoder`, and
  `PlatformAudioEncoder` behind `platform-codecs` or the explicit platform
  feature lanes. These currently probe/open target-native handles and fail
  explicitly at packet/frame marshaling time.
- `Unsupported*` backend stubs for explicit missing backend wiring.

The `CodecRegistry` and `builtin_codec_support` report exactly what exists
today. They intentionally distinguish packet-copy read/write, platform backend
delegation, and concrete bundled backend decode/encode.

## Dependency Policy

Default features are intended to remain portable and auditable. Optional
dependencies should be introduced through named features that explain the cost:

- `audio-io` for audio decode/write/resample libraries.
- `image-io` for image codecs, high-quality resizing, and YUV conversion.
- `preview` for preview encoders.
- `svg` for SVG rendering.
- codec features for backend-dependent work.
