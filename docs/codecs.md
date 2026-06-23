# Codecs

The crate separates three concepts that are often blurred together:

- **Codec IDs**: names like H.264, VP9, AAC, MP3, SRT, and WebVTT.
- **Packet-copy support**: encoded packets can be copied or remuxed without
  decoding.
- **Platform backend delegation**: codec work is intentionally routed toward an
  OS/system API that must be probed at runtime.
- **Frame decode/encode support**: packets can be converted to/from decoded
  audio or video frames.
- **Container compatibility**: a container can carry a codec without the codec
  being decoded by this crate.

## Codec IDs

`CodecId` currently models:

- Video: `H264`, `H265`, `AV1`, `VP8`, `VP9`, `Mpeg1Video`,
  `Mpeg2Video`, `Mpeg4Part2`, `ProRes`, `Theora`, `Dirac`, `RawVideo`
- Audio: `Aac`, `Ac3`, `Eac3`, `Adpcm`, `Alac`, `Opus`, `Flac`, `Mp1`,
  `Mp2`, `Mp3`, `Pcm`, `Vorbis`, `Speex`, `Dts`, `Wma`, `WavPack`
- Images: `Png`, `Jpeg`, `Gif`, `WebP`, `Avif`
- Subtitles: `Srt`, `WebVtt`
- Escape hatch: `Unknown(String)`

Modeling a codec ID does not mean this crate can decode or encode that codec.

## Implemented Support

Use `CodecRegistry::builtin()` or `builtin_codec_support()` to inspect the
current support matrix.

```rust
use video_utils_rs::{CodecId, CodecImplementationKind, CodecRegistry};

let registry = CodecRegistry::builtin();
assert!(registry.supports_decode(&CodecId::Srt, CodecImplementationKind::TextSubtitle));
assert!(registry.supports_encode(&CodecId::H264, CodecImplementationKind::PacketCopy));
#[cfg(feature = "platform-codecs")]
assert!(registry.supports_decode(&CodecId::H264, CodecImplementationKind::PlatformBackend));
#[cfg(not(feature = "codec-h264-rust"))]
assert!(!registry.supports_decode(&CodecId::H264, CodecImplementationKind::Backend));
#[cfg(feature = "codec-h264-rust")]
assert!(registry.supports_decode(&CodecId::H264, CodecImplementationKind::Backend));
#[cfg(feature = "codec-av1-rust")]
assert!(registry.supports_encode(&CodecId::AV1, CodecImplementationKind::Backend));
```

Current built-ins:

`runtime` means the platform lane is selected, but the concrete codec/profile
must be probed on the running host before use.

| Codec | Implementation kind | Decode/read | Encode/write | Meaning |
| --- | --- | ---: | ---: | --- |
| SRT | `TextSubtitle` | yes | yes | Parse/write subtitle text |
| WebVTT | `TextSubtitle` | yes | yes | Parse/write subtitle text |
| H.264 | `PacketCopy` | yes | yes | Validate/copy encoded packets |
| H.264 | `PlatformBackend` with `platform-codecs` | runtime | runtime | Probe/open the selected OS/system backend; GStreamer, Android, Windows, and Apple move packet/frame bytes; WebCodecs uses the wasm async adapter surface |
| H.264 | `Backend` with `codec-h264-rust` | yes | no | Decode Annex-B or `avcC`/length-prefixed packets to `RgbaFrame` with `RustH264Decoder` |
| H.265 | `PacketCopy` | yes | yes | Validate/copy encoded packets |
| H.265 | `PlatformBackend` with `platform-codecs` | runtime | runtime | Probe/open the selected OS/system backend; GStreamer, Android, Windows, and Apple move packet/frame bytes where the host supports the profile |
| H.265 | `Backend` with `codec-h265-rust` | yes | no | Decode Annex-B or `hvcC`/length-prefixed packets to `RgbaFrame` with `RustH265Decoder` |
| AV1 | `PacketCopy` | yes | yes | Validate/copy encoded packets |
| AV1 | `Backend` with `codec-av1-rust` | no | yes | Encode `RgbaFrame` to AV1 packets with `Rav1eAv1Encoder` |
| VP8 | `PacketCopy` | yes | yes | Validate/copy encoded packets |
| VP9 | `PacketCopy` | yes | yes | Validate/copy encoded packets |
| MPEG-1/2/4 Part 2 | `PacketCopy` | yes | yes | Validate/copy encoded packets |
| ProRes/Theora/Dirac/raw video | `PacketCopy` | yes | yes | Validate/copy encoded packets |
| ProRes | `PlatformBackend` with `platform-codecs` | runtime | runtime | Probe/open Apple VideoToolbox or GStreamer when available; GStreamer and Apple move packet/frame bytes where the host supports the profile |
| Raw RGBA video | `Backend` | yes | yes | `RawRgbaVideoDecoder`/`RawRgbaVideoEncoder` convert tightly-packed RGBA packets to/from `RgbaFrame` |
| AAC | `PacketCopy` | yes | yes | Validate/copy encoded packets |
| AAC | `PlatformBackend` with `platform-codecs` | runtime | runtime | Probe/open AudioToolbox, MediaCodec, WebCodecs, Media Foundation, or GStreamer when available; GStreamer, Android, Windows, Apple AudioToolbox, and the wasm async WebCodecs surface move packet/sample bytes where supported |
| AC-3/E-AC-3 | `PacketCopy` | yes | yes | Validate/copy encoded packets |
| E-AC-3 | `PlatformBackend` with `platform-codecs` | runtime | runtime | Probe/open platform/system codec APIs where supported; GStreamer, Android, and Windows move packet/sample bytes where mapped |
| ADPCM | `PacketCopy` | yes | yes | Validate/copy encoded packets |
| ALAC | `PacketCopy` | yes | yes | Validate/copy encoded packets |
| Opus | `PacketCopy` | yes | yes | Validate/copy encoded packets |
| FLAC | `PacketCopy` | yes | yes | Validate/copy encoded packets |
| MP1/MP2/MP3 | `PacketCopy` | yes | yes | Validate/copy encoded packets |
| PCM | `PacketCopy` | yes | yes | Validate/copy encoded packets |
| Vorbis/Speex/DTS/WMA/WavPack | `PacketCopy` | yes | yes | Validate/copy encoded packets |
| DTS/WMA | `PlatformBackend` with `platform-codecs` | runtime | runtime | Probe/open platform/system codec APIs where supported; GStreamer and Windows move decode packet/sample bytes where mapped |
| PCM WAV container | `ContainerDemuxer`/`ContainerMuxer` | yes | yes | RIFF/WAVE PCM packet demux/mux and PCM packet/frame conversion helpers |
| Opus/Vorbis/FLAC Ogg container | `ContainerDemuxer`/`ContainerMuxer` | yes | yes | Ogg page demux/mux for packet-copy audio streams |
| MPEG-TS | `ContainerDemuxer`/`ContainerMuxer` | yes | yes | PAT/PMT/PES packet demux/mux for common TS audio/video streams |
| Raw elementary streams | `ContainerDemuxer`/`ContainerMuxer` | yes | yes | Extension-guided one-stream packet demux/mux for `.h264`, `.hevc`, `.aac`, `.mp3`, `.flac`, and related streams |
| PCM WAV | `AudioFile` with `audio-io` | yes | yes | Decode/write WAV PCM with `hound`; integer 8/16/24/32-bit and float32 output |
| AAC | `AudioFile` with `audio-io` | yes | no | Decode audio files with `symphonia` |
| ADPCM | `AudioFile` with `audio-io` | yes | no | Decode audio files with `symphonia` |
| FLAC | `AudioFile` with `audio-io` | yes | no | Decode audio files with `symphonia` |
| MP1/MP2/MP3 | `AudioFile` with `audio-io` | yes | no | Decode audio files with `symphonia` |
| PCM | `AudioFile` with `audio-io` | yes | no | Decode audio files with `symphonia` |
| Vorbis | `AudioFile` with `audio-io` | yes | no | Decode audio files with `symphonia` |
| PNG | `ImageStill` with `image-io` | yes | yes | Decode/write `RgbaFrame` still images |
| JPEG | `ImageStill` with `image-io` | yes | yes | Decode/write `RgbaFrame` still images |
| GIF | `ImageStill` with `image-io` | yes | yes | Decode/write `RgbaFrame` still images |
| WebP | `ImageStill` with `image-io` | yes | yes | Decode/write `RgbaFrame` still images |
| AVIF | `ImageStill` with `image-io` | yes | yes | Decode/write `RgbaFrame` still images |

For platform backend mappings and official API references, see
[platform_codecs.md](platform_codecs.md).

## Generic Traits

For simple read/write adapters:

```rust
use video_utils_rs::{Decoder, Encoder, SubtitleTextCodec};

let mut codec = SubtitleTextCodec::srt();
let events = codec.decode(b"1\n00:00:00,000 --> 00:00:01,000\nHello\n\n")?;
let output = codec.encode(events.as_slice())?;
assert!(String::from_utf8(output).unwrap().contains("Hello"));
# Ok::<(), video_utils_rs::Error>(())
```

For packet-copy work:

```rust
use video_utils_rs::{CodecId, Decoder, Encoder, EncodedPacket, PacketCopyCodec, TimeBase};

let packet = EncodedPacket::new(
    1,
    CodecId::H264,
    0,
    1_000,
    TimeBase::milliseconds(),
    vec![0, 0, 1, 9],
);
let mut codec = PacketCopyCodec::new(CodecId::H264);
let copied = codec.decode(&packet)?;
let written = codec.encode(&copied)?;
assert_eq!(written.codec, CodecId::H264);
# Ok::<(), video_utils_rs::Error>(())
```

For WAV PCM read/write with `audio-io`:

```rust
use video_utils_rs::{AudioFrame, Decoder, Encoder, WavPcmDecoder, WavPcmEncoder};

let frame = AudioFrame::new(48_000, 2, 0, vec![0.0, 0.5, -0.5, 0.0])?;
let mut encoder = WavPcmEncoder::new();
let bytes = encoder.encode(&frame)?;
let decoded = WavPcmDecoder::new().decode(&bytes)?;
assert_eq!(decoded.channels, 2);
# Ok::<(), video_utils_rs::Error>(())
```

For PNG/JPEG still images with `image-io`:

```rust
use video_utils_rs::{Decoder, Encoder, ImageRgbaDecoder, ImageRgbaEncoder, RgbaFrame};

let frame = RgbaFrame::solid(2, 2, [255, 0, 0, 255]);
let mut encoder = ImageRgbaEncoder::png();
let bytes = encoder.encode(&frame)?;
let decoded = ImageRgbaDecoder::new().decode(&bytes)?;
assert_eq!(decoded.width, 2);
# Ok::<(), video_utils_rs::Error>(())
```

For H.264/H.265 frame decode with Rust software backends:

```rust,ignore
use video_utils_rs::{EncodedPacket, RustH264Decoder, TimeBase, VideoDecoder};

let packet = EncodedPacket::new(
    1,
    video_utils_rs::CodecId::H264,
    0,
    1,
    TimeBase::new(1, 30)?,
    annex_b_h264_bytes,
)
.with_keyframe(true);
let mut decoder = RustH264Decoder::new_annex_b();
let mut frames = decoder.decode_packet(&packet)?;
frames.extend(decoder.flush()?);
# Ok::<(), video_utils_rs::Error>(())
```

For AV1 video encode with `codec-av1-rust`:

```rust
use video_utils_rs::{Rav1eAv1Encoder, RgbaFrame, TimeBase, VideoEncoder};

let time_base = TimeBase::new(1, 30)?;
let mut encoder = Rav1eAv1Encoder::new(1, 16, 16, time_base, 1)?;
let frame = RgbaFrame::solid(16, 16, [32, 96, 160, 255]);
let mut packets = encoder.encode_frame(&frame, 0)?;
packets.extend(encoder.finish()?);
assert!(packets.iter().all(|packet| packet.codec == video_utils_rs::CodecId::AV1));
# Ok::<(), video_utils_rs::Error>(())
```

## Backend Traits

Concrete video/audio codec backends should implement:

- `VideoDecoder`
- `VideoEncoder`
- `AudioDecoder`
- `AudioEncoder`

Those traits are in place for packet-to-frame codec adapters. The currently
wired concrete backends are sidecar text, packet copy, raw RGBA video packets,
H.264 decode through `rust_h264`, H.265/HEVC decode through `rust_h265`, AV1
encode through `rav1e`, byte-oriented audio file adapters,
`SymphoniaPacketAudioDecoder` for AAC/FLAC/MP3/Opus/Vorbis packet decode, and
still-image adapters. Platform feature flags such as `codec-apple`,
`codec-android`, `codec-windows`, `codec-gstreamer`, and `codec-web` expose
target-native probe/open adapters for their OS APIs. The Linux/GStreamer lane
implements synchronous packet/frame/sample plumbing with `appsrc`/`appsink`;
Apple, Android, and Windows marshal through their native callback or buffer
APIs; and wasm/browser callers use the async WebCodecs adapter surface for
Promise/callback-driven output.

## Important Boundary

`PacketCopyCodec` is not a decoder in the video-frame sense. It implements the
generic `Decoder`/`Encoder` traits for encoded packets so packet-copy pipelines
can share the same read/write abstraction. It does not produce `RgbaFrame` or
`AudioFrame` values.

`ContainerFormat` and `plan_container_remux` answer a different question: can
the already-encoded stream be carried by the target container? For example,
H.264/AAC in MP4 can be packet-copied into MOV, but H.264/AAC to WebM requires
transcoding because WebM expects VP8/VP9/AV1 video and Opus/Vorbis audio.

Cross-container object conversion still needs a demux/mux adapter. The
object-store remux helpers therefore copy bytes for same-container object keys,
or demux and re-mux only when a Rust-native target muxer exists and the remux
plan is packet-copy-only. MP4/MOV, Matroska/WebM, MPEG-TS, Ogg, WAV, and raw
elementary targets are wired today; targets that require transcoding, such as
H.264/AAC `.mp4` to `.webm`, return an explicit unsupported error.
