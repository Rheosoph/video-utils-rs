# video-utils-rs

Portable Rust media utilities for object-store-first video, audio, subtitle,
container, streaming, and transform workflows.

The crate is intentionally packet-copy-first. It can inspect, demux, mux, copy,
remux, segment, and transform media without pretending that container rewrites
are the same thing as codec transcoding. Decode/encode backends are explicit
feature lanes; there is no FFmpeg/libav dependency.

## What It Does

- Reads and writes media through `object_store::ObjectStore`, not native file
  paths.
- Models encoded packets, stream metadata, time bases, and container remux
  plans.
- Demuxes and muxes MP4/MOV/fMP4, Matroska/WebM, MPEG-TS, FLV, Ogg, WAV, AIFF,
  and raw elementary streams where adapters are wired.
- Parses, writes, shifts, packetizes, extracts, and burns in SRT/WebVTT
  subtitles.
- Applies decoded RGBA frame transforms: crop, pad, resize, flip, rotate, blur,
  color filters, overlays, watermarks, and black-bar detection.
- Applies decoded PCM audio transforms: gain, dB gain, peak normalization,
  fades, mixing, waveform buckets, and silence detection.
- Builds HLS VOD playlists and object-store segment outputs for TS and fMP4
  workflows.
- Provides a high-level `ObjectTranscodeJob` that chooses exact object copy,
  packet-copy remux, or a configured decoded-frame video transform/encode path.

## Install

```toml
[dependencies]
video-utils-rs = "0.1"
```

Default features enable the portable core plus a small diagnostics CLI:

```toml
default = ["cli", "portable-core", "platform-codecs"]
portable-core = [
  "packet-ops",
  "containers",
  "audio-core",
  "frame-core",
  "subtitles",
  "streaming",
]
```

Optional feature lanes:

| Feature | Adds |
| --- | --- |
| `audio-io` | WAV PCM read/write plus Symphonia audio decode |
| `image-io` | PNG/JPEG/GIF/WebP/AVIF still-image decode/write |
| `preview` | Preview-oriented image dependencies |
| `svg` | SVG rasterization dependency lane |
| `mp4e-muxer` | Alternate MP4 muxer dependency lane |
| `platform-codecs` | Default runtime-probed platform backend descriptors and target-native probe/open adapters for patent-sensitive codecs |
| `codec-h264-rust` | Rust-native H.264 decode backend |
| `codec-h265-rust` | Rust-native H.265/HEVC decode backend |
| `codec-av1-rust` | Rust-native AV1 encode backend through `rav1e` |
| `codec-apple`, `codec-android`, `codec-windows`, `codec-gstreamer`, `codec-web`, `codec-openh264-ffi` | Explicit backend lanes for platform/system or external native codecs |

## Quick Examples

Packet-copy timeline operations:

```rust
use video_utils_rs::{CodecId, EncodedPacket, TimeBase, concat_copy};

let tb = TimeBase::milliseconds();
let first = vec![
    EncodedPacket::new(1, CodecId::H264, 0, 1_000, tb, vec![0]).with_keyframe(true),
];
let second = vec![
    EncodedPacket::new(1, CodecId::H264, 0, 1_000, tb, vec![1]).with_keyframe(true),
];

let merged = concat_copy(&[&first, &second])?;
assert_eq!(merged[1].pts, 1_000);
# Ok::<(), video_utils_rs::Error>(())
```

Decoded-frame transforms:

```rust
use video_utils_rs::{
    ColorFilter, FrameTransform, FrameTransformPipeline, RgbaFrame,
};

let frame = RgbaFrame::solid(320, 180, [16, 24, 32, 255]);
let pipeline = FrameTransformPipeline::new()
    .with(FrameTransform::BoxBlur { radius: 1 })
    .with(FrameTransform::ColorFilter(ColorFilter::sepia()));

let output = pipeline.apply(&frame)?;
assert_eq!(output.width, 320);
# Ok::<(), video_utils_rs::Error>(())
```

Object-store remux planning:

```rust
use object_store::{ObjectStore, PutPayload, memory::InMemory, path::Path};
use video_utils_rs::{
    CodecId, ContainerFormat, MediaInfo, MediaType, StreamInfo, TimeBase,
    plan_object_remux,
};

# futures::executor::block_on(async {
let store = InMemory::new();
let source = Path::from("incoming/source.mp4");
let target = Path::from("out/video.webm");
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
# Ok::<(), video_utils_rs::Error>(())
# }).unwrap();
```

Packet-copy-first object transcode/remux dispatch:

```rust
use video_utils_rs::{
    FrameTransformPipeline, ObjectTranscodeJob, ObjectVideoTransformJob,
};

let job = ObjectTranscodeJob::new()
    .with_video(ObjectVideoTransformJob::new(FrameTransformPipeline::new()));

// transcode_object_same_store(
//     &store,
//     &source,
//     &target,
//     &job,
//     Some(&mut decoder),
//     Some(&mut encoder),
// ).await?;
```

## Current Support

| Area | Current implementation |
| --- | --- |
| Containers | MP4/MOV/fMP4, Matroska/WebM, MPEG-TS, FLV, Ogg, WAV, AIFF, raw elementary |
| Packet copy | H.264, H.265, AV1, VP8/VP9, MPEG video, ProRes, Theora, Dirac, common audio codecs, SRT/WebVTT |
| Platform codec adapters | Default runtime-probed OS/system lanes plus target-native probe/open handles for DTS, WMA, ProRes, AAC, E-AC-3, H.264, and H.265/HEVC |
| Video frame backends | Raw RGBA decode/encode; optional H.264 decode; optional H.265 decode; optional AV1 encode |
| Audio backends | PCM packet/frame helpers; optional WAV IO; optional Symphonia file and packet decode |
| Image backends | Optional PNG, JPEG, GIF, WebP, AVIF still-image IO |
| Subtitles | SRT/WebVTT sidecars, Matroska/fMP4 packet tracks, burn-in on decoded RGBA frames |
| Streaming | HLS playlists, TS segments, fMP4 init/media segments |
| Object IO | Read, write, range/chunk reads, copy, probe, demux, mux, remux, transcode orchestration |

For the full inventory, see [docs/feature_matrix.md](docs/feature_matrix.md).

## Important Limits

- No FFmpeg/libav backend is included.
- Platform codec adapters probe and open native VideoToolbox/AudioToolbox,
  AMediaCodec, WebCodecs, Media Foundation, or GStreamer handles where
  available. Packet/frame/sample marshaling for actual decoded-frame
  decode/encode still returns explicit backend errors.
- Bundled concrete H.264/H.265 encode is not implemented; platform adapters
  may report runtime-probed system encode lanes.
- AV1 decode is not implemented.
- VP8/VP9 decoded-frame backends are not wired.
- Bundled compressed audio packet encoders are not wired; platform lanes may
  expose runtime-probed system encoders.
- AVI and MPEG-PS are modeled for detection/planning but do not yet have object
  demux/mux adapters.
- Remux helpers never byte-copy across different container extensions.
  Cross-container output requires a packet-copy-compatible plan or a configured
  transcode stage.

## CLI

```sh
cargo run -- capabilities
cargo run -- srt-shift subtitles.srt --offset-ms 1500
cat subtitles.vtt | cargo run -- srt-shift --webvtt --offset-ms=-500
```

The CLI is intentionally small. The library API is the main surface.

## Development

```sh
cargo fmt
python3 scripts/check_licenses.py
cargo test --all-features
cargo clippy --all-targets --all-features -- -D warnings
cargo test --no-default-features
cargo clippy --no-default-features --all-targets -- -D warnings
```

Ignored fixture tests use a local FFmpeg binary only to generate external test
media:

```sh
cargo test --test ffmpeg_smoke -- --ignored
cargo test --test ffmpeg_real_data -- --ignored
```

## Documentation

- [Architecture](docs/architecture.md)
- [Codecs](docs/codecs.md)
- [Feature matrix](docs/feature_matrix.md)
- [Platform codec backends](docs/platform_codecs.md)
- [Object stores](docs/object_stores.md)
- [Subtitles](docs/subtitles.md)
- [Testing](docs/testing.md)
- [End-to-end test plan](docs/e2e_test_plan.md)
- [Dependency license audit](docs/dependency_licenses.md)

## License

Recommended crate license: `MIT OR Apache-2.0`.

This repository is dual-licensed under either the Apache License, Version 2.0
([LICENSE-APACHE](LICENSE-APACHE)) or the MIT license
([LICENSE-MIT](LICENSE-MIT)), at your option. Dependency license notes are in
[docs/dependency_licenses.md](docs/dependency_licenses.md).
