# Platform Integration Audit

Audit date: 2026-06-23

Scope: `src/platform/apple.rs`, `src/platform/android.rs`,
`src/platform/windows.rs`, `src/platform/web.rs`, and
`src/platform/gstreamer.rs`.

## Sources Checked

- Android NDK Media API:
  https://developer.android.com/ndk/reference/group/media
- Android `MediaCodec` API:
  https://developer.android.com/reference/android/media/MediaCodec
- Android official native-codec sample:
  https://github.com/android/ndk-samples/tree/main/native-codec
- Apple local Xcode SDK headers:
  `CoreMedia/CMBlockBuffer.h`, `VideoToolbox/VTCompressionSession.h`,
  `VideoToolbox/VTDecompressionSession.h`, and
  `AudioToolbox/AudioConverter.h`.
- Microsoft Media Foundation MFT processing model:
  https://learn.microsoft.com/en-us/windows/win32/medfound/basic-mft-processing-model
- Microsoft Media Foundation `IMFTransform::ProcessInput`:
  https://learn.microsoft.com/en-us/windows/win32/api/mftransform/nf-mftransform-imftransform-processinput
- Microsoft Media Foundation `IMFTransform::ProcessOutput`:
  https://learn.microsoft.com/en-us/windows/win32/api/mftransform/nf-mftransform-imftransform-processoutput
- Microsoft official Media Foundation samples:
  https://github.com/microsoft/Windows-classic-samples/tree/main/Samples/Win7Samples/multimedia/mediafoundation
- W3C WebCodecs specification:
  https://www.w3.org/TR/webcodecs/
- W3C WebCodecs samples:
  https://github.com/w3c/webcodecs/tree/main/samples
- GStreamer `appsrc` documentation:
  https://gstreamer.freedesktop.org/documentation/app/appsrc.html
- GStreamer `appsink` documentation:
  https://gstreamer.freedesktop.org/documentation/app/appsink.html
- GStreamer buffer documentation:
  https://gstreamer.freedesktop.org/documentation/gstreamer/gstbuffer.html
- GStreamer Rust examples:
  https://github.com/GStreamer/gstreamer-rs/tree/main/examples/src/bin

## Fixed During Audit

- Android output buffers now copy the documented payload size from
  `AMediaCodecBufferInfo.size` instead of trusting
  `AMediaCodec_getOutputBuffer`'s `out_size` on all API levels.
- Android H.264/H.265 decoder configuration now converts MP4-style
  `avcC`/`hvcC` extradata into documented `csd-*` buffers with Annex B
  start codes.
- GStreamer `appsrc` buffer pushes now release the caller's original
  `GstBuffer` reference after `appsrc` takes its queued reference.
- GStreamer input buffers now set explicit `GstBuffer` PTS/duration and pulled
  samples feed their PTS/duration back into packet/audio-frame timestamps.
- Windows Media Foundation now drains and preserves output when
  `ProcessInput` returns `MF_E_NOTACCEPTING`, then retries the same input
  sample.
- Windows Media Foundation output samples are now allocated from
  `GetOutputStreamInfo`, transform-provided samples are supported, and
  incomplete output plus stream/format changes are handled explicitly.
- WebCodecs probing now reports constructor availability only, because exact
  codec support requires async `isConfigSupported` or `configure` on the
  current browser runtime.

## Remaining Validation

- WebCodecs exact codec strings and browser support for H.265 and ProRes still
  need runtime `isConfigSupported` coverage in browser tests.
- Windows Media Foundation needs runtime E2E coverage on a Windows runner with
  real MFTs.
- GStreamer needs runtime E2E coverage on a Linux runner with codec plugins
  installed.

## Verification

Passed:

- `cargo fmt --all`
- `cargo check --all-targets --all-features`
- `cargo test --all-features`
- `cargo check --target aarch64-linux-android --features platform-codecs`
- `cargo check --target wasm32-unknown-unknown --features platform-codecs`

Blocked by local toolchain setup:

- `cargo check --target x86_64-pc-windows-msvc --features platform-codecs`
  fails before crate code because the Windows C toolchain/MSVC headers are not
  available for `ring`.
- `cargo check --target aarch64-unknown-linux-gnu --no-default-features --features codec-gstreamer`
  fails before crate code because `aarch64-linux-gnu-gcc` is not installed.
