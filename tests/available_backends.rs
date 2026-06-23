use video_utils_rs::{
    CodecId, EncodedPacket, Error, PacketCopyCodec, SubtitleTextCodec, TimeBase,
    UnsupportedVideoDecoder,
};
use video_utils_rs::{Decoder, Encoder, VideoDecoder};

#[test]
fn core_backends_are_usable_from_the_public_api() {
    let mut subtitles = SubtitleTextCodec::srt();
    let events = subtitles
        .decode(b"1\n00:00:00,000 --> 00:00:01,000\nhello\n\n")
        .unwrap();
    let encoded = subtitles.encode(events.as_slice()).unwrap();
    assert!(String::from_utf8(encoded).unwrap().contains("hello"));

    let packet = EncodedPacket::new(
        1,
        CodecId::H264,
        0,
        1,
        TimeBase::milliseconds(),
        vec![0, 0, 1],
    );
    let mut packet_copy = PacketCopyCodec::new(CodecId::H264);
    assert_eq!(packet_copy.decode(&packet).unwrap(), packet);
}

#[test]
fn unsupported_video_backend_makes_missing_video_codecs_explicit() {
    let packet = EncodedPacket::new(
        1,
        CodecId::H264,
        0,
        1,
        TimeBase::milliseconds(),
        vec![0, 0, 1],
    );
    let mut decoder = UnsupportedVideoDecoder::new(CodecId::H264, "no video backend compiled");

    assert!(matches!(
        decoder.decode_packet(&packet),
        Err(Error::Unsupported {
            operation: "video decode",
            reason: "no video backend compiled"
        })
    ));
}

#[cfg(feature = "audio-io")]
#[test]
fn wav_pcm_backend_round_trips_audio_frames() {
    use video_utils_rs::{AudioFrame, WavPcmDecoder, WavPcmEncoder};

    let samples = vec![0.0, 0.1, -0.1, 0.5, -0.5, 0.9, -0.9, 0.0];
    let frame = AudioFrame::new(48_000, 2, 0, samples).unwrap();
    let mut encoder = WavPcmEncoder::new();
    let mut decoder = WavPcmDecoder::new();

    let bytes = encoder.encode(&frame).unwrap();
    let decoded = decoder.decode(&bytes).unwrap();

    assert_eq!(decoded.sample_rate, 48_000);
    assert_eq!(decoded.channels, 2);
    assert_eq!(decoded.sample_frames(), frame.sample_frames());
    assert!((decoded.samples_f32_interleaved[3] - 0.5).abs() < 0.001);
}

#[cfg(feature = "audio-io")]
#[test]
fn symphonia_backend_decodes_audio_file_bytes() {
    use video_utils_rs::{AudioFrame, CodecDescriptor, SymphoniaAudioDecoder, WavPcmEncoder};

    let frame = AudioFrame::new(44_100, 1, 0, vec![0.0, 0.25, -0.25, 0.5, -0.5]).unwrap();
    let mut wav = WavPcmEncoder::new();
    let bytes = wav.encode(&frame).unwrap();
    let mut decoder = SymphoniaAudioDecoder::for_codec_with_extension(CodecId::Pcm, "wav");

    let decoded = decoder.decode(&bytes).unwrap();
    let sample_frames: usize = decoded.iter().map(|frame| frame.sample_frames()).sum();

    assert_eq!(decoder.codec_id(), CodecId::Pcm);
    assert_eq!(decoded[0].sample_rate, 44_100);
    assert_eq!(decoded[0].channels, 1);
    assert_eq!(sample_frames, 5);
}

#[cfg(feature = "image-io")]
#[test]
fn png_backend_round_trips_rgba_frames() {
    use video_utils_rs::{ImageRgbaDecoder, ImageRgbaEncoder, ImageStillFormat, RgbaFrame};

    let mut frame = RgbaFrame::solid(4, 4, [0, 0, 0, 0]);
    frame.set_pixel(0, 0, [255, 0, 0, 255]);
    frame.set_pixel(3, 3, [0, 128, 255, 64]);
    let mut encoder = ImageRgbaEncoder::png();
    let mut decoder = ImageRgbaDecoder::with_format(ImageStillFormat::Png);

    let bytes = encoder.encode(&frame).unwrap();
    let decoded = decoder.decode(&bytes).unwrap();

    assert_eq!(decoded.width, 4);
    assert_eq!(decoded.height, 4);
    assert_eq!(decoded.pixel(0, 0), Some([255, 0, 0, 255]));
    assert_eq!(decoded.pixel(3, 3), Some([0, 128, 255, 64]));
}

#[cfg(feature = "image-io")]
#[test]
fn jpeg_backend_encodes_and_decodes_rgb_pixels() {
    use video_utils_rs::{ImageRgbaDecoder, ImageRgbaEncoder, ImageStillFormat, RgbaFrame};

    let frame = RgbaFrame::solid(8, 8, [30, 80, 120, 32]);
    let mut encoder = ImageRgbaEncoder::jpeg();
    let mut decoder = ImageRgbaDecoder::with_format(ImageStillFormat::Jpeg);

    let bytes = encoder.encode(&frame).unwrap();
    let decoded = decoder.decode(&bytes).unwrap();

    assert_eq!(decoded.width, 8);
    assert_eq!(decoded.height, 8);
    assert_eq!(decoded.pixel(0, 0).unwrap()[3], 255);
}

#[cfg(feature = "image-io")]
#[test]
fn gif_and_webp_backends_encode_and_decode_frames() {
    use video_utils_rs::{ImageRgbaDecoder, ImageRgbaEncoder, ImageStillFormat, RgbaFrame};

    let frame = RgbaFrame::solid(8, 8, [30, 80, 120, 255]);
    for (mut encoder, format) in [
        (ImageRgbaEncoder::gif(), ImageStillFormat::Gif),
        (ImageRgbaEncoder::webp(), ImageStillFormat::WebP),
    ] {
        let bytes = encoder.encode(&frame).unwrap();
        let decoded = ImageRgbaDecoder::with_format(format)
            .decode(&bytes)
            .unwrap();

        assert_eq!(decoded.width, 8);
        assert_eq!(decoded.height, 8);
    }
}

#[cfg(feature = "audio-io")]
#[test]
fn registry_reports_audio_file_backends_when_compiled() {
    use video_utils_rs::{CodecImplementationKind, CodecRegistry};

    let registry = CodecRegistry::builtin();

    assert!(registry.supports_decode(&CodecId::Pcm, CodecImplementationKind::AudioFile));
    assert!(registry.supports_encode(&CodecId::Pcm, CodecImplementationKind::AudioFile));
    assert!(registry.supports_decode(&CodecId::Mp3, CodecImplementationKind::AudioFile));
    assert!(registry.supports_decode(&CodecId::Vorbis, CodecImplementationKind::AudioFile));
    assert!(registry.supports_decode(&CodecId::Adpcm, CodecImplementationKind::AudioFile));
    assert!(registry.supports_decode(&CodecId::Flac, CodecImplementationKind::AudioFile));
    assert!(!registry.supports_encode(&CodecId::Flac, CodecImplementationKind::AudioFile));
    assert!(!registry.supports_decode(&CodecId::Opus, CodecImplementationKind::AudioFile));
}

#[cfg(feature = "image-io")]
#[test]
fn registry_reports_image_backends_when_compiled() {
    use video_utils_rs::{CodecImplementationKind, CodecRegistry};

    let registry = CodecRegistry::builtin();

    assert!(registry.supports_decode(&CodecId::Png, CodecImplementationKind::ImageStill));
    assert!(registry.supports_encode(&CodecId::Jpeg, CodecImplementationKind::ImageStill));
    assert!(registry.supports_decode(&CodecId::Gif, CodecImplementationKind::ImageStill));
    assert!(registry.supports_encode(&CodecId::WebP, CodecImplementationKind::ImageStill));
    assert!(registry.supports_decode(&CodecId::Avif, CodecImplementationKind::ImageStill));
}

#[cfg(any(
    feature = "codec-h264-rust",
    feature = "codec-h265-rust",
    feature = "codec-av1-rust"
))]
#[test]
fn rust_software_backend_reports_compiled_codec_lanes() {
    use video_utils_rs::{BackendKind, recommended_backends_for_current_target};

    let rust_backend = recommended_backends_for_current_target()
        .into_iter()
        .find(|backend| backend.kind == BackendKind::RustSoftware)
        .unwrap();

    #[cfg(feature = "codec-h264-rust")]
    assert!(rust_backend.decodes.contains(&CodecId::H264));
    #[cfg(feature = "codec-h265-rust")]
    assert!(rust_backend.decodes.contains(&CodecId::H265));
    #[cfg(feature = "codec-av1-rust")]
    assert!(rust_backend.encodes.contains(&CodecId::AV1));
    #[cfg(not(feature = "codec-av1-rust"))]
    assert!(!rust_backend.encodes.contains(&CodecId::AV1));
}
