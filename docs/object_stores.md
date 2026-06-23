# Object Stores

`video-utils-rs` treats media inputs and outputs as objects. The reusable media
APIs take `object_store::ObjectStore` plus `object_store::path::Path`, not native
filesystem paths.

## What Works Today

- Read an object into `Bytes` with `read_object_bytes`.
- Read object byte ranges with `read_object_range`.
- Read bounded byte chunks with `read_object_chunks`.
- Write `Bytes` with `write_object_bytes`.
- Copy within one store with `copy_object_same_store`.
- Copy between stores with `copy_object_between_stores`.
- Detect containers from object key extensions or small header probes with
  `detect_object_container_format`.
- Probe MP4/MOV/fMP4, Matroska/WebM, MPEG-TS, FLV, Ogg, WAV, AIFF, and raw elementary objects with
  `probe_object_media_info`.
- Demux MP4/MOV/fMP4, Matroska/WebM, MPEG-TS, FLV, Ogg, WAV, AIFF, and raw elementary objects into `DemuxedMedia` with
  `demux_object`.
- Mux packet-copy-compatible streams into MP4/MOV, Matroska/WebM, MPEG-TS, FLV, Ogg, WAV, AIFF, or raw elementary
  objects with `mux_object`.
- Plan object remux compatibility with `plan_object_remux`.
- Plan object remux compatibility from the probed source object with
  `plan_object_remux_from_probe`.
- Same-container object remux helpers copy bytes exactly.
- Cross-container object remux can write MP4/MOV, Matroska/WebM, MPEG-TS, FLV, Ogg, WAV, AIFF, or raw elementary
  output when the probed plan is packet-copy-only and the current Rust-native
  muxer supports the streams.
- Run packet-copy-first transcode/remux jobs with `transcode_object_same_store`
  or `transcode_object_between_stores`. `ObjectTranscodeJob` chooses exact copy
  or packet-copy remux when no decode stage is needed, and runs a configured
  decoded-frame video stage when matching `VideoDecoder` and `VideoEncoder`
  backends are supplied.
- Transform decoded video frames with `transform_object_video_same_store` or
  `transform_object_video_between_stores` when a matching video decoder and
  encoder are supplied. The built-in raw RGBA backend supports deterministic
  raw-video transform jobs. With `codec-av1-rust`, raw decoded frames can be
  encoded into AV1 WebM through `Rav1eAv1Encoder`. With `codec-h264-rust` or
  `codec-h265-rust`, compressed H.264/HEVC input can be decoded before the
  transform; output still requires a matching encoder for the target codec.
- Transform PCM audio packets with `transform_object_audio_same_store` or
  `transform_object_audio_between_stores`. The current object audio transform
  path decodes PCM packets to `AudioFrame`, runs an `AudioTransformPipeline`,
  and writes WAV PCM output.
- With `audio-io`, transform audio file objects with
  `transform_object_audio_file_to_wav_same_store` or
  `transform_object_audio_file_to_wav_between_stores`. This path decodes with
  Symphonia, applies the same audio pipeline, and writes WAV PCM output.
- Package HLS VOD outputs with `write_hls_vod_same_store` when packet data is
  already available, or `package_object_hls_vod_same_store` when the source
  object should be demuxed first. Segment and playlist bytes are written only
  through `ObjectStore`. MPEG-TS segment output supports common packet-copy
  audio/video streams. fMP4 output writes `init.mp4` plus `.m4s` media segments
  for configured H.264/H.265 video tracks, AAC audio tracks, and WebVTT subtitle
  tracks when MP4 codec config is present where required.
- Add/extract Matroska subtitle tracks from sidecars with
  `add_subtitle_sidecar_to_object_same_store` and
  `extract_subtitle_track_to_sidecar_same_store`.
- Burn subtitle sidecars into object-store video outputs with
  `burn_subtitle_sidecar_into_object_same_store` when a matching video decoder
  and encoder are supplied.

## Container Planning

`ContainerFormat` models MP4, MOV, WebM, Matroska, MPEG-TS/PS, AVI, FLV, Ogg,
WAV, AIFF, and raw elementary streams. `plan_container_remux` and
`plan_object_remux` return a `RemuxPlan` with one action per stream:

- `PacketCopy`: the encoded stream can be carried by the target container.
- `TranscodeRequired`: the target can carry the media type, but not that codec.
- `Unsupported`: the target container cannot preserve the stream type.

This distinction matters. MP4 with H.264/AAC can be packet-copied into MOV, but
MP4 to WebM usually requires transcoding to VP8/VP9/AV1 plus Opus/Vorbis.

## Guarded Remux

The remux helpers do not fake conversion by copying bytes across different
containers. These succeed:

- `input.mp4` to `copy.mp4`
- `input.webm` to `copy.webm`
- `input.mov` to `output.mp4` when the MOV object is ISO-BMFF-compatible and
  contains one supported video stream plus optional supported audio
- `input.mp4` to `output.mov` when the streams fit the ISO-BMFF muxer
- `input.webm` to `output.mkv` for packet-copy-compatible WebM streams
- `input.mkv` to `output.webm` when all streams use WebM-compatible codecs
- `input.ogg` to `output.ogg` as an exact copy, or through muxing when the
  stream is supported Opus, Vorbis, or FLAC
- `input.wav` to `output.wav` as an exact copy, or through muxing when the
  stream is PCM
- `input.aiff` to `output.wav`, or `input.wav` to `output.aiff`, when the stream
  is uncompressed integer PCM
- `input.flv` to `output.ts` or `output.mp4` when the H.264/AAC streams fit the
  target muxer
- `input.h264` to `output.ts` for raw H.264 elementary packet-copy remuxing
- `input.ts` to `output.h264` when the demuxed stream set contains exactly one
  stream compatible with raw elementary output

These return `Error::Unsupported` until a real mux adapter and any remaining
bitstream filters are wired:

- H.264/AAC `input.mp4` to `output.webm`
- `input.webm` to `output.mov`
- multi-track sources to raw elementary targets, because raw elementary output
  carries one stream

Use `plan_object_remux_from_probe` before dispatching to mux/remux when the
source media metadata should be read directly from the object.

## Packet-Copy-First Transcode Jobs

`ObjectTranscodeJob` is the higher-level dispatch surface for applications that
want one object-store call instead of separate plan/remux/transform decisions.
With the default job, `transcode_object_same_store` and
`transcode_object_between_stores` use the same guarded behavior as the remux
helpers: same-container targets are exact byte copies, and cross-container
targets are demuxed and remuxed only when the probed plan is packet-copy-only.

When a video stage is attached with `ObjectTranscodeJob::with_video`, the job
demuxes the source, decodes the selected video track through the supplied
`VideoDecoder`, applies the `FrameTransformPipeline`, encodes through the
supplied `VideoEncoder`, and muxes the output object. Non-video packets are
preserved only when `ObjectVideoTransformJob::preserve_non_video` is true and
the target container can packet-copy those streams. Audio, subtitle, and extra
video streams that still need transcoding return explicit errors until matching
backend stages are wired.

## Example

```rust
use object_store::{ObjectStore, PutPayload, memory::InMemory, path::Path};
use video_utils_rs::{
    CodecId, ContainerFormat, MediaInfo, MediaType, StreamInfo, TimeBase,
    plan_object_remux, plan_object_remux_from_probe, remux_object_same_store,
};

# futures::executor::block_on(async {
let store = InMemory::new();
let source = Path::from("clips/input.mp4");
let target = Path::from("clips/output.webm");
store.put(&source, PutPayload::from_static(b"media bytes")).await.unwrap();

let mut info = MediaInfo::default();
info.push_stream(StreamInfo::new(
    1,
    MediaType::Video,
    CodecId::H264,
    TimeBase::new(1, 90_000)?,
));

let plan = plan_object_remux(&store, &source, &target, &info).await?;
assert_eq!(plan.source, ContainerFormat::Mp4);
assert!(plan.requires_transcode());

// For real MP4/MOV or Matroska/WebM object bytes, use:
// let plan = plan_object_remux_from_probe(&store, &source, &target).await?;

let err = remux_object_same_store(&store, &source, &target, Some(&info))
    .await
    .unwrap_err();
assert!(matches!(err, video_utils_rs::Error::Unsupported { .. }));
# Ok::<(), video_utils_rs::Error>(())
# }).unwrap();
```
