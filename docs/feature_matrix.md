# Feature Matrix

This matrix describes the current implementation, not the long-term roadmap.
It separates three surfaces that are easy to confuse:

- Packet-copy support: encoded packets can be modeled, validated, copied, and
  remuxed without decoding frames.
- Backend support: encoded packets can be decoded into frames or frames can be
  encoded back into packets.
- Container adapter support: object bytes can actually be demuxed or muxed by a
  Rust-native adapter.

## Legend

| Mark | Meaning |
| --- | --- |
| yes | Implemented in the crate today |
| no | Not implemented today |
| feature | Implemented only when the named Cargo feature is enabled |
| runtime | Platform/system API lane exists, but codec/profile support must be probed on the running host |
| policy | Format/codec is modeled for planning, but no byte adapter is wired |
| descriptor | Capability metadata exists |
| native handle | A target-native API object can be probed/opened, but packet/frame marshaling is not complete |
| packet copy | Encoded packets are preserved; no frame decode/encode happens |

## Cargo Features

| Feature | Default | Main surface | Dependencies or backend | Current status |
| --- | ---: | --- | --- | --- |
| `cli` | yes | `video-utils` binary | `clap` | Small diagnostics CLI |
| `platform-codecs` | yes | Platform/system backend descriptors and native adapters | target-specific system APIs | Runtime-probed OS/system codec lanes; GStreamer, Android MediaCodec, Windows Media Foundation, Apple VideoToolbox/AudioToolbox, and wasm async WebCodecs have packet/frame/sample plumbing where supported |
| `portable-core` | yes | Default library bundle | `packet-ops`, `containers`, `audio-core`, `frame-core`, `subtitles`, `streaming` | Convenience feature for the packet/container/audio/frame/subtitle/streaming core |
| `packet-ops` | yes | Packet helpers and bitstream filters | `h264-reader` | Packet model is core; H.264-oriented helper dependency is feature gated |
| `containers` | yes | Container demux/mux adapters | `re_mp4`, `matroska-demuxer`, `muxide` | MP4/MOV/fMP4, Matroska/WebM, MPEG-TS, FLV, Ogg, WAV, AIFF, and raw elementary demux/mux paths |
| `mp4e-muxer` | no | Alternate MP4 muxer dependency lane | `mp4e` | Dependency lane only; the active MP4 object mux path uses `muxide` |
| `audio-core` | yes | Decoded PCM helpers | none | Gain, fades, normalization, mixing, waveform, silence detection |
| `audio-io` | no | Audio file decode/write | `hound`, `rubato`, `symphonia` | WAV PCM read/write and Symphonia decode |
| `frame-core` | yes | Decoded RGBA helpers | none | Crop, resize, pad, flip, rotate, blur, filters, overlay, watermark, black-bar detection |
| `image-io` | no | Still-image decode/write | `image`, `imageproc`, `fast_image_resize`, `v_frame`, `yuv` | PNG/JPEG/GIF/WebP/AVIF to/from `RgbaFrame` |
| `preview` | no | Preview codec dependency lane | `gif`, `png`, `ravif` | Dependency lane for preview-oriented work |
| `svg` | no | SVG rasterization dependency lane | `resvg` | Dependency lane; no broad SVG workflow surface beyond feature wiring |
| `subtitles` | yes | Subtitle sidecars and burn-in | `ab_glyph` | SRT/WebVTT parse/write, cue lookup, frame burn-in; parser/burn-in is crate-local |
| `streaming` | yes | HLS helpers | `m3u8-rs` | Media playlist writing, keyframe segment planning, object-store TS segments, and fMP4 HLS for configured H.264/H.265 video, AAC audio, and WebVTT subtitle tracks |
| `codec-h264-rust` | no | Rust software video backend | `rust_h264` | H.264 decode to `RgbaFrame`; no H.264 encode |
| `codec-h265-rust` | no | Rust software video backend | `rust_h265` | H.265/HEVC decode to `RgbaFrame`; no H.265 encode |
| `codec-av1-rust` | no | Rust software video backend | `rav1e` | AV1 encode from `RgbaFrame`; no AV1 decode |
| `codec-openh264-ffi` | no | Native H.264 FFI lane | none | Descriptor only; no concrete adapter is compiled by this crate |
| `codec-apple` | no | Apple VideoToolbox/AudioToolbox lane | Apple frameworks | Explicit platform lane; VideoToolbox and AudioToolbox marshal packet/frame/sample bytes through platform callbacks; also selected by default through `platform-codecs` on Apple targets |
| `codec-android` | no | Android MediaCodec lane | Android NDK media | Explicit platform lane with AMediaCodec buffer queue plumbing; also selected by default through `platform-codecs` on Android |
| `codec-windows` | no | Windows Media Foundation lane | `windows` crate | Explicit platform lane with IMFSample/IMFMediaBuffer plumbing; also selected by default through `platform-codecs` on Windows |
| `codec-gstreamer` | no | Linux GStreamer lane | dynamic `libgstreamer-1.0` and `libgstapp-1.0` | Explicit platform lane with appsrc/appsink encode/decode plumbing; also selected by default through `platform-codecs` on Linux |
| `codec-web` | no | WebCodecs lane | `web-sys`/`wasm-bindgen` | Explicit platform lane with async `AsyncWebCodecs*` adapters; also selected by default through `platform-codecs` on WASM/browser targets |

## Codec Surfaces

| Media | Codec or group | Surface | Read/decode | Write/encode | Feature | Notes |
| --- | --- | --- | ---: | ---: | --- | --- |
| Subtitle | SRT | Text sidecar and Matroska packet track | yes | yes | default | Parse/write `.srt`, shift cues, active-cue lookup, packetize/extract Matroska subtitles, burn active cues onto decoded RGBA frames |
| Subtitle | WebVTT | Text sidecar and Matroska/fMP4 packet track | yes | yes | default | Parse/write `.vtt`, shift cues, active-cue lookup, packetize/extract Matroska subtitles, fMP4 `wvtt` cue samples, burn active cues onto decoded RGBA frames |
| Video | H.264 | Packet copy | yes | yes | default | Encoded packet passthrough/remux; not decoded by this surface |
| Video | H.264 | Platform backend | runtime | runtime | `platform-codecs` | Probes/opens VideoToolbox, MediaCodec, WebCodecs, Media Foundation, or GStreamer on the active target; native targets and wasm async WebCodecs move packet/frame bytes where supported |
| Video | H.264 | Backend | feature | no | `codec-h264-rust` | `RustH264Decoder` decodes Annex-B or `avcC`/length-prefixed packets to `RgbaFrame` |
| Video | H.265/HEVC | Packet copy | yes | yes | default | Encoded packet passthrough/remux; not decoded by this surface |
| Video | H.265/HEVC | Platform backend | runtime | runtime | `platform-codecs` | Probes/opens target platform/system codecs; native targets and wasm async WebCodecs have byte plumbing where the host supports the profile |
| Video | H.265/HEVC | Backend | feature | no | `codec-h265-rust` | `RustH265Decoder` decodes Annex-B or `hvcC`/length-prefixed packets to `RgbaFrame` |
| Video | AV1 | Packet copy | yes | yes | default | Encoded packet passthrough/remux; not decoded by this surface |
| Video | AV1 | Backend | no | feature | `codec-av1-rust` | `Rav1eAv1Encoder` encodes `RgbaFrame` to AV1 packets |
| Video | VP8, VP9 | Packet copy | yes | yes | default | Encoded packet passthrough/remux only |
| Video | VP8, VP9 | Backend | no | no | none | No pure Rust reconstructed-frame backend is wired |
| Video | MPEG-1, MPEG-2, MPEG-4 Part 2 | Packet copy | yes | yes | default | Encoded packet passthrough/remux only |
| Video | ProRes, Theora, Dirac | Packet copy | yes | yes | default | Encoded packet passthrough/remux only |
| Video | ProRes | Platform backend | runtime | runtime | `platform-codecs` | Apple VideoToolbox or GStreamer platform lane; GStreamer and Apple have native byte plumbing where exact profiles are supported |
| Video | Raw video | Packet copy | yes | yes | default | Encoded packet passthrough/remux for `RawVideo` packets |
| Video | Raw RGBA | Backend | yes | yes | default | `RawRgbaVideoDecoder` and `RawRgbaVideoEncoder` convert tightly packed RGBA packets to/from `RgbaFrame` |
| Audio | AAC, AC-3, E-AC-3, ADPCM, ALAC, Opus, FLAC, MP1, MP2, MP3, PCM, Vorbis, Speex, DTS, WMA, WavPack | Packet copy | yes | yes | default | Encoded packet passthrough/remux only |
| Audio | AAC, E-AC-3, DTS, WMA | Platform backend | runtime | runtime | `platform-codecs` | Probes/opens platform/system codec APIs where the active target reports support; GStreamer, Android, Windows, Apple AudioToolbox, and wasm async WebCodecs move packet/sample bytes for mapped codecs |
| Audio | PCM WAV container | Container packets | yes | yes | default | RIFF/WAVE PCM demux/mux through `WavDemuxer`/`WavMuxer`; PCM packets can convert to/from `AudioFrame` |
| Audio | Opus, Vorbis, FLAC in Ogg | Container packets | yes | yes | default | Ogg page demux/mux through `OggDemuxer`/`OggMuxer`; Vorbis/FLAC muxing requires codec-private header data |
| Audio/Video | MPEG-TS packet streams | Container packets | yes | yes | default | TS PAT/PMT/PES demux/mux for H.264, H.265, MPEG-2 video, AAC, AC-3, E-AC-3, MP2, and MP3; demux resyncs on packet boundaries and mux writes PCR |
| Audio/Video/Subtitle | Fragmented MP4 segments | Streaming container packets | yes | yes | default | fMP4 reads and writes configured H.264/H.265 video, AAC audio, and WebVTT subtitle tracks as `init.mp4` plus `moof`/`mdat` media fragments |
| Audio/Video | FLV | Container packets | yes | yes | default | FLV demux/mux for H.264 video and AAC audio packet-copy workflows |
| Audio | AIFF/AIFC PCM | Container packets | yes | yes | default | AIFF/AIFC uncompressed integer PCM demux/mux with endian normalization to the crate PCM packet format |
| Audio/Video | Raw elementary streams | Container packets | yes | yes | default | Extension-guided raw `.h264`, `.hevc`, `.aac`, `.mp3`, `.flac`, etc.; standalone streams carry limited metadata |
| Audio | PCM WAV | Audio file | feature | feature | `audio-io` | `WavPcmDecoder` and `WavPcmEncoder` through `hound` |
| Audio | AAC, ADPCM, FLAC, MP1, MP2, MP3, PCM, Vorbis | Audio file | feature | no | `audio-io` | Decode through Symphonia from supported audio containers |
| Audio | AAC, FLAC, MP3, Opus, Vorbis | Audio packet backend | feature | no | `audio-io` | `SymphoniaPacketAudioDecoder` decodes compressed packets to `AudioFrame` when stream metadata and required codec-private data are supplied |
| Audio | AC-3, E-AC-3, ALAC, Speex, DTS, WMA, WavPack | Audio frame backend | no | no | none | Packet-copy modeled, but no decoded-frame backend is wired |
| Image | PNG, JPEG, GIF, WebP, AVIF | Still image | feature | feature | `image-io` | Decode/write still images as `RgbaFrame` |

## Codec Trait Setup

| Trait or type | Purpose | Implemented adapters today |
| --- | --- | --- |
| `CodecDescriptor` | Common adapter identity and `CodecId` | All concrete codec adapters |
| `Decoder` / `Encoder` | Generic byte/object-ish read/write adapters | `SubtitleTextCodec`, `PacketCopyCodec`, `WavPcmDecoder`/`WavPcmEncoder`, `ImageRgbaDecoder`/`ImageRgbaEncoder` |
| `VideoDecoder` / `VideoEncoder` | Encoded video packet to decoded frame, and decoded frame to packet | `RawRgbaVideoDecoder`, `RawRgbaVideoEncoder`, optional `RustH264Decoder`, optional `RustH265Decoder`, optional `Rav1eAv1Encoder` |
| `AudioDecoder` / `AudioEncoder` | Encoded audio packet to PCM frame, and PCM frame to encoded packet | Optional `SymphoniaPacketAudioDecoder` for AAC/FLAC/MP3/Opus/Vorbis decode; no compressed audio packet encoder is wired today |
| `CodecBackendDescriptor` | Common capability trait for platform, bundled, and FFI backend descriptors | `BackendCapability` implements it |
| `CodecRegistry` | Runtime support inventory | Reports text subtitles, packet copy, raw RGBA, platform backend descriptors/adapters, optional H.264/H.265 decode, optional AV1 encode, optional audio/image IO |
| `Unsupported*` adapters | Explicit missing-backend objects | `UnsupportedVideoDecoder`, `UnsupportedVideoEncoder`, `UnsupportedAudioDecoder`, `UnsupportedAudioEncoder` |

`PacketCopyCodec` implements the generic `Decoder` and `Encoder` traits for
`EncodedPacket`, but it is not a decoded-frame codec. It validates and returns
encoded packets unchanged.

## Container And Object-Store Support

All media IO helpers operate on `object_store::ObjectStore` and
`object_store::path::Path`; they do not require native filesystem paths.

| Format or group | Detect from key | Detect from header | Demux/probe object bytes | Mux object bytes | Same-container object copy | Cross-container packet-copy remux | Notes |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | --- |
| MP4 / M4V / M4A | yes | yes | yes | yes | yes | yes | Flat MP4 demux through `re_mp4`; fMP4 demux through crate-local `moov`/`moof` parser; mux through `muxide`; fMP4 segment mux through crate-local writer |
| MOV / QuickTime | yes | yes | yes | yes | yes | yes | MOV demux uses the ISO-BMFF reader; MOV mux uses the ISO-BMFF mux path with QuickTime branding |
| Matroska / MKV | yes | yes | yes | yes | yes | yes | Demux through `matroska-demuxer`; mux through crate-local EBML writer |
| WebM | yes | yes | yes | yes | yes | yes | Demux through `matroska-demuxer`; mux through crate-local EBML writer with WebM-compatible streams |
| MPEG-TS / M2TS / MTS | yes | yes | yes | yes | yes | yes | Crate-local PAT/PMT/PES packet-copy demux/mux for common TS audio/video codecs |
| MPEG-PS / VOB | yes | yes | no | no | yes | no | Modeled for planning and detection; no demux/mux adapter |
| AVI | yes | yes | no | no | yes | no | Modeled for planning and detection; no demux/mux adapter |
| FLV | yes | yes | yes | yes | yes | yes | Crate-local FLV demux/mux for H.264 video and AAC audio |
| Ogg / OGV / OGA / Opus | yes | yes | yes | yes | yes | yes | Opus, Vorbis, and FLAC packet-copy demux/mux |
| WAV | yes | yes | yes | yes | yes | yes | PCM integer and float WAV demux/mux |
| AIFF / AIFC | yes | yes | yes | yes | yes | yes | Crate-local AIFF/AIFC adapter for uncompressed integer PCM |
| Raw elementary streams | yes | no | yes | yes | yes | yes | Extension-guided packet streams; mux output carries exactly one stream |

Object-store byte helpers:

| Helper group | Status | Notes |
| --- | --- | --- |
| Whole-object read/write | yes | `read_object_bytes`, `write_object_bytes` |
| Range/chunk reads | yes | `read_object_range`, `read_object_chunks` |
| Same-store object copy | yes | `copy_object_same_store` and same-container remux path use object-store copy |
| Cross-store byte copy | yes | `copy_object_between_stores` and same-container remux path use object-store get/put |
| Probe from object | yes | MP4/MOV including fMP4, Matroska/WebM, MPEG-TS, FLV, Ogg, WAV, AIFF, and raw elementary streams when `containers` is enabled |
| Mux to object | yes | MP4/MOV, Matroska/WebM, MPEG-TS, FLV, Ogg, WAV, AIFF, and raw elementary streams when `containers` is enabled |
| Cross-container remux | partial | Requires a packet-copy-only plan and an available target muxer; it never byte-copies between different container extensions |
| Object transcode jobs | partial | `ObjectTranscodeJob` chooses exact copy, packet-copy remux, or one configured decoded-frame video stage with caller-supplied `VideoDecoder`/`VideoEncoder`; audio/subtitle transcode stages are not wired |

## Container Codec Policy

`ContainerFormat::supports_codec` and `plan_container_remux` model whether a
stream can be packet-copied into a target format. That policy is broader than
the mux adapters available today.

| Target | Packet-copy policy allows | Mux adapter actually writes today |
| --- | --- | --- |
| MP4 | H.264, H.265, AV1, VP9, MPEG-4 Part 2, AAC, AC-3, E-AC-3, ALAC, FLAC, MP3, Opus | H.264, H.265, AV1, VP9 video plus AAC or Opus audio; one video stream and at most one audio stream |
| MOV / QuickTime | H.264, H.265, MPEG-4 Part 2, ProRes, raw video, AAC, ALAC, MP3, PCM | H.264, H.265, AV1, VP9 video plus AAC or Opus audio through the ISO-BMFF mux path |
| WebM | VP8, VP9, AV1, Opus, Vorbis | WebM mux adapter, constrained by Matroska/WebM codec ID mapping and required codec-private data |
| Matroska | Broad video/audio plus SRT/WebVTT subtitles | Broad Matroska mux adapter, constrained by codec ID mapping and required codec-private data |
| Ogg | Theora, Dirac, Vorbis, Opus, FLAC, Speex | Opus, Vorbis, and FLAC audio |
| WAV | PCM and ADPCM | PCM audio |
| MPEG-TS | H.264, H.265, MPEG-2 video, AAC, AC-3, E-AC-3, MP2, MP3 | Same set through the crate-local TS muxer |
| FLV | H.264, AAC, MP3 | H.264 and AAC |
| AIFF | PCM | PCM integer |
| Raw elementary | Any modeled non-unknown codec | One stream at a time; H.264/H.265 convert to Annex-B and AAC can wrap ADTS |
| MPEG-PS, AVI | Modeled by container policy | no object mux adapter |

## Decoded Frame, Audio, Subtitle, And Streaming Tools

| Area | Feature | Current operations |
| --- | --- | --- |
| Decoded video frames | default | `RgbaFrame`, pixel read/write, crop, pad, nearest-neighbor resize, horizontal/vertical flip, 90-degree rotation, alpha overlay, box blur, color filters, watermark, black-bar detection |
| Frame pipelines | default | Ordered `FrameTransformPipeline` over one frame or an iterator of frames |
| Object video transforms | `containers` plus supplied codecs | Demux object, decode selected video track with caller-supplied `VideoDecoder`, apply frame pipeline, encode with caller-supplied `VideoEncoder`, mux to MP4/MOV, Matroska/WebM, MPEG-TS, or raw elementary when the output codec fits |
| Object transcode orchestration | `containers` plus supplied codecs for decode stages | `transcode_object_*` runs exact copy or packet-copy remux when possible, otherwise runs a configured decoded-frame video stage and muxes the result while preserving compatible packet-copy streams |
| Decoded audio frames | default | `AudioFrame`, gain, gain in dB, peak normalization, fades, mixing, waveform buckets, silence detection |
| Audio pipelines | default | Ordered `AudioTransformPipeline` with gain, dB gain, peak normalization, and fades |
| Object audio transforms | `containers` | Demux PCM audio packets, apply an `AudioTransformPipeline`, and mux WAV PCM output |
| Object audio file transforms | `containers` + `audio-io` | Decode an audio object with Symphonia, apply an `AudioTransformPipeline`, and mux WAV PCM output |
| Subtitles | default / `containers` for object workflows | SRT/WebVTT parse/write, shift, active event lookup, packetize/extract Matroska subtitle tracks, subtitle overlay rendering, burn-in onto decoded RGBA frames or object-store video jobs with supplied codecs |
| Streaming | default | HLS VOD media playlist writing, keyframe-aligned packet segment planning, object-store MPEG-TS segments, and object-store fMP4 HLS segments for configured H.264/H.265 video, AAC audio, and WebVTT subtitle tracks |

## Backend Capability Descriptors

`recommended_backends_for_current_target()` exposes target/backend capability
metadata. Only the Rust software lanes listed below have concrete portable
adapters in this crate today. Platform lanes also have target-native
probe/open adapters; Linux/GStreamer additionally has synchronous
`appsrc`/`appsink` packet/frame plumbing.

| Backend kind | Target | Decode capabilities reported | Encode capabilities reported | Concrete adapter status |
| --- | --- | --- | --- | --- |
| `RustSoftware` | Native | H.264 with `codec-h264-rust`, H.265 with `codec-h265-rust` | AV1 with `codec-av1-rust` | Concrete adapters wired; `BackendSource::BundledNative`, `BackendProbe::Static` |
| `AppleVideoToolbox` | Apple | H.264, H.265, ProRes | H.264, H.265, ProRes | Runtime-probed VideoToolbox session handle with CMSampleBuffer/CVPixelBuffer callback plumbing |
| `AppleAudioToolbox` | Apple | AAC, E-AC-3 | AAC | Runtime-probed AudioToolbox converter handle with AudioConverter callback plumbing |
| `AndroidMediaCodec` | Android | H.264, H.265, AV1, VP8, VP9, MPEG-2, MPEG-4 Part 2, AAC, E-AC-3, DTS | H.264, H.265, VP8, VP9, AAC | Runtime-probed AMediaCodec handle with buffer queue marshaling |
| `WebCodecs` | WASM/browser | H.264, H.265, AV1, VP8, VP9, AAC | H.264, H.265, AV1, VP8, VP9, AAC | Runtime-probed WebCodecs constructor handle; sync traits return explicit async-boundary errors; async adapter types marshal packets/frames |
| `WindowsMediaFoundation` | Windows | H.264, H.265, AAC, E-AC-3, DTS, WMA | H.264, AAC, WMA | Runtime-probed Media Foundation transform handle with IMFSample/IMFMediaBuffer process plumbing |
| `GStreamer` | Linux | DTS, WMA, ProRes, AAC, E-AC-3, H.264, H.265 | H.264, H.265, ProRes, AAC, E-AC-3 | Runtime-probed GStreamer `appsrc`/`appsink` pipelines; packet/frame/sample bytes are copied to and from crate types |
| `OpenH264Ffi` | Native | H.264 | H.264 | Descriptor only; feature currently has no dependency |

## Known Gaps

| Gap | Current behavior |
| --- | --- |
| FFmpeg/libav backend | Intentionally absent; Linux platform default is the system GStreamer plugin lane |
| Apple fixture-level decode/encode CI coverage | Apple platform plumbing is compiled and probed on macOS; fixture-level VideoToolbox decode and AudioToolbox compressed audio tests need known-good compressed packets plus codec-private config |
| WebCodecs synchronous trait mismatch | Browser WebCodecs completes through Promises/output callbacks; synchronous traits return explicit errors, and wasm callers use the async adapter types |
| Bundled H.264 encode | Not implemented; platform lanes may report runtime-probed system encode |
| Bundled H.265/HEVC encode | Not implemented; platform lanes may report runtime-probed system encode |
| AV1 decode | Not implemented |
| VP8/VP9 decoded-frame backend | Not implemented |
| Compressed audio packet encoders | `SymphoniaPacketAudioDecoder` covers AAC/FLAC/MP3/Opus/Vorbis decode with `audio-io`; compressed audio packet encode is not wired |
| fMP4 non-AAC audio tracks | fMP4 currently reads/writes H.264/H.265 video, AAC audio, and WebVTT subtitles; use MPEG-TS, FLV, or Matroska workflows for other stream types |
| MPEG-PS and AVI object demux/mux | Modeled for detection/planning, not wired as object demux/mux adapters |
| End-to-end subtitle burn-in on compressed video | Supported when the caller supplies a matching video decoder and encoder pair |
| Transcode-required remuxes | `ObjectTranscodeJob` can satisfy selected video transcodes when matching decoder/encoder backends are supplied; object remux helpers still return unsupported unless a packet-copy-only mux path exists |
