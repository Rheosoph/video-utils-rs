# Platform Codec Backends

`platform-codecs` is enabled by default. It does not bundle native codec
implementations. It exposes runtime-probed platform backend descriptors plus
target-native probe/open adapters so Flow-Like can prefer OS/system codec APIs
for patent-sensitive codecs while keeping bundled implementations explicit.

## Policy

Prefer platform/system backends by default for:

- `Dts`
- `Wma`
- `ProRes`
- `Aac`
- `Eac3`
- `H264`
- `H265`

Rust-native or external-library implementations for these codecs should remain
behind explicit opt-in features such as `codec-h264-rust`, `codec-h265-rust`,
or `codec-openh264-ffi`.

`CodecRegistry::builtin()` reports platform delegation as
`CodecImplementationKind::PlatformBackend`. This is intentionally separate from
`CodecImplementationKind::Backend`, which is reserved for concrete bundled
packet-to-frame or frame-to-packet implementations.

## Current Backend Selection

| Target | Default platform backend | Notes |
| --- | --- | --- |
| Apple | `AppleVideoToolbox`, `AppleAudioToolbox` | Opens VideoToolbox sessions for H.264, HEVC, and ProRes and marshals RGBA frames through `VTCompressionSessionEncodeFrame`; opens AudioToolbox converters for AAC and E-AC-3 where available |
| Android | `AndroidMediaCodec` | Opens NDK `AMediaCodec` instances by MIME type on the device and queues input/output buffers for configured video/audio adapters |
| Web/WASM | `WebCodecs` | Checks WebCodecs constructors on the active browser/global object; the synchronous codec traits return explicit errors because WebCodecs output is Promise/callback driven |
| Windows | `WindowsMediaFoundation` | Enumerates and activates Media Foundation Transforms on the host and marshals buffers through `IMFSample`/`IMFMediaBuffer` |
| Linux | `GStreamer` | Dynamically loads GStreamer plus the app library and marshals packets/frames through `appsrc`/`appsink`; exact codec coverage depends on installed plugins |

Every platform backend is marked `BackendProbe::Runtime`. The descriptor means
the API lane is selected for that target; it does not guarantee that a given
codec/profile is available on every device.

## Adapter Surface

The public platform adapter entry points are:

- `probe_platform_codec(codec, direction)` for runtime support checks.
- `PlatformVideoDecoder` and `PlatformVideoEncoder`.
- `PlatformAudioDecoder` and `PlatformAudioEncoder`.
- `PlatformVideoDecoderConfig`, `PlatformVideoEncoderConfig`,
  `PlatformAudioDecoderConfig`, and `PlatformAudioEncoderConfig`.

The current adapters validate media type, select the target backend, and open a
real native handle when the host reports support:

- Apple: `CMVideoFormatDescriptionCreate`, `VTDecompressionSessionCreate`,
  `VTCompressionSessionCreate`, `VTCompressionSessionEncodeFrame`,
  `VTCompressionSessionCompleteFrames`, `CVPixelBufferCreate`,
  `CMBlockBufferGetDataPointer`, `CMSampleBufferGetFormatDescription`,
  `CMVideoFormatDescriptionGetH264ParameterSetAtIndex`,
  `CMVideoFormatDescriptionGetHEVCParameterSetAtIndex`,
  `VTIsHardwareDecodeSupported`, `AudioConverterNew`, and
  `AudioConverterFillComplexBuffer`.
- Android: `AMediaCodec_createDecoderByType`,
  `AMediaCodec_createEncoderByType`, `AMediaCodec_configure`,
  `AMediaCodec_queueInputBuffer`, and `AMediaCodec_dequeueOutputBuffer`.
- Web/WASM: WebCodecs constructor availability for decoder/encoder objects.
- Windows: `MFStartup`, `MFTEnumEx`, `IMFActivate::ActivateObject`,
  `IMFTransform::SetInputType`, `IMFTransform::SetOutputType`,
  `IMFTransform::ProcessInput`, `IMFTransform::ProcessOutput`,
  `MFCreateSample`, and `MFCreateMemoryBuffer`.
- Linux: `dlopen`/`dlsym` against GStreamer with `gst_init_check`,
  `gst_element_factory_find`, `gst_parse_launch`, `gst_buffer_map`, and
  `libgstapp` `appsrc`/`appsink` calls.

The Linux/GStreamer data plane is wired through synchronous stateful
`appsrc`/`appsink` sessions that are drained by `flush`/`finish`. H.264/H.265
input is converted to Annex-B before decode, AAC input can be wrapped as ADTS
when `AudioSpecificConfig` is supplied, video output is copied as RGBA, and
audio output is copied as interleaved F32LE. Encoder output is returned as
`EncodedPacket` values.

Android uses configured `AMediaCodec` handles with input/output buffer queues.
Video decode currently requests direct 32-bit output buffers and video encode
queues tight RGBA bytes; audio uses interleaved F32LE PCM for decoded/encoder
input and strips ADTS from AAC input before queueing.

Windows uses configured Media Foundation transforms and copies packet/frame
bytes through `IMFSample`/`IMFMediaBuffer` for H.264 video and supported audio
lanes. Video output/input is mapped through RGB32/BGRA conversion; audio output
and input uses interleaved F32LE.

Apple VideoToolbox encode marshals `RgbaFrame` values into BGRA
`CVPixelBuffer` objects and returns compressed `CMSampleBuffer` bytes from the
compression callback. Apple VideoToolbox decode builds `CMSampleBuffer` inputs
from ProRes samples or `avcC`/`hvcC`-backed H.264/HEVC samples, requests BGRA
decoder output, and drains decoded `RgbaFrame` values from the VT callback.
Apple AudioToolbox uses `AudioConverterFillComplexBuffer` callbacks for AAC and
E-AC-3 compressed decode/encode paths.

`PlatformVideoEncoder::codec_config()` exposes codec-private bytes discovered
from platform encoder output when a backend can provide them. On Apple this is
used to surface H.264 `avcC` and HEVC `hvcC` data for downstream MP4/fMP4 muxing
or configured platform decode.

WebCodecs remains constructor-probed only in the synchronous trait surface, and
those sync methods return explicit async-boundary errors. WASM callers should
use the public async `AsyncWebCodecs*` adapter types, which await browser
`flush()` Promises and drain output callbacks into crate frame/packet types.

## API Name Mapping

`platform_codec_api_names()` records the stable API names used by the platform
adapters:

| Codec | Apple | Android | WebCodecs | Windows | GStreamer |
| --- | --- | --- | --- | --- | --- |
| H.264 | `kCMVideoCodecType_H264` | `video/avc` | `avc1.*` / `avc3.*` | `MFVideoFormat_H264` | `video/x-h264` |
| H.265/HEVC | `kCMVideoCodecType_HEVC` | `video/hevc` | `hev1.*` / `hvc1.*` | `MFVideoFormat_HEVC` | `video/x-h265` |
| ProRes | `kCMVideoCodecType_AppleProRes422*` | none | none | none | `video/x-prores` |
| AAC | `kAudioFormatMPEG4AAC` | `audio/mp4a-latm` | `mp4a.*` | `MFAudioFormat_AAC` | `audio/mpeg, mpegversion=(int)4` |
| E-AC-3 | `kAudioFormatEnhancedAC3` | `audio/eac3` | none | `MFAudioFormat_Dolby_DDPlus` | `audio/x-eac3` |
| DTS | none | `audio/vnd.dts` | none | `MFAudioFormat_DTS` | `audio/x-dts` |
| WMA | none | none | none | `MFAudioFormat_WMAudioV8` / `MFAudioFormat_WMAudioV9` | `audio/x-wma` |

## Official References

- Apple Developer: VideoToolbox, `VTCompressionSession`,
  `VTDecompressionSession`, and `VTIsHardwareDecodeSupported`.
  <https://developer.apple.com/documentation/videotoolbox>
- Apple SDK headers checked locally from the active macOS SDK:
  `CoreMedia/CMFormatDescription.h`, `VideoToolbox/VTCompressionSession.h`,
  `VideoToolbox/VTDecompressionSession.h`,
  `AudioToolbox/AudioConverter.h`, and
  `CoreAudioTypes/CoreAudioBaseTypes.h`.
- Android Developers: `MediaCodec`, `MediaCodecList`, supported media formats,
  `MediaFormat` MIME constants, and NDK media APIs.
  <https://developer.android.com/reference/android/media/MediaCodec>
  <https://developer.android.com/reference/android/media/MediaCodecList>
  <https://developer.android.com/media/platform/supported-formats>
  <https://developer.android.com/reference/android/media/MediaFormat>
  <https://developer.android.com/ndk/reference/group/media>
- W3C WebCodecs and WebCodecs Codec Registry.
  <https://www.w3.org/TR/webcodecs/>
  <https://www.w3.org/TR/webcodecs-codec-registry/>
- Microsoft Learn: Media Foundation, supported Media Foundation formats, audio
  subtype GUIDs, video subtype GUIDs, and Media Foundation Transforms.
  <https://learn.microsoft.com/en-us/windows/win32/medfound/microsoft-media-foundation-sdk>
  <https://learn.microsoft.com/en-us/windows/win32/medfound/supported-media-formats-in-media-foundation>
  <https://learn.microsoft.com/en-us/windows/win32/medfound/audio-subtype-guids>
  <https://learn.microsoft.com/en-us/windows/win32/medfound/video-subtype-guids>
  <https://learn.microsoft.com/en-us/windows/win32/medfound/media-foundation-transforms>
- GStreamer application manual and Linux install docs.
  <https://gstreamer.freedesktop.org/documentation/application-development/introduction/gstreamer.html>
  <https://gstreamer.freedesktop.org/documentation/installing/on-linux.html>

Platform codec use does not by itself answer content distribution or streaming
license questions. It only keeps this crate from bundling those codec
implementations by default.
