#![cfg(feature = "containers")]

use bytes::Bytes;
use futures::executor::block_on;
use object_store::{ObjectStore, PutPayload, memory::InMemory, path::Path};
use video_utils_rs::{
    CodecId, EncodedPacket, MediaInfo, MediaType, ObjectSubtitleBurnInJob, ObjectSubtitleTrackJob,
    RawRgbaVideoDecoder, RawRgbaVideoEncoder, RgbaFrame, StreamInfo, SubtitleFormat, TimeBase,
    add_subtitle_sidecar_to_object_same_store, burn_subtitle_sidecar_into_object_same_store,
    demux_matroska_bytes, extract_subtitle_track_to_sidecar_same_store, mux_matroska_bytes,
    read_object_bytes,
};

#[test]
fn subtitle_sidecar_can_be_added_to_and_extracted_from_matroska_object() {
    block_on(async {
        let store = InMemory::new();
        let source = Path::from("media/source.mkv");
        let sidecar = Path::from("subs/captions.srt");
        let muxed = Path::from("media/with-subs.mkv");
        let extracted = Path::from("subs/extracted.srt");

        let (media, packets) = raw_video_media();
        let source_bytes =
            mux_matroska_bytes(video_utils_rs::ContainerFormat::Matroska, &media, &packets)
                .unwrap();
        store
            .put(&source, PutPayload::from_bytes(source_bytes))
            .await
            .unwrap();
        store
            .put(
                &sidecar,
                PutPayload::from_static(
                    b"1\n00:00:00,000 --> 00:00:01,000\nHello subtitle track\n\n",
                ),
            )
            .await
            .unwrap();

        let add_report = add_subtitle_sidecar_to_object_same_store(
            &store,
            &source,
            &sidecar,
            &muxed,
            &ObjectSubtitleTrackJob::new(3, SubtitleFormat::Srt).with_language("eng"),
        )
        .await
        .unwrap();
        assert_eq!(add_report.subtitle_track_id, 3);
        assert_eq!(add_report.event_count, 1);

        let muxed_bytes = read_object_bytes(&store, &muxed).await.unwrap();
        let demuxed =
            demux_matroska_bytes(video_utils_rs::ContainerFormat::Matroska, &muxed_bytes).unwrap();
        assert!(
            demuxed
                .media
                .streams
                .iter()
                .any(|stream| stream.codec == CodecId::Srt)
        );

        let extract_report = extract_subtitle_track_to_sidecar_same_store(
            &store,
            &muxed,
            &extracted,
            Some(3),
            SubtitleFormat::Srt,
        )
        .await
        .unwrap();
        assert_eq!(extract_report.event_count, 1);
        let extracted_text = String::from_utf8(
            read_object_bytes(&store, &extracted)
                .await
                .unwrap()
                .to_vec(),
        )
        .unwrap();
        assert!(extracted_text.contains("Hello subtitle track"));
    });
}

#[test]
fn subtitle_sidecar_burn_in_transforms_raw_rgba_video_frames() {
    block_on(async {
        let store = InMemory::new();
        let source = Path::from("media/source.mkv");
        let sidecar = Path::from("subs/burn.srt");
        let target = Path::from("media/burned.mkv");
        let (media, packets) = raw_video_media();
        let source_bytes =
            mux_matroska_bytes(video_utils_rs::ContainerFormat::Matroska, &media, &packets)
                .unwrap();
        store
            .put(&source, PutPayload::from_bytes(source_bytes))
            .await
            .unwrap();
        store
            .put(
                &sidecar,
                PutPayload::from_static(b"1\n00:00:00,000 --> 00:00:01,000\nHI\n\n"),
            )
            .await
            .unwrap();

        let mut decoder = RawRgbaVideoDecoder::new(64, 32);
        let mut encoder = RawRgbaVideoEncoder::new(1, TimeBase::milliseconds(), 1_000);
        let report = burn_subtitle_sidecar_into_object_same_store(
            &store,
            &source,
            &sidecar,
            &target,
            &ObjectSubtitleBurnInJob::new(SubtitleFormat::Srt).preserve_non_video(false),
            &mut decoder,
            &mut encoder,
        )
        .await
        .unwrap();

        assert_eq!(report.decoded_frames, 1);
        assert_eq!(report.encoded_video_packets, 1);

        let burned = read_object_bytes(&store, &target).await.unwrap();
        let demuxed =
            demux_matroska_bytes(video_utils_rs::ContainerFormat::Matroska, &burned).unwrap();
        assert_eq!(demuxed.packets.len(), 1);
        assert_ne!(demuxed.packets[0].data, packets[0].data);
    });
}

fn raw_video_media() -> (MediaInfo, Vec<EncodedPacket>) {
    let time_base = TimeBase::milliseconds();
    let mut media = MediaInfo {
        duration_seconds: Some(1.0),
        ..Default::default()
    };
    media.push_stream(
        StreamInfo::new(1, MediaType::Video, CodecId::RawVideo, time_base).with_dimensions(64, 32),
    );
    let frame = RgbaFrame::solid(64, 32, [0, 0, 0, 255]);
    let packet = EncodedPacket::new(
        1,
        CodecId::RawVideo,
        0,
        1_000,
        time_base,
        Bytes::from(frame.data),
    )
    .with_keyframe(true);
    (media, vec![packet])
}
