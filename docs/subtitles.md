# Subtitles

The subtitle module covers sidecar manipulation and frame-level placement.

## Parse

```rust
use video_utils_rs::{SubtitleFormat, parse_subtitles};

let events = parse_subtitles(
    SubtitleFormat::Srt,
    "1\n00:00:01,000 --> 00:00:02,000\nHello\n\n",
)?;
assert_eq!(events[0].start_ms, 1_000);
# Ok::<(), video_utils_rs::Error>(())
```

Supported input formats:

- SRT with `HH:MM:SS,mmm` or `MM:SS,mmm` timestamps.
- WebVTT with `HH:MM:SS.mmm` or `MM:SS.mmm` timestamps.
- LF, CRLF, and CR line endings.

## Shift

```rust
use video_utils_rs::{SubtitleEvent, shift_events};

let events = vec![SubtitleEvent::new(1_000, 2_000, "Hello")?];
let shifted = shift_events(&events, -500);
assert_eq!(shifted[0].start_ms, 500);
# Ok::<(), video_utils_rs::Error>(())
```

Negative shifts saturate at zero and keep `end_ms > start_ms`.

## Write

```rust
use video_utils_rs::{SubtitleEvent, write_srt, write_webvtt};

let events = vec![SubtitleEvent::new(1_000, 2_000, "Hello")?];
assert!(write_srt(&events).contains("00:00:01,000"));
assert!(write_webvtt(&events).contains("00:00:01.000"));
# Ok::<(), video_utils_rs::Error>(())
```

## Active Cues

```rust
use video_utils_rs::{SubtitleEvent, active_events_at};

let events = vec![SubtitleEvent::new(1_000, 2_000, "Hello")?];
assert_eq!(active_events_at(&events, 1_500).len(), 1);
assert!(active_events_at(&events, 2_000).is_empty());
# Ok::<(), video_utils_rs::Error>(())
```

Cue end times are exclusive.

## Burn Into Frames

`burn_subtitles_onto_frame` draws currently active cues onto an `RgbaFrame`.
This is a frame-layer operation: a caller still needs a decoder to get frames
and an encoder to write video output.

```rust
use video_utils_rs::{
    RgbaFrame, SubtitleEvent, SubtitleStyle, burn_subtitles_onto_frame,
};

let events = vec![SubtitleEvent::new(1_000, 2_000, "Hello")?];
let mut frame = RgbaFrame::solid(320, 180, [8, 12, 16, 255]);
let style = SubtitleStyle {
    scale: 2,
    margin_bottom: 12,
    padding: 6,
    ..SubtitleStyle::default()
};

let result = burn_subtitles_onto_frame(&mut frame, &events, 1_500, &style)?;
assert_eq!(result.active_events, 1);
assert!(result.bounds.is_some());
# Ok::<(), video_utils_rs::Error>(())
```

The renderer uses a built-in 5x7 bitmap font. It is deliberately dependency-free
and deterministic for tests, thumbnails, previews, and portable fallback paths.
Production typography can be layered later behind a richer text-rendering
feature without changing the active-cue and frame-burn API shape.

## Packet Tracks

SRT/WebVTT events can be converted to subtitle packets and back. This is the
format used by the Matroska muxer for soft subtitle tracks.

```rust
use video_utils_rs::{
    CodecId, SubtitleEvent, TimeBase, subtitle_events_to_packets,
    subtitle_packets_to_events,
};

let events = vec![SubtitleEvent::new(1_000, 2_000, "Hello")?];
let packets = subtitle_events_to_packets(3, CodecId::Srt, TimeBase::milliseconds(), &events)?;
let round_trip = subtitle_packets_to_events(&CodecId::Srt, &packets)?;
assert_eq!(round_trip[0].text, "Hello");
# Ok::<(), video_utils_rs::Error>(())
```

## Object Workflows

With `containers`, sidecar subtitle workflows operate through
`object_store::ObjectStore`:

- `add_subtitle_sidecar_to_object_same_store` parses SRT/WebVTT and muxes it as
  a Matroska subtitle track.
- `extract_subtitle_track_to_sidecar_same_store` writes a Matroska SRT/WebVTT
  subtitle track back to a sidecar object.
- `burn_subtitle_sidecar_into_object_same_store` decodes selected video packets
  with a caller-supplied `VideoDecoder`, burns active sidecar cues into the
  decoded `RgbaFrame`s, encodes with a caller-supplied `VideoEncoder`, and muxes
  the output object.

MP4 `mov_text` soft-subtitle muxing is not implemented yet; Matroska SRT/WebVTT
is the native soft-subtitle container path today.
