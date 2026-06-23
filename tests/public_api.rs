use video_utils_rs::{
    AudioFrame, CodecId, CropRect, EncodedPacket, Error, FadeShape, HlsPlaylist, HlsSegment,
    RgbaFrame, SubtitleFormat, SubtitleStyle, TimeBase, active_events_at, apply_gain,
    burn_subtitles_onto_frame, concat_copy, detect_silence, fade, filter_track, mix,
    normalize_peak, parse_subtitles, plan_keyframe_segments, select_keyframe_range, shift_events,
    validate_concat_compatible, validate_monotonic_by_track, waveform_peaks, write_srt,
    write_webvtt,
};

fn packet(track_id: u32, codec: CodecId, pts: i64, duration: i64, key: bool) -> EncodedPacket {
    EncodedPacket::new(
        track_id,
        codec,
        pts,
        duration,
        TimeBase::milliseconds(),
        vec![track_id as u8, pts as u8],
    )
    .with_keyframe(key)
}

#[test]
fn packet_workflow_normalizes_trims_filters_and_concats() {
    let mut first = vec![
        packet(1, CodecId::H264, -1_000, 1_000, true),
        packet(1, CodecId::H264, 0, 1_000, false),
        packet(2, CodecId::Aac, -960, 960, true),
        packet(2, CodecId::Aac, 0, 960, true),
    ];
    let second = vec![
        packet(1, CodecId::H264, 500, 1_000, true),
        packet(1, CodecId::H264, 1_500, 1_000, false),
        packet(2, CodecId::Aac, 240, 960, true),
    ];

    video_utils_rs::normalize_timestamps(&mut first).unwrap();
    let video_only = filter_track(&first, 1);
    let range = select_keyframe_range(&video_only, 1, 0.4, 1.6).unwrap();
    let merged = concat_copy(&[&first, &second]).unwrap();

    assert_eq!(first[0].pts, 0);
    assert_eq!(first[2].pts, 0);
    assert_eq!(video_only.len(), 2);
    assert_eq!(range.start, 0);
    assert!(merged.iter().any(|packet| packet.track_id == 2));
    validate_monotonic_by_track(&merged).unwrap();
}

#[test]
fn packet_validation_rejects_codec_and_timestamp_mismatches() {
    let good = vec![packet(1, CodecId::H264, 0, 1_000, true)];
    let wrong_codec = vec![packet(1, CodecId::H265, 0, 1_000, true)];
    let non_monotonic = vec![
        packet(1, CodecId::H264, 1_000, 1_000, true),
        packet(1, CodecId::H264, 0, 1_000, false),
    ];

    assert!(matches!(
        validate_concat_compatible(&[&good, &wrong_codec]),
        Err(Error::CodecMismatch { .. })
    ));
    assert!(validate_monotonic_by_track(&non_monotonic).is_err());
}

#[test]
fn audio_workflow_applies_gain_fade_mix_waveform_and_silence() {
    let mut voice = AudioFrame::new(48_000, 2, 0, vec![0.0, 0.0, 0.5, -0.5, 0.25, -0.25]).unwrap();
    let bed = AudioFrame::new(48_000, 2, 1, vec![0.1, 0.1, 0.1, 0.1]).unwrap();

    apply_gain(&mut voice, 2.0);
    normalize_peak(&mut voice, 0.8).unwrap();
    fade(&mut voice, 1, 1, FadeShape::EqualPower);
    let mixed = mix(&[voice.clone(), bed]).unwrap();
    let peaks = waveform_peaks(&mixed, 3).unwrap();
    let silence = detect_silence(&voice, -60.0, 1, 1).unwrap();

    assert_eq!(mixed.sample_rate, 48_000);
    assert_eq!(mixed.channels, 2);
    assert_eq!(peaks.len(), 3);
    assert_eq!(silence[0].start_sample, 0);
}

#[test]
fn frame_workflow_crops_pads_resizes_flips_overlays_and_detects_bars() {
    let mut frame = RgbaFrame::solid(6, 6, [0, 0, 0, 255]);
    for y in 2..4 {
        for x in 2..4 {
            frame.set_pixel(x, y, [200, 10, 20, 255]);
        }
    }

    let cropped = frame.crop(CropRect::new(2, 2, 2, 2)).unwrap();
    let padded = cropped.pad(6, 6, 2, 2, [0, 0, 0, 255]).unwrap();
    let resized = padded.resize_nearest(12, 12).unwrap();
    let flipped = resized.flip_horizontal().flip_vertical();
    let mut composited = RgbaFrame::solid(12, 12, [0, 0, 0, 255]);
    composited.overlay(&flipped, 0, 0);
    let bars = composited.detect_black_bars(5, 1.0);

    assert_eq!(cropped.width, 2);
    assert_eq!(resized.width, 12);
    assert!(bars.top >= 3);
    assert!(composited.data.chunks_exact(4).any(|pixel| pixel[0] == 200));
}

#[test]
fn subtitle_workflow_parses_shifts_writes_and_burns_into_frame() {
    let source = "1\r\n00:00:01,000 --> 00:00:02,500\r\nHello video!\r\n\r\n";
    let events = parse_subtitles(SubtitleFormat::Srt, source).unwrap();
    let shifted = shift_events(&events, 500);
    let vtt = write_webvtt(&shifted);
    let srt = write_srt(&shifted);
    let active = active_events_at(&shifted, 1_750);
    let mut frame = RgbaFrame::solid(160, 90, [8, 12, 16, 255]);
    let style = SubtitleStyle {
        scale: 2,
        margin_bottom: 8,
        padding: 4,
        ..SubtitleStyle::default()
    };

    let rendered = burn_subtitles_onto_frame(&mut frame, &shifted, 1_750, &style).unwrap();

    assert_eq!(active.len(), 1);
    assert!(vtt.contains("00:00:01.500 --> 00:00:03.000"));
    assert!(srt.contains("Hello video!"));
    assert_eq!(rendered.active_events, 1);
    assert!(frame.data.chunks_exact(4).any(|pixel| pixel[0] > 200));
}

#[test]
fn streaming_workflow_plans_keyframe_segments_and_writes_playlist() {
    let packets: Vec<_> = (0..8)
        .map(|index| packet(1, CodecId::H264, index * 1_000, 1_000, index % 2 == 0))
        .collect();
    let slices = plan_keyframe_segments(&packets, 1, 2.0).unwrap();
    let playlist = HlsPlaylist::vod(
        slices
            .iter()
            .enumerate()
            .map(|(index, slice)| {
                let duration = packets[slice.end - 1].end_pts() - packets[slice.start].pts;
                HlsSegment::new(duration as f64 / 1_000.0, format!("seg{index}.m4s"))
            })
            .collect(),
    );
    let m3u8 = playlist.to_m3u8();

    assert_eq!(slices.len(), 4);
    assert!(m3u8.contains("#EXT-X-INDEPENDENT-SEGMENTS"));
    assert!(m3u8.contains("seg3.m4s"));
}
