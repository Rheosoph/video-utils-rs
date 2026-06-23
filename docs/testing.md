# Testing

The test suite is split into deterministic default tests and explicit
environment-dependent fixture tests.

## Default Tests

Run:

```sh
cargo test --offline
```

Coverage:

- Unit tests inside each module.
- Frame operation and transform pipeline unit tests inside `src/frame.rs`.
- Public API integration workflows in `tests/public_api.rs`.
- Object-store container planning and copy/remux guards in
  `tests/object_store_containers.rs`.
- In-memory MP4 object-store probe/demux coverage in `tests/container_demux.rs`.
- In-memory MP4/MOV packet mux and `.mov` source to `.mp4` object-store remux
  coverage in `tests/container_mux.rs`.
- In-memory WebM/Matroska packet mux, demux round trips, and object-store
  `.webm`/`.mkv` remux coverage in `tests/matroska_mux.rs`.
- In-memory WAV and Ogg packet mux/demux plus object-store routing coverage in
  `tests/audio_containers.rs`.
- PCM WAV object-store audio transform coverage in `tests/audio_transform.rs`.
- Raw RGBA object-store video transform coverage in `tests/video_transform.rs`.
- Compressed-video backend boundary tests in `tests/compressed_video_backends.rs`;
  with `codec-av1-rust`, these encode real AV1 packets with `rav1e`, mux them
  into WebM, and exercise a raw-RGBA object to AV1 WebM transform.
- Pure-Rust compressed decoder smoke tests in `tests/pure_rust_video_decoders.rs`;
  these are ignored by default and generate real H.264/HEVC Annex-B streams with
  ffmpeg before decoding through `RustH264Decoder`/`RustH265Decoder`.
- CLI behavior in `tests/cli.rs`.
- Platform codec descriptor policy and target-native probe/open coverage in
  `tests/platform_codecs.rs`.
- Ignored ffmpeg fixture tests are discovered but not run.

The default suite does not require network access, external video files, ffmpeg,
native media files, or a GUI. Object-store tests use `object_store::memory::InMemory`.
Default features include `platform-codecs`. The default suite verifies backend
selection and, when the current host reports support, opens a matching native
platform handle. On hosted CI that means macOS can instantiate VideoToolbox and
AudioToolbox, Linux can instantiate a GStreamer element when plugins are
present, Windows can activate a Media Foundation transform, Android can create
an AMediaCodec, and WASM/browser tests can check WebCodecs constructor
availability. macOS additionally runs runtime-probed VideoToolbox H.264
encode/decode and AudioToolbox AAC encode/decode smoke tests when those host
codecs are available.

For the workflow-level backlog of complete media scenarios, see
[End-to-End Test Plan](e2e_test_plan.md).

## CI Test Matrix

GitHub Actions runs the platform-neutral test suite on Ubuntu, macOS, and
Windows with platform codec features disabled:

```sh
cargo test --no-default-features --features cli,portable-core
cargo test --no-default-features --features cli,portable-core,mp4e-muxer,audio-io,image-io,preview,svg,codec-h264-rust,codec-h265-rust,codec-av1-rust,codec-openh264-ffi
cargo test --no-default-features
```

Platform adapter coverage is intentionally separate from the shared matrix:

```sh
cargo test --no-default-features --features platform-codecs --test platform_codecs
cargo check --no-default-features --features platform-codecs
cargo check --target aarch64-linux-android --no-default-features --features platform-codecs
```

Linux platform adapter jobs install GStreamer runtime packages. Android is a
check-only target because hosted GitHub runners do not provide an Android media
codec runtime for native tests.

## All Features

Run:

```sh
cargo test --offline --all-features
cargo clippy --offline --all-targets --all-features -- -D warnings
```

This verifies the optional dependency graph and the same public API surface with
all feature flags enabled.

This is useful for local exhaustive checks, but CI keeps platform adapters in
separate jobs so shared test failures are not coupled to host media runtime
availability.

## ffmpeg Fixture Tests

Run:

```sh
cargo test --test ffmpeg_smoke -- --ignored
cargo test --test ffmpeg_real_data -- --ignored
cargo test --test ffmpeg_e2e -- --ignored
```

The ignored smoke test:

1. Creates a temporary SRT file.
2. Uses `ffmpeg` to generate a synthetic MP4 test pattern.
3. Muxes the SRT as a soft `mov_text` subtitle stream.
4. Uses `ffprobe` to verify the subtitle stream exists.

The ignored real-data tests generate H.264 MP4 and VP9/Opus WebM fixtures with
ffmpeg, load those bytes into `object_store::memory::InMemory`, and exercise
crate demux/mux/remux APIs from object-store data.

The ignored E2E tests generate MOV, MP4, Matroska, MPEG-TS, and FLV H.264/AAC
source objects with ffmpeg, put them into `object_store::memory::InMemory`, and
verify cross-container packet-copy remuxing through the library. They also cover
metadata recovery needed for TS/FLV to MP4 output.

They are intentionally ignored because ffmpeg availability and codec support differ
between machines. It gives contributors a useful local fixture path without
making CI or normal development brittle.

## Expected Warnings

In this environment, `sccache` is blocked by the sandbox and Cargo falls back to
direct `rustc`. Builds still pass.

Cargo also reports a future-incompatibility warning from `nom v2.1.0`, pulled in
through the current `m3u8-rs` dependency graph. That warning is upstream, not
from local crate code.
