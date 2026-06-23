#![cfg(feature = "containers")]

use bytes::Bytes;
use futures::executor::block_on;
use object_store::{ObjectStore, PutPayload, memory::InMemory, path::Path};
use video_utils_rs::{
    AudioFrame, CodecId, ColorFilter, ContainerFormat, EncodedPacket, Error, FrameTransform,
    FrameTransformPipeline, MediaInfo, MediaType, ObjectTranscodeJob, ObjectTranscodeOperation,
    ObjectVideoTransformJob, PcmEncoding, RawRgbaVideoDecoder, RawRgbaVideoEncoder, RgbaFrame,
    StreamInfo, TimeBase, VideoDecoder, decode_pcm_packet, demux_aiff_bytes, demux_matroska_bytes,
    encode_pcm_packet, mux_matroska_bytes, mux_wav_bytes, read_object_bytes, set_pcm_tags,
    transcode_object_between_stores, transcode_object_same_store,
};

#[test]
fn transcode_job_uses_same_store_copy_when_formats_match() {
    block_on(async {
        let store = InMemory::new();
        let source = Path::from("clips/input.mkv");
        let target = Path::from("clips/output.mkv");
        let bytes = Bytes::from_static(b"same-container object body");
        store
            .put(&source, PutPayload::from_bytes(bytes.clone()))
            .await
            .unwrap();

        let report = transcode_object_same_store(
            &store,
            &source,
            &target,
            &ObjectTranscodeJob::new(),
            None,
            None,
        )
        .await
        .unwrap();

        assert_eq!(report.operation, ObjectTranscodeOperation::SameStoreCopy);
        assert_eq!(report.source_format, ContainerFormat::Matroska);
        assert_eq!(report.target_format, ContainerFormat::Matroska);
        assert_eq!(report.bytes_written, bytes.len() as u64);
        assert_eq!(read_object_bytes(&store, &target).await.unwrap(), bytes);
    });
}

#[test]
fn transcode_job_uses_cross_store_byte_copy_when_formats_match() {
    block_on(async {
        let source_store = InMemory::new();
        let target_store = InMemory::new();
        let source = Path::from("clips/input.webm");
        let target = Path::from("clips/output.webm");
        let bytes = Bytes::from_static(b"cross-store media bytes");
        source_store
            .put(&source, PutPayload::from_bytes(bytes.clone()))
            .await
            .unwrap();

        let report = transcode_object_between_stores(
            &source_store,
            &source,
            &target_store,
            &target,
            &ObjectTranscodeJob::new(),
            None,
            None,
        )
        .await
        .unwrap();

        assert_eq!(
            report.operation,
            ObjectTranscodeOperation::CrossStoreByteCopy
        );
        assert_eq!(report.source_format, ContainerFormat::WebM);
        assert_eq!(report.target_format, ContainerFormat::WebM);
        assert_eq!(
            read_object_bytes(&target_store, &target).await.unwrap(),
            bytes
        );
    });
}

#[test]
fn transcode_job_packet_copy_remuxes_when_no_decode_stage_is_needed() {
    block_on(async {
        let store = InMemory::new();
        let source = Path::from("audio/source.wav");
        let target = Path::from("audio/output.aiff");
        let (media, packets) = pcm_media_packets();
        let wav = mux_wav_bytes(&media, &packets).unwrap();
        store
            .put(&source, PutPayload::from_bytes(wav))
            .await
            .unwrap();

        let report = transcode_object_same_store(
            &store,
            &source,
            &target,
            &ObjectTranscodeJob::new(),
            None,
            None,
        )
        .await
        .unwrap();

        assert_eq!(
            report.operation,
            ObjectTranscodeOperation::SameStorePacketCopyMux
        );
        assert_eq!(report.source_format, ContainerFormat::Wav);
        assert_eq!(report.target_format, ContainerFormat::Aiff);
        assert!(report.plan.unwrap().is_packet_copy_only());

        let output = read_object_bytes(&store, &target).await.unwrap();
        let demuxed = demux_aiff_bytes(&output).unwrap();
        assert_eq!(demuxed.media.streams[0].codec, CodecId::Pcm);
        let decoded = decode_pcm_packet(&demuxed.media.streams[0], &demuxed.packets[0]).unwrap();
        assert_samples_close(
            &decoded.samples_f32_interleaved,
            &[0.0, 0.25, -0.25, 0.5],
            0.0001,
        );
    });
}

#[test]
fn transcode_job_runs_configured_video_stage_and_muxes_output() {
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
        let job = ObjectTranscodeJob::new().with_video(ObjectVideoTransformJob::new(pipeline));
        let mut decoder = RawRgbaVideoDecoder::new(2, 1);
        let mut encoder = RawRgbaVideoEncoder::new(1, time_base, 33);

        let report = transcode_object_same_store(
            &store,
            &source,
            &target,
            &job,
            Some(&mut decoder),
            Some(&mut encoder),
        )
        .await
        .unwrap();

        assert_eq!(
            report.operation,
            ObjectTranscodeOperation::VideoTranscodeMux
        );
        assert_eq!(report.video_track_id, Some(1));
        assert_eq!(report.input_packets, 1);
        assert_eq!(report.output_packets, 1);
        assert_eq!(report.decoded_video_frames, 1);
        assert_eq!(report.encoded_video_packets, 1);
        assert_eq!(report.copied_packets, 0);

        let output = read_object_bytes(&store, &target).await.unwrap();
        let demuxed = demux_matroska_bytes(ContainerFormat::Matroska, &output).unwrap();
        let mut output_decoder = RawRgbaVideoDecoder::new(2, 1);
        let frames = output_decoder.decode_packet(&demuxed.packets[0]).unwrap();
        assert_eq!(frames[0].pixel(0, 0), Some([143, 143, 143, 255]));
        assert_eq!(frames[0].pixel(1, 0), Some([19, 19, 19, 255]));
    });
}

#[test]
fn transcode_job_reports_missing_video_backends() {
    block_on(async {
        let store = InMemory::new();
        let source = Path::from("raw/input.mkv");
        let target = Path::from("raw/output.mkv");
        let time_base = TimeBase::milliseconds();
        let mut media = MediaInfo::default();
        media.push_stream(
            StreamInfo::new(1, MediaType::Video, CodecId::RawVideo, time_base)
                .with_dimensions(1, 1),
        );
        let packet = EncodedPacket::new(
            1,
            CodecId::RawVideo,
            0,
            33,
            time_base,
            Bytes::from_static(&[1, 2, 3, 255]),
        )
        .with_keyframe(true);
        let bytes = mux_matroska_bytes(ContainerFormat::Matroska, &media, &[packet]).unwrap();
        store
            .put(&source, PutPayload::from_bytes(bytes))
            .await
            .unwrap();
        let job = ObjectTranscodeJob::new()
            .with_video(ObjectVideoTransformJob::new(FrameTransformPipeline::new()));

        let err = transcode_object_same_store(&store, &source, &target, &job, None, None)
            .await
            .unwrap_err();

        assert!(matches!(
            err,
            Error::Unsupported {
                operation: "object transcode",
                ..
            }
        ));
        assert!(store.get(&target).await.is_err());
    });
}

#[test]
fn transcode_job_can_disable_packet_copy_fallback() {
    block_on(async {
        let store = InMemory::new();
        let source = Path::from("clips/input.mkv");
        let target = Path::from("clips/output.mkv");
        store
            .put(&source, PutPayload::from_static(b"body"))
            .await
            .unwrap();

        let err = transcode_object_same_store(
            &store,
            &source,
            &target,
            &ObjectTranscodeJob::new().allow_packet_copy(false),
            None,
            None,
        )
        .await
        .unwrap_err();

        assert!(matches!(
            err,
            Error::Unsupported {
                operation: "object transcode",
                ..
            }
        ));
    });
}

fn pcm_media_packets() -> (MediaInfo, Vec<EncodedPacket>) {
    let time_base = TimeBase::new(1, 48_000).unwrap();
    let mut stream =
        StreamInfo::new(1, MediaType::Audio, CodecId::Pcm, time_base).with_audio_format(48_000, 2);
    set_pcm_tags(&mut stream, PcmEncoding::signed_16(), 4);
    let mut media = MediaInfo::default();
    media.push_stream(stream);
    let frame = AudioFrame::new(48_000, 2, 0, vec![0.0, 0.25, -0.25, 0.5]).unwrap();
    let packet = encode_pcm_packet(&frame, 1, 0, PcmEncoding::signed_16()).unwrap();
    (media, vec![packet])
}

fn assert_samples_close(actual: &[f32], expected: &[f32], tolerance: f32) {
    assert_eq!(actual.len(), expected.len());
    for (actual, expected) in actual.iter().zip(expected) {
        assert!(
            (actual - expected).abs() <= tolerance,
            "sample {actual} differs from {expected} by more than {tolerance}"
        );
    }
}
