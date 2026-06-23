use crate::{
    codec::CodecId,
    error::{Error, Result},
    frame::{CropRect, RgbaFrame},
    packet::EncodedPacket,
    time::TimeBase,
};
use bytes::Bytes;

/// Supported sidecar subtitle formats in the portable core.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SubtitleFormat {
    /// SubRip `.srt`.
    Srt,
    /// WebVTT `.vtt`.
    WebVtt,
}

/// Normalized subtitle event.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SubtitleEvent {
    /// Optional source cue index.
    pub index: Option<usize>,
    /// Start time in milliseconds.
    pub start_ms: i64,
    /// End time in milliseconds.
    pub end_ms: i64,
    /// Cue payload text.
    pub text: String,
}

/// Styling used when drawing subtitle cues onto decoded RGBA frames.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SubtitleStyle {
    /// Text color.
    pub text_color: [u8; 4],
    /// Outline color. Use alpha 0 to disable outlines.
    pub outline_color: [u8; 4],
    /// Backing box color. Use alpha 0 to disable the box.
    pub box_color: [u8; 4],
    /// Distance from the bottom edge in pixels.
    pub margin_bottom: u32,
    /// Padding around the rendered text in pixels.
    pub padding: u32,
    /// Integer pixel scale for the built-in 5x7 bitmap font.
    pub scale: u32,
    /// Extra vertical gap between rendered text lines in pixels.
    pub line_gap: u32,
}

impl Default for SubtitleStyle {
    fn default() -> Self {
        Self {
            text_color: [255, 255, 255, 255],
            outline_color: [0, 0, 0, 255],
            box_color: [0, 0, 0, 160],
            margin_bottom: 24,
            padding: 8,
            scale: 2,
            line_gap: 4,
        }
    }
}

/// Result returned by subtitle frame rendering.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SubtitleRenderResult {
    /// Number of active cues rendered.
    pub active_events: usize,
    /// Rendered bounding box, when at least one cue was active.
    pub bounds: Option<CropRect>,
}

impl SubtitleEvent {
    /// Create a subtitle event.
    pub fn new(start_ms: i64, end_ms: i64, text: impl Into<String>) -> Result<Self> {
        if end_ms <= start_ms {
            return Err(Error::InvalidRange {
                start: start_ms,
                end: end_ms,
            });
        }

        Ok(Self {
            index: None,
            start_ms,
            end_ms,
            text: text.into(),
        })
    }
}

/// Parse supported subtitle text into events.
pub fn parse_subtitles(format: SubtitleFormat, input: &str) -> Result<Vec<SubtitleEvent>> {
    match format {
        SubtitleFormat::Srt => parse_srt(input),
        SubtitleFormat::WebVtt => parse_webvtt(input),
    }
}

/// Return the events active at a timestamp in milliseconds.
#[must_use]
pub fn active_events_at(events: &[SubtitleEvent], time_ms: i64) -> Vec<&SubtitleEvent> {
    events
        .iter()
        .filter(|event| event.start_ms <= time_ms && time_ms < event.end_ms)
        .collect()
}

/// Shift subtitle events by milliseconds. Negative shifts saturate at zero.
#[must_use]
pub fn shift_events(events: &[SubtitleEvent], offset_ms: i64) -> Vec<SubtitleEvent> {
    events
        .iter()
        .map(|event| {
            let mut shifted = event.clone();
            shifted.start_ms = (shifted.start_ms + offset_ms).max(0);
            shifted.end_ms = (shifted.end_ms + offset_ms).max(shifted.start_ms + 1);
            shifted
        })
        .collect()
}

/// Write normalized events as SRT.
#[must_use]
pub fn write_srt(events: &[SubtitleEvent]) -> String {
    let mut output = String::new();
    for (index, event) in events.iter().enumerate() {
        output.push_str(&(index + 1).to_string());
        output.push('\n');
        output.push_str(&format_timestamp_srt(event.start_ms));
        output.push_str(" --> ");
        output.push_str(&format_timestamp_srt(event.end_ms));
        output.push('\n');
        output.push_str(event.text.trim());
        output.push_str("\n\n");
    }
    output
}

/// Write normalized events as WebVTT.
#[must_use]
pub fn write_webvtt(events: &[SubtitleEvent]) -> String {
    let mut output = String::from("WEBVTT\n\n");
    for event in events {
        output.push_str(&format_timestamp_webvtt(event.start_ms));
        output.push_str(" --> ");
        output.push_str(&format_timestamp_webvtt(event.end_ms));
        output.push('\n');
        output.push_str(event.text.trim());
        output.push_str("\n\n");
    }
    output
}

/// Return the subtitle codec represented by a sidecar subtitle format.
#[must_use]
pub const fn subtitle_codec_for_format(format: SubtitleFormat) -> CodecId {
    match format {
        SubtitleFormat::Srt => CodecId::Srt,
        SubtitleFormat::WebVtt => CodecId::WebVtt,
    }
}

/// Return the sidecar subtitle format represented by a subtitle codec.
#[must_use]
pub fn subtitle_format_for_codec(codec: &CodecId) -> Option<SubtitleFormat> {
    match codec {
        CodecId::Srt => Some(SubtitleFormat::Srt),
        CodecId::WebVtt => Some(SubtitleFormat::WebVtt),
        _ => None,
    }
}

/// Convert normalized subtitle events into encoded subtitle packets.
pub fn subtitle_events_to_packets(
    track_id: u32,
    codec: CodecId,
    time_base: TimeBase,
    events: &[SubtitleEvent],
) -> Result<Vec<EncodedPacket>> {
    if subtitle_format_for_codec(&codec).is_none() {
        return Err(Error::Unsupported {
            operation: "subtitle packets",
            reason: "subtitle packet conversion supports SRT and WebVTT codecs",
        });
    }

    let milliseconds = TimeBase::milliseconds();
    let mut packets = Vec::with_capacity(events.len());
    for event in events {
        let pts = milliseconds.rescale(event.start_ms, time_base);
        let end = milliseconds.rescale(event.end_ms, time_base);
        if end <= pts {
            return Err(Error::InvalidRange {
                start: event.start_ms,
                end: event.end_ms,
            });
        }
        packets.push(
            EncodedPacket::new(
                track_id,
                codec.clone(),
                pts,
                end - pts,
                time_base,
                Bytes::from(event.text.trim().to_owned()),
            )
            .with_keyframe(true),
        );
    }
    Ok(packets)
}

/// Convert encoded subtitle packets into normalized subtitle events.
pub fn subtitle_packets_to_events(
    codec: &CodecId,
    packets: &[EncodedPacket],
) -> Result<Vec<SubtitleEvent>> {
    if subtitle_format_for_codec(codec).is_none() {
        return Err(Error::Unsupported {
            operation: "subtitle packets",
            reason: "subtitle packet conversion supports SRT and WebVTT codecs",
        });
    }

    let milliseconds = TimeBase::milliseconds();
    let mut events = Vec::with_capacity(packets.len());
    for (index, packet) in packets.iter().enumerate() {
        if &packet.codec != codec {
            return Err(Error::CodecMismatch {
                expected: codec.clone(),
                actual: packet.codec.clone(),
            });
        }
        let start_ms = packet.time_base.rescale(packet.pts, milliseconds);
        let end_ms = packet
            .time_base
            .rescale(packet.pts + packet.duration.max(1), milliseconds)
            .max(start_ms + 1);
        let text = std::str::from_utf8(&packet.data)
            .map_err(|err| Error::Parse {
                format: match codec {
                    CodecId::Srt => "srt",
                    CodecId::WebVtt => "webvtt",
                    _ => "subtitle",
                },
                message: format!("subtitle packet is not valid UTF-8: {err}"),
            })?
            .to_owned();
        let mut event = SubtitleEvent::new(start_ms, end_ms, text)?;
        event.index = Some(index + 1);
        events.push(event);
    }
    Ok(events)
}

/// Render active subtitle cues into a transparent RGBA overlay.
pub fn render_subtitle_overlay(
    width: u32,
    height: u32,
    events: &[SubtitleEvent],
    time_ms: i64,
    style: &SubtitleStyle,
) -> Result<SubtitleRenderResult> {
    let active = active_events_at(events, time_ms);
    if active.is_empty() {
        return Ok(SubtitleRenderResult {
            active_events: 0,
            bounds: None,
        });
    }

    let scale = style.scale.max(1);
    let padding = style.padding;
    let lines = wrapped_subtitle_lines(&active, width, style);
    if lines.is_empty() {
        return Ok(SubtitleRenderResult {
            active_events: active.len(),
            bounds: None,
        });
    }

    let text_width = lines
        .iter()
        .map(|line| measure_line_width(line, scale))
        .max()
        .unwrap_or(0);
    let line_height = glyph_height(scale);
    let text_height = line_height * lines.len() as u32
        + style
            .line_gap
            .saturating_mul(lines.len().saturating_sub(1) as u32);
    let box_width = text_width.saturating_add(padding * 2).min(width);
    let box_height = text_height.saturating_add(padding * 2).min(height);

    let x = width.saturating_sub(box_width) / 2;
    let y = height.saturating_sub(style.margin_bottom.saturating_add(box_height));
    let mut overlay = RgbaFrame::solid(width, height, [0, 0, 0, 0]);

    if style.box_color[3] != 0 {
        fill_rect(&mut overlay, x, y, box_width, box_height, style.box_color);
    }

    let text_start_y = y + padding;
    for (line_index, line) in lines.iter().enumerate() {
        let line_width = measure_line_width(line, scale);
        let line_x = x + box_width.saturating_sub(line_width) / 2;
        let line_y = text_start_y + line_index as u32 * (line_height + style.line_gap);
        draw_text(&mut overlay, line, line_x, line_y, style);
    }

    Ok(SubtitleRenderResult {
        active_events: active.len(),
        bounds: Some(CropRect::new(x, y, box_width, box_height)),
    })
}

/// Burn active subtitle cues into a decoded RGBA frame.
pub fn burn_subtitles_onto_frame(
    frame: &mut RgbaFrame,
    events: &[SubtitleEvent],
    time_ms: i64,
    style: &SubtitleStyle,
) -> Result<SubtitleRenderResult> {
    let result = render_subtitle_overlay(frame.width, frame.height, events, time_ms, style)?;
    if result.bounds.is_some() {
        let active = active_events_at(events, time_ms);
        let mut overlay = RgbaFrame::solid(frame.width, frame.height, [0, 0, 0, 0]);
        draw_active_subtitles_into_overlay(&mut overlay, &active, style);
        frame.overlay(&overlay, 0, 0);
    }
    Ok(result)
}

fn parse_srt(input: &str) -> Result<Vec<SubtitleEvent>> {
    let input = normalize_newlines(input);
    let mut events = Vec::new();
    for block in input.split("\n\n").filter(|block| !block.trim().is_empty()) {
        let mut lines = block.lines().filter(|line| !line.trim().is_empty());
        let first = lines.next().ok_or_else(|| parse_error("missing cue"))?;
        let (index, timing_line) = if first.trim().chars().all(|ch| ch.is_ascii_digit()) {
            (
                first.trim().parse::<usize>().ok(),
                lines
                    .next()
                    .ok_or_else(|| parse_error("missing timestamp line"))?,
            )
        } else {
            (None, first)
        };

        let (start_ms, end_ms) = parse_timing_line(timing_line, ',')?;
        let text = lines.collect::<Vec<_>>().join("\n");
        let mut event = SubtitleEvent::new(start_ms, end_ms, text)?;
        event.index = index;
        events.push(event);
    }
    Ok(events)
}

fn parse_webvtt(input: &str) -> Result<Vec<SubtitleEvent>> {
    let input = normalize_newlines(input);
    let input = input.trim_start_matches('\u{feff}');
    let input = input.strip_prefix("WEBVTT").unwrap_or(input);
    let mut events = Vec::new();

    for block in input.split("\n\n").filter(|block| !block.trim().is_empty()) {
        let mut lines = block.lines().filter(|line| !line.trim().is_empty());
        let first = lines.next().ok_or_else(|| parse_error("missing cue"))?;
        let timing_line = if first.contains("-->") {
            first
        } else {
            lines
                .next()
                .ok_or_else(|| parse_error("missing timestamp line"))?
        };

        let (start_ms, end_ms) = parse_timing_line(timing_line, '.')?;
        let text = lines.collect::<Vec<_>>().join("\n");
        events.push(SubtitleEvent::new(start_ms, end_ms, text)?);
    }

    Ok(events)
}

fn parse_timing_line(line: &str, separator: char) -> Result<(i64, i64)> {
    let (start, end) = line
        .split_once("-->")
        .ok_or_else(|| parse_error("timestamp line must contain -->"))?;
    Ok((
        parse_timestamp(start.trim(), separator)?,
        parse_timestamp(
            end.split_whitespace().next().unwrap_or("").trim(),
            separator,
        )?,
    ))
}

fn parse_timestamp(input: &str, separator: char) -> Result<i64> {
    let mut time_and_ms = input.rsplitn(2, separator);
    let millis = time_and_ms
        .next()
        .ok_or_else(|| parse_error("missing milliseconds"))?
        .parse::<i64>()
        .map_err(|_| parse_error("invalid milliseconds"))?;
    let time = time_and_ms
        .next()
        .ok_or_else(|| parse_error("missing time"))?;
    let parts = time
        .split(':')
        .map(str::parse::<i64>)
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|_| parse_error("invalid time component"))?;

    let (hours, minutes, seconds) = match parts.as_slice() {
        [minutes, seconds] => (0, *minutes, *seconds),
        [hours, minutes, seconds] => (*hours, *minutes, *seconds),
        _ => return Err(parse_error("timestamps must be MM:SS.mmm or HH:MM:SS.mmm")),
    };

    Ok(((hours * 60 + minutes) * 60 + seconds) * 1_000 + millis)
}

fn format_timestamp_srt(ms: i64) -> String {
    let ms = ms.max(0);
    let millis = ms % 1_000;
    let total_seconds = ms / 1_000;
    let seconds = total_seconds % 60;
    let total_minutes = total_seconds / 60;
    let minutes = total_minutes % 60;
    let hours = total_minutes / 60;
    format!("{hours:02}:{minutes:02}:{seconds:02},{millis:03}")
}

fn format_timestamp_webvtt(ms: i64) -> String {
    format_timestamp_srt(ms).replace(',', ".")
}

fn normalize_newlines(input: &str) -> String {
    input.replace("\r\n", "\n").replace('\r', "\n")
}

fn draw_active_subtitles_into_overlay(
    overlay: &mut RgbaFrame,
    active: &[&SubtitleEvent],
    style: &SubtitleStyle,
) {
    let width = overlay.width;
    let height = overlay.height;
    let lines = wrapped_subtitle_lines(active, width, style);
    if lines.is_empty() {
        return;
    }

    let scale = style.scale.max(1);
    let padding = style.padding;
    let text_width = lines
        .iter()
        .map(|line| measure_line_width(line, scale))
        .max()
        .unwrap_or(0);
    let line_height = glyph_height(scale);
    let text_height = line_height * lines.len() as u32
        + style
            .line_gap
            .saturating_mul(lines.len().saturating_sub(1) as u32);
    let box_width = text_width.saturating_add(padding * 2).min(width);
    let box_height = text_height.saturating_add(padding * 2).min(height);
    let x = width.saturating_sub(box_width) / 2;
    let y = height.saturating_sub(style.margin_bottom.saturating_add(box_height));

    if style.box_color[3] != 0 {
        fill_rect(overlay, x, y, box_width, box_height, style.box_color);
    }

    for (line_index, line) in lines.iter().enumerate() {
        let line_width = measure_line_width(line, scale);
        let line_x = x + box_width.saturating_sub(line_width) / 2;
        let line_y = y + padding + line_index as u32 * (line_height + style.line_gap);
        draw_text(overlay, line, line_x, line_y, style);
    }
}

fn wrapped_subtitle_lines(
    active: &[&SubtitleEvent],
    width: u32,
    style: &SubtitleStyle,
) -> Vec<String> {
    let scale = style.scale.max(1);
    let max_text_width = width
        .saturating_sub(style.padding * 4)
        .max(glyph_advance(scale));
    let mut lines = Vec::new();

    for event in active {
        for raw_line in event.text.lines() {
            wrap_line(raw_line.trim(), max_text_width, scale, &mut lines);
        }
    }

    lines
}

fn wrap_line(input: &str, max_width: u32, scale: u32, lines: &mut Vec<String>) {
    let mut current = String::new();
    for word in input.split_whitespace() {
        let candidate = if current.is_empty() {
            word.to_string()
        } else {
            format!("{current} {word}")
        };

        if measure_line_width(&candidate, scale) <= max_width {
            current = candidate;
        } else {
            if !current.is_empty() {
                lines.push(std::mem::take(&mut current));
            }
            if measure_line_width(word, scale) <= max_width {
                current = word.to_string();
            } else {
                split_long_word(word, max_width, scale, lines);
                current.clear();
            }
        }
    }

    if !current.is_empty() {
        lines.push(current);
    }
}

fn split_long_word(word: &str, max_width: u32, scale: u32, lines: &mut Vec<String>) {
    let mut current = String::new();
    for ch in word.chars() {
        let candidate = format!("{current}{ch}");
        if !current.is_empty() && measure_line_width(&candidate, scale) > max_width {
            lines.push(current);
            current = ch.to_string();
        } else {
            current = candidate;
        }
    }
    if !current.is_empty() {
        lines.push(current);
    }
}

fn fill_rect(frame: &mut RgbaFrame, x: u32, y: u32, width: u32, height: u32, color: [u8; 4]) {
    for yy in y..y.saturating_add(height).min(frame.height) {
        for xx in x..x.saturating_add(width).min(frame.width) {
            frame.set_pixel(xx, yy, color);
        }
    }
}

fn draw_text(frame: &mut RgbaFrame, text: &str, x: u32, y: u32, style: &SubtitleStyle) {
    let scale = style.scale.max(1);
    if style.outline_color[3] != 0 {
        let outline = scale.clamp(1, 3) as i32;
        for dy in -outline..=outline {
            for dx in -outline..=outline {
                if dx == 0 && dy == 0 {
                    continue;
                }
                draw_text_line(
                    frame,
                    text,
                    x as i32 + dx,
                    y as i32 + dy,
                    scale,
                    style.outline_color,
                );
            }
        }
    }
    draw_text_line(frame, text, x as i32, y as i32, scale, style.text_color);
}

fn draw_text_line(frame: &mut RgbaFrame, text: &str, x: i32, y: i32, scale: u32, color: [u8; 4]) {
    let mut cursor_x = x;
    for ch in text.chars() {
        draw_glyph(frame, ch, cursor_x, y, scale, color);
        cursor_x += glyph_advance(scale) as i32;
    }
}

fn draw_glyph(frame: &mut RgbaFrame, ch: char, x: i32, y: i32, scale: u32, color: [u8; 4]) {
    for (row_index, row) in glyph_pattern(ch).iter().enumerate() {
        for (col_index, value) in row.bytes().enumerate() {
            if value != b'1' {
                continue;
            }
            let px = x + col_index as i32 * scale as i32;
            let py = y + row_index as i32 * scale as i32;
            fill_rect_i32(frame, px, py, scale, scale, color);
        }
    }
}

fn fill_rect_i32(frame: &mut RgbaFrame, x: i32, y: i32, width: u32, height: u32, color: [u8; 4]) {
    for yy in 0..height as i32 {
        for xx in 0..width as i32 {
            let px = x + xx;
            let py = y + yy;
            if px >= 0 && py >= 0 {
                frame.set_pixel(px as u32, py as u32, color);
            }
        }
    }
}

fn measure_line_width(text: &str, scale: u32) -> u32 {
    let chars = text.chars().count() as u32;
    if chars == 0 {
        0
    } else {
        chars * glyph_advance(scale) - scale
    }
}

fn glyph_width(scale: u32) -> u32 {
    5 * scale
}

fn glyph_height(scale: u32) -> u32 {
    7 * scale
}

fn glyph_advance(scale: u32) -> u32 {
    glyph_width(scale) + scale
}

fn glyph_pattern(ch: char) -> [&'static str; 7] {
    match ch.to_ascii_uppercase() {
        'A' => [
            "01110", "10001", "10001", "11111", "10001", "10001", "10001",
        ],
        'B' => [
            "11110", "10001", "10001", "11110", "10001", "10001", "11110",
        ],
        'C' => [
            "01111", "10000", "10000", "10000", "10000", "10000", "01111",
        ],
        'D' => [
            "11110", "10001", "10001", "10001", "10001", "10001", "11110",
        ],
        'E' => [
            "11111", "10000", "10000", "11110", "10000", "10000", "11111",
        ],
        'F' => [
            "11111", "10000", "10000", "11110", "10000", "10000", "10000",
        ],
        'G' => [
            "01111", "10000", "10000", "10011", "10001", "10001", "01110",
        ],
        'H' => [
            "10001", "10001", "10001", "11111", "10001", "10001", "10001",
        ],
        'I' => [
            "11111", "00100", "00100", "00100", "00100", "00100", "11111",
        ],
        'J' => [
            "00111", "00010", "00010", "00010", "00010", "10010", "01100",
        ],
        'K' => [
            "10001", "10010", "10100", "11000", "10100", "10010", "10001",
        ],
        'L' => [
            "10000", "10000", "10000", "10000", "10000", "10000", "11111",
        ],
        'M' => [
            "10001", "11011", "10101", "10101", "10001", "10001", "10001",
        ],
        'N' => [
            "10001", "11001", "10101", "10011", "10001", "10001", "10001",
        ],
        'O' => [
            "01110", "10001", "10001", "10001", "10001", "10001", "01110",
        ],
        'P' => [
            "11110", "10001", "10001", "11110", "10000", "10000", "10000",
        ],
        'Q' => [
            "01110", "10001", "10001", "10001", "10101", "10010", "01101",
        ],
        'R' => [
            "11110", "10001", "10001", "11110", "10100", "10010", "10001",
        ],
        'S' => [
            "01111", "10000", "10000", "01110", "00001", "00001", "11110",
        ],
        'T' => [
            "11111", "00100", "00100", "00100", "00100", "00100", "00100",
        ],
        'U' => [
            "10001", "10001", "10001", "10001", "10001", "10001", "01110",
        ],
        'V' => [
            "10001", "10001", "10001", "10001", "10001", "01010", "00100",
        ],
        'W' => [
            "10001", "10001", "10001", "10101", "10101", "10101", "01010",
        ],
        'X' => [
            "10001", "10001", "01010", "00100", "01010", "10001", "10001",
        ],
        'Y' => [
            "10001", "10001", "01010", "00100", "00100", "00100", "00100",
        ],
        'Z' => [
            "11111", "00001", "00010", "00100", "01000", "10000", "11111",
        ],
        '0' => [
            "01110", "10001", "10011", "10101", "11001", "10001", "01110",
        ],
        '1' => [
            "00100", "01100", "00100", "00100", "00100", "00100", "01110",
        ],
        '2' => [
            "01110", "10001", "00001", "00010", "00100", "01000", "11111",
        ],
        '3' => [
            "11110", "00001", "00001", "01110", "00001", "00001", "11110",
        ],
        '4' => [
            "00010", "00110", "01010", "10010", "11111", "00010", "00010",
        ],
        '5' => [
            "11111", "10000", "10000", "11110", "00001", "00001", "11110",
        ],
        '6' => [
            "01110", "10000", "10000", "11110", "10001", "10001", "01110",
        ],
        '7' => [
            "11111", "00001", "00010", "00100", "01000", "01000", "01000",
        ],
        '8' => [
            "01110", "10001", "10001", "01110", "10001", "10001", "01110",
        ],
        '9' => [
            "01110", "10001", "10001", "01111", "00001", "00001", "01110",
        ],
        '!' => [
            "00100", "00100", "00100", "00100", "00100", "00000", "00100",
        ],
        '?' => [
            "01110", "10001", "00001", "00010", "00100", "00000", "00100",
        ],
        '.' => [
            "00000", "00000", "00000", "00000", "00000", "01100", "01100",
        ],
        ',' => [
            "00000", "00000", "00000", "00000", "00000", "01100", "01000",
        ],
        ':' => [
            "00000", "01100", "01100", "00000", "01100", "01100", "00000",
        ],
        ';' => [
            "00000", "01100", "01100", "00000", "01100", "01000", "10000",
        ],
        '-' => [
            "00000", "00000", "00000", "11111", "00000", "00000", "00000",
        ],
        '\'' => [
            "00100", "00100", "01000", "00000", "00000", "00000", "00000",
        ],
        '"' => [
            "01010", "01010", "01010", "00000", "00000", "00000", "00000",
        ],
        '(' => [
            "00010", "00100", "01000", "01000", "01000", "00100", "00010",
        ],
        ')' => [
            "01000", "00100", "00010", "00010", "00010", "00100", "01000",
        ],
        '/' => [
            "00001", "00010", "00010", "00100", "01000", "01000", "10000",
        ],
        ' ' => [
            "00000", "00000", "00000", "00000", "00000", "00000", "00000",
        ],
        _ => [
            "01110", "10001", "00001", "00010", "00100", "00000", "00100",
        ],
    }
}

fn parse_error(message: impl Into<String>) -> Error {
    Error::Parse {
        format: "subtitle",
        message: message.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        SubtitleEvent, SubtitleFormat, SubtitleStyle, active_events_at, burn_subtitles_onto_frame,
        parse_subtitles, render_subtitle_overlay, shift_events, write_srt, write_webvtt,
    };
    use crate::frame::RgbaFrame;

    #[test]
    fn parses_shifts_and_writes_srt() {
        let input = "1\n00:00:01,000 --> 00:00:02,500\nhello\n\n";
        let events = parse_subtitles(SubtitleFormat::Srt, input).unwrap();
        let shifted = shift_events(&events, 500);
        let output = write_srt(&shifted);

        assert_eq!(shifted[0].start_ms, 1_500);
        assert!(output.contains("00:00:01,500 --> 00:00:03,000"));
    }

    #[test]
    fn parses_webvtt() {
        let input = "WEBVTT\n\n00:01.000 --> 00:02.000\nhello\n";
        let events = parse_subtitles(SubtitleFormat::WebVtt, input).unwrap();

        assert_eq!(events[0].start_ms, 1_000);
        assert_eq!(events[0].end_ms, 2_000);
    }

    #[test]
    fn parses_crlf_srt_and_writes_webvtt() {
        let input = "1\r\n00:00:01,000 --> 00:00:02,000\r\nhello\r\n\r\n";
        let events = parse_subtitles(SubtitleFormat::Srt, input).unwrap();
        let output = write_webvtt(&events);

        assert_eq!(events.len(), 1);
        assert!(output.starts_with("WEBVTT\n\n"));
        assert!(output.contains("00:00:01.000 --> 00:00:02.000"));
    }

    #[test]
    fn finds_active_events_at_timestamp() {
        let events = vec![
            SubtitleEvent::new(0, 1_000, "first").unwrap(),
            SubtitleEvent::new(500, 1_500, "second").unwrap(),
        ];

        let active = active_events_at(&events, 750);

        assert_eq!(active.len(), 2);
        assert_eq!(active[0].text, "first");
        assert!(active_events_at(&events, 1_500).is_empty());
    }

    #[test]
    fn renders_and_burns_subtitles_onto_frame() {
        let events = vec![SubtitleEvent::new(0, 2_000, "Hi!").unwrap()];
        let style = SubtitleStyle {
            scale: 1,
            margin_bottom: 2,
            padding: 2,
            ..SubtitleStyle::default()
        };
        let mut frame = RgbaFrame::solid(96, 48, [10, 20, 30, 255]);

        let result = burn_subtitles_onto_frame(&mut frame, &events, 500, &style).unwrap();

        assert_eq!(result.active_events, 1);
        assert!(result.bounds.is_some());
        assert!(frame.data.chunks_exact(4).any(|pixel| pixel[0] > 200));
    }

    #[test]
    fn inactive_subtitle_render_is_noop() {
        let events = vec![SubtitleEvent::new(0, 1_000, "gone").unwrap()];
        let result =
            render_subtitle_overlay(64, 32, &events, 1_500, &SubtitleStyle::default()).unwrap();

        assert_eq!(result.active_events, 0);
        assert_eq!(result.bounds, None);
    }
}
