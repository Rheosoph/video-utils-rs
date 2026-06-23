use video_utils_rs::{
    CodecId, EncodedPacket, Error, RgbaFrame, TimeBase, UnsupportedVideoDecoder,
    UnsupportedVideoEncoder, VideoDecoder, VideoEncoder,
};

#[test]
fn compressed_video_decode_backends_are_explicit_when_missing() {
    let codecs = [
        CodecId::AV1,
        CodecId::VP8,
        CodecId::VP9,
        #[cfg(not(feature = "codec-h264-rust"))]
        CodecId::H264,
        #[cfg(not(feature = "codec-h265-rust"))]
        CodecId::H265,
    ];

    for codec in codecs {
        let packet = EncodedPacket::new(
            1,
            codec.clone(),
            0,
            1,
            TimeBase::milliseconds(),
            vec![0, 1, 2],
        );
        let mut decoder =
            UnsupportedVideoDecoder::new(codec, "no Rust-native compressed decoder compiled");

        assert!(matches!(
            decoder.decode_packet(&packet),
            Err(Error::Unsupported {
                operation: "video decode",
                reason: "no Rust-native compressed decoder compiled"
            })
        ));
    }
}

#[test]
fn compressed_video_encode_backends_are_explicit_when_missing() {
    let frame = RgbaFrame::solid(16, 16, [16, 32, 64, 255]);
    #[cfg(feature = "codec-av1-rust")]
    let codecs = vec![CodecId::H264, CodecId::H265, CodecId::VP8, CodecId::VP9];
    #[cfg(not(feature = "codec-av1-rust"))]
    let codecs = vec![
        CodecId::H264,
        CodecId::H265,
        CodecId::VP8,
        CodecId::VP9,
        CodecId::AV1,
    ];

    for codec in codecs {
        let mut encoder =
            UnsupportedVideoEncoder::new(codec, "no Rust-native compressed encoder compiled");

        assert!(matches!(
            encoder.encode_frame(&frame, 0),
            Err(Error::Unsupported {
                operation: "video encode",
                reason: "no Rust-native compressed encoder compiled"
            })
        ));
    }
}

#[cfg(all(feature = "codec-av1-rust", feature = "containers"))]
#[test]
fn rav1e_av1_encoder_outputs_muxable_webm_packets() {
    use video_utils_rs::{
        ContainerFormat, MediaInfo, MediaType, Rav1eAv1Encoder, StreamInfo, demux_matroska_bytes,
        mux_matroska_bytes,
    };

    let time_base = TimeBase::new(1, 30).unwrap();
    let mut encoder = Rav1eAv1Encoder::new(1, 16, 16, time_base, 1).unwrap();
    let mut packets = Vec::new();
    packets.extend(
        encoder
            .encode_frame(&RgbaFrame::solid(16, 16, [32, 96, 160, 255]), 0)
            .unwrap(),
    );
    packets.extend(
        encoder
            .encode_frame(&RgbaFrame::solid(16, 16, [160, 96, 32, 255]), 1)
            .unwrap(),
    );
    packets.extend(encoder.finish().unwrap());

    assert!(!packets.is_empty());
    assert!(packets.iter().all(|packet| packet.codec == CodecId::AV1));
    assert!(packets.iter().all(|packet| !packet.data.is_empty()));

    let mut media = MediaInfo {
        duration_seconds: Some(2.0 / 30.0),
        ..Default::default()
    };
    let mut stream =
        StreamInfo::new(1, MediaType::Video, CodecId::AV1, time_base).with_dimensions(16, 16);
    stream.codec_config = Some(encoder.codec_config());
    media.push_stream(stream);

    let bytes = mux_matroska_bytes(ContainerFormat::WebM, &media, &packets).unwrap();
    let demuxed = demux_matroska_bytes(ContainerFormat::WebM, &bytes).unwrap();

    assert_eq!(demuxed.format, ContainerFormat::WebM);
    assert_eq!(demuxed.media.streams[0].codec, CodecId::AV1);
    assert_eq!(demuxed.packets.len(), packets.len());
    assert!(demuxed.packets.iter().all(|packet| !packet.data.is_empty()));
}

#[cfg(all(feature = "codec-av1-rust", feature = "containers"))]
#[test]
fn object_store_transform_can_encode_raw_rgba_to_av1_webm() {
    use bytes::Bytes;
    use futures::executor::block_on;
    use object_store::{ObjectStore, PutPayload, memory::InMemory, path::Path};
    use video_utils_rs::{
        ColorFilter, ContainerFormat, FrameTransform, FrameTransformPipeline, MediaInfo, MediaType,
        Rav1eAv1Encoder, RawRgbaVideoDecoder, StreamInfo, demux_matroska_bytes, mux_matroska_bytes,
        read_object_bytes, transform_object_video_same_store,
    };

    block_on(async {
        let store = InMemory::new();
        let source = Path::from("compressed/raw-input.mkv");
        let target = Path::from("compressed/av1-output.webm");
        let time_base = TimeBase::new(1, 30).unwrap();
        let mut media = MediaInfo {
            duration_seconds: Some(1.0 / 30.0),
            ..Default::default()
        };
        media.push_stream(
            StreamInfo::new(1, MediaType::Video, CodecId::RawVideo, time_base)
                .with_dimensions(16, 16),
        );

        let frame = RgbaFrame::solid(16, 16, [80, 140, 200, 255]);
        let packet = EncodedPacket::new(
            1,
            CodecId::RawVideo,
            0,
            1,
            time_base,
            Bytes::copy_from_slice(&frame.data),
        )
        .with_keyframe(true);
        let source_bytes =
            mux_matroska_bytes(ContainerFormat::Matroska, &media, &[packet]).unwrap();
        store
            .put(&source, PutPayload::from_bytes(source_bytes))
            .await
            .unwrap();

        let pipeline =
            FrameTransformPipeline::new().with(FrameTransform::ColorFilter(ColorFilter::sepia()));
        let mut encoder = Rav1eAv1Encoder::new(1, 16, 16, time_base, 1).unwrap();
        let job = video_utils_rs::ObjectVideoTransformJob::new(pipeline)
            .with_output_video_codec_config(encoder.codec_config());
        let mut decoder = RawRgbaVideoDecoder::new(16, 16);

        let report = transform_object_video_same_store(
            &store,
            &source,
            &target,
            &job,
            &mut decoder,
            &mut encoder,
        )
        .await
        .unwrap();

        assert_eq!(report.target_format, ContainerFormat::WebM);
        assert_eq!(report.decoded_frames, 1);
        assert!(report.encoded_video_packets >= 1);

        let output = read_object_bytes(&store, &target).await.unwrap();
        let demuxed = demux_matroska_bytes(ContainerFormat::WebM, &output).unwrap();
        assert_eq!(demuxed.media.streams[0].codec, CodecId::AV1);
        assert_eq!(demuxed.media.streams[0].width, Some(16));
        assert_eq!(demuxed.media.streams[0].height, Some(16));
        assert!(demuxed.packets.iter().all(|packet| !packet.data.is_empty()));
    });
}
