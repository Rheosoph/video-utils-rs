#![cfg(feature = "containers")]

use bytes::Bytes;
use futures::executor::block_on;
use object_store::{ObjectStore, PutPayload, memory::InMemory, path::Path};
use video_utils_rs::{
    CodecId, ColorFilter, ContainerFormat, EncodedPacket, FrameTransform, FrameTransformPipeline,
    MediaInfo, MediaType, RawRgbaVideoDecoder, RawRgbaVideoEncoder, RgbaFrame, StreamInfo,
    TimeBase, VideoDecoder, demux_matroska_bytes, mux_matroska_bytes, read_object_bytes,
    transform_object_video_same_store,
};

#[test]
fn raw_rgba_transform_job_filters_video_frames_in_object_store() {
    block_on(async {
        let store = InMemory::new();
        let source = Path::from("raw/input.mkv");
        let target = Path::from("raw/output.mkv");
        let time_base = TimeBase::milliseconds();
        let mut media = MediaInfo {
            duration_seconds: Some(0.033),
            ..Default::default()
        };
        media.push_stream(
            StreamInfo::new(1, MediaType::Video, CodecId::RawVideo, time_base)
                .with_dimensions(2, 1),
        );
        let input_frame =
            RgbaFrame::new(2, 1, 8, vec![100, 150, 200, 255, 10, 20, 30, 255]).unwrap();
        let packet = EncodedPacket::new(
            1,
            CodecId::RawVideo,
            0,
            33,
            time_base,
            Bytes::copy_from_slice(&input_frame.data),
        )
        .with_keyframe(true);
        let bytes = mux_matroska_bytes(ContainerFormat::Matroska, &media, &[packet]).unwrap();
        store
            .put(&source, PutPayload::from_bytes(bytes))
            .await
            .unwrap();

        let pipeline = FrameTransformPipeline::new()
            .with(FrameTransform::ColorFilter(ColorFilter::grayscale()));
        let job = video_utils_rs::ObjectVideoTransformJob::new(pipeline);
        let mut decoder = RawRgbaVideoDecoder::new(2, 1);
        let mut encoder = RawRgbaVideoEncoder::new(1, time_base, 33);

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

        assert_eq!(report.source_format, ContainerFormat::Matroska);
        assert_eq!(report.target_format, ContainerFormat::Matroska);
        assert_eq!(report.decoded_frames, 1);
        assert_eq!(report.encoded_video_packets, 1);

        let output = read_object_bytes(&store, &target).await.unwrap();
        let demuxed = demux_matroska_bytes(ContainerFormat::Matroska, &output).unwrap();
        assert_eq!(demuxed.media.streams[0].codec, CodecId::RawVideo);
        let mut output_decoder = RawRgbaVideoDecoder::new(2, 1);
        let frames = output_decoder.decode_packet(&demuxed.packets[0]).unwrap();
        assert_eq!(frames[0].pixel(0, 0), Some([143, 143, 143, 255]));
        assert_eq!(frames[0].pixel(1, 0), Some([19, 19, 19, 255]));
    });
}
