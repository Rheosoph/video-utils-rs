use video_utils_rs::{
    CodecDescriptor, CodecId, CodecImplementationKind, CodecRegistry, Decoder, EncodedPacket,
    Encoder, Error, PacketCopyCodec, SubtitleEvent, SubtitleTextCodec, TimeBase, VideoDecoder,
    VideoEncoder,
};

#[test]
fn subtitle_text_codecs_use_decoder_encoder_traits() {
    let mut srt = SubtitleTextCodec::srt();
    let events = srt
        .decode(b"1\n00:00:01,000 --> 00:00:02,000\nhello\n\n")
        .unwrap();
    let encoded = srt.encode(events.as_slice()).unwrap();

    assert_eq!(srt.codec_id(), CodecId::Srt);
    assert_eq!(events[0].text, "hello");
    assert!(
        String::from_utf8(encoded)
            .unwrap()
            .contains("00:00:01,000 --> 00:00:02,000")
    );

    let mut webvtt = SubtitleTextCodec::webvtt();
    let events = webvtt
        .decode(b"WEBVTT\n\n00:00:01.000 --> 00:00:02.000\nhello\n\n")
        .unwrap();
    let encoded = webvtt.encode(events.as_slice()).unwrap();

    assert_eq!(webvtt.codec_id(), CodecId::WebVtt);
    assert!(String::from_utf8(encoded).unwrap().starts_with("WEBVTT"));
}

#[test]
fn packet_copy_codec_is_a_packet_read_write_adapter_not_a_frame_decoder() {
    let packet = EncodedPacket::new(
        7,
        CodecId::H264,
        90_000,
        3_000,
        TimeBase::new(1, 90_000).unwrap(),
        vec![0, 0, 1, 9],
    )
    .with_keyframe(true);
    let mut codec = PacketCopyCodec::new(CodecId::H264);

    let decoded = codec.decode(&packet).unwrap();
    let encoded = codec.encode(&decoded).unwrap();

    assert_eq!(decoded, packet);
    assert_eq!(encoded, packet);
}

#[test]
fn packet_copy_codec_rejects_mismatched_codec_ids() {
    let packet = EncodedPacket::new(
        7,
        CodecId::H265,
        0,
        1,
        TimeBase::milliseconds(),
        vec![0, 0, 1],
    );
    let mut codec = PacketCopyCodec::new(CodecId::H264);

    assert!(matches!(
        codec.decode(&packet),
        Err(Error::CodecMismatch { expected, actual })
            if expected == CodecId::H264 && actual == CodecId::H265
    ));
}

#[test]
fn registry_distinguishes_text_packet_copy_and_backend_support() {
    let registry = CodecRegistry::builtin();

    assert!(registry.supports_decode(&CodecId::Srt, CodecImplementationKind::TextSubtitle));
    assert!(registry.supports_encode(&CodecId::WebVtt, CodecImplementationKind::TextSubtitle));
    assert!(registry.supports_decode(&CodecId::H264, CodecImplementationKind::PacketCopy));
    assert!(registry.supports_decode(&CodecId::VP8, CodecImplementationKind::PacketCopy));
    assert!(registry.supports_encode(&CodecId::Aac, CodecImplementationKind::PacketCopy));
    assert!(registry.supports_encode(&CodecId::Mp3, CodecImplementationKind::PacketCopy));
    #[cfg(not(feature = "codec-h264-rust"))]
    assert!(!registry.supports_decode(&CodecId::H264, CodecImplementationKind::Backend));
    #[cfg(feature = "codec-h264-rust")]
    assert!(registry.supports_decode(&CodecId::H264, CodecImplementationKind::Backend));
    #[cfg(not(feature = "codec-h265-rust"))]
    assert!(!registry.supports_decode(&CodecId::H265, CodecImplementationKind::Backend));
    #[cfg(feature = "codec-h265-rust")]
    assert!(registry.supports_decode(&CodecId::H265, CodecImplementationKind::Backend));
    #[cfg(not(feature = "codec-av1-rust"))]
    assert!(!registry.supports_encode(&CodecId::AV1, CodecImplementationKind::Backend));
    #[cfg(feature = "codec-av1-rust")]
    assert!(registry.supports_encode(&CodecId::AV1, CodecImplementationKind::Backend));
}

#[test]
fn video_codec_traits_are_available_for_backend_implementations() {
    struct CountingVideoCodec;

    impl CodecDescriptor for CountingVideoCodec {
        fn name(&self) -> &'static str {
            "test-video-codec"
        }

        fn codec_id(&self) -> CodecId {
            CodecId::H264
        }
    }

    impl VideoDecoder for CountingVideoCodec {
        fn decode_packet(
            &mut self,
            packet: &EncodedPacket,
        ) -> video_utils_rs::Result<Vec<video_utils_rs::RgbaFrame>> {
            if packet.codec != CodecId::H264 {
                return Err(Error::CodecMismatch {
                    expected: CodecId::H264,
                    actual: packet.codec.clone(),
                });
            }
            Ok(vec![video_utils_rs::RgbaFrame::solid(2, 2, [1, 2, 3, 255])])
        }
    }

    impl VideoEncoder for CountingVideoCodec {
        fn encode_frame(
            &mut self,
            _frame: &video_utils_rs::RgbaFrame,
            pts: i64,
        ) -> video_utils_rs::Result<Vec<EncodedPacket>> {
            Ok(vec![EncodedPacket::new(
                1,
                CodecId::H264,
                pts,
                1,
                TimeBase::milliseconds(),
                vec![1],
            )])
        }
    }

    let mut codec = CountingVideoCodec;
    let packet = EncodedPacket::new(1, CodecId::H264, 0, 1, TimeBase::milliseconds(), vec![1]);
    let frames = codec.decode_packet(&packet).unwrap();
    let packets = codec.encode_frame(&frames[0], 123).unwrap();

    assert_eq!(frames[0].width, 2);
    assert_eq!(packets[0].pts, 123);
}

#[test]
fn subtitle_text_encoder_accepts_event_slices() {
    let events = vec![SubtitleEvent::new(0, 1_000, "slice").unwrap()];
    let mut codec = SubtitleTextCodec::srt();

    let encoded = codec.encode(events.as_slice()).unwrap();

    assert!(String::from_utf8(encoded).unwrap().contains("slice"));
}
