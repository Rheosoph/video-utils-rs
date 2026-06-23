use bytes::Bytes;
use futures::executor::block_on;
use object_store::{ObjectStore, PutPayload, memory::InMemory, path::Path};
use video_utils_rs::{
    CodecId, ContainerFormat, Error, MediaInfo, MediaType, ObjectChunkReadOptions,
    ObjectRemuxOperation, RemuxAction, StreamInfo, TimeBase, copy_object_between_stores,
    copy_object_same_store, detect_object_container_format, plan_container_remux,
    plan_object_remux, read_object_bytes, read_object_chunks, read_object_range,
    remux_object_between_stores, remux_object_same_store, write_object_bytes,
};

fn mp4_h264_aac_info() -> MediaInfo {
    let mut info = MediaInfo::default();
    info.push_stream(
        StreamInfo::new(
            1,
            MediaType::Video,
            CodecId::H264,
            TimeBase::new(1, 90_000).unwrap(),
        )
        .with_dimensions(1280, 720),
    );
    info.push_stream(
        StreamInfo::new(
            2,
            MediaType::Audio,
            CodecId::Aac,
            TimeBase::new(1, 48_000).unwrap(),
        )
        .with_audio_format(48_000, 2),
    );
    info
}

#[test]
fn container_remux_plan_packet_copies_mov_compatible_streams() {
    let plan = plan_container_remux(
        ContainerFormat::Mp4,
        ContainerFormat::QuickTime,
        &mp4_h264_aac_info(),
    )
    .unwrap();

    assert!(plan.is_packet_copy_only());
    assert!(!plan.requires_transcode());
    assert!(!plan.has_unsupported_streams());
}

#[test]
fn container_remux_plan_marks_webm_transcodes() {
    let plan = plan_container_remux(
        ContainerFormat::Mp4,
        ContainerFormat::WebM,
        &mp4_h264_aac_info(),
    )
    .unwrap();

    assert!(!plan.is_packet_copy_only());
    assert!(plan.requires_transcode());
    assert!(!plan.has_unsupported_streams());
    assert!(matches!(
        plan.stream(1).unwrap().action,
        RemuxAction::TranscodeRequired { .. }
    ));
    assert!(matches!(
        plan.stream(2).unwrap().action,
        RemuxAction::TranscodeRequired { .. }
    ));
}

#[test]
fn container_remux_plan_rejects_streams_target_cannot_carry() {
    let plan = plan_container_remux(
        ContainerFormat::Mp4,
        ContainerFormat::Wav,
        &mp4_h264_aac_info(),
    )
    .unwrap();

    assert!(plan.requires_transcode());
    assert!(plan.has_unsupported_streams());
    assert!(matches!(
        plan.stream(1).unwrap().action,
        RemuxAction::Unsupported { .. }
    ));
    assert!(matches!(
        plan.stream(2).unwrap().action,
        RemuxAction::TranscodeRequired { .. }
    ));
}

#[test]
fn object_store_read_write_round_trip_uses_object_store_trait() {
    block_on(async {
        let store = InMemory::new();
        let location = Path::from("media/raw/input.bin");
        let bytes = Bytes::from_static(b"object-store media bytes");

        write_object_bytes(&store, &location, bytes.clone())
            .await
            .unwrap();
        let fetched = read_object_bytes(&store, &location).await.unwrap();

        assert_eq!(fetched, bytes);
    });
}

#[test]
fn object_store_range_and_chunk_reads_preserve_offsets() {
    block_on(async {
        let store = InMemory::new();
        let location = Path::from("media/raw/chunked.bin");
        let bytes = Bytes::from_static(b"abcdefghijklmnop");
        write_object_bytes(&store, &location, bytes.clone())
            .await
            .unwrap();

        let range = read_object_range(&store, &location, 3..8).await.unwrap();
        let chunks = read_object_chunks(&store, &location, ObjectChunkReadOptions::new(5))
            .await
            .unwrap();

        assert_eq!(range, Bytes::from_static(b"defgh"));
        assert_eq!(chunks.len(), 4);
        assert_eq!(chunks[0].offset, 0);
        assert_eq!(chunks[0].bytes, Bytes::from_static(b"abcde"));
        assert_eq!(chunks[3].offset, 15);
        assert_eq!(chunks[3].bytes, Bytes::from_static(b"p"));
    });
}

#[test]
fn same_store_copy_uses_object_store_copy() {
    block_on(async {
        let store = InMemory::new();
        let source = Path::from("clips/source.mp4");
        let target = Path::from("clips/copied.mp4");
        store
            .put(&source, PutPayload::from_static(b"mp4 object bytes"))
            .await
            .unwrap();

        let report = copy_object_same_store(&store, &source, &target)
            .await
            .unwrap();

        assert_eq!(report.bytes_written, 16);
        assert_eq!(report.source_format, Some(ContainerFormat::Mp4));
        assert_eq!(report.target_format, Some(ContainerFormat::Mp4));
        assert_eq!(
            read_object_bytes(&store, &target).await.unwrap(),
            Bytes::from_static(b"mp4 object bytes")
        );
    });
}

#[test]
fn cross_store_copy_uses_object_store_get_and_put() {
    block_on(async {
        let source_store = InMemory::new();
        let target_store = InMemory::new();
        let source = Path::from("source/input.webm");
        let target = Path::from("target/output.webm");
        store_bytes(&source_store, &source, b"webm bytes").await;

        let report = copy_object_between_stores(&source_store, &source, &target_store, &target)
            .await
            .unwrap();

        assert_eq!(report.bytes_written, 10);
        assert_eq!(report.source_format, Some(ContainerFormat::WebM));
        assert_eq!(report.target_format, Some(ContainerFormat::WebM));
        assert_eq!(
            read_object_bytes(&target_store, &target).await.unwrap(),
            Bytes::from_static(b"webm bytes")
        );
    });
}

#[test]
fn detect_object_format_uses_extension_first() {
    block_on(async {
        let store = InMemory::new();
        let source = Path::from("source/no-real-header.mov");
        store_bytes(&store, &source, b"not a quicktime header").await;

        let format = detect_object_container_format(&store, &source)
            .await
            .unwrap();

        assert_eq!(format, ContainerFormat::QuickTime);
    });
}

#[test]
fn detect_object_format_falls_back_to_header() {
    block_on(async {
        let store = InMemory::new();
        let source = Path::from("source/no-extension");
        store_bytes(&store, &source, b"\0\0\0\x18ftypisom\0\0\0\0payload").await;

        let format = detect_object_container_format(&store, &source)
            .await
            .unwrap();

        assert_eq!(format, ContainerFormat::Mp4);
    });
}

#[test]
fn plan_object_remux_detects_source_from_store() {
    block_on(async {
        let store = InMemory::new();
        let source = Path::from("incoming/no-extension");
        let target = Path::from("out/video.webm");
        store_bytes(&store, &source, b"\0\0\0\x18ftypisom\0\0\0\0payload").await;

        let plan = plan_object_remux(&store, &source, &target, &mp4_h264_aac_info())
            .await
            .unwrap();

        assert_eq!(plan.source, ContainerFormat::Mp4);
        assert_eq!(plan.target, ContainerFormat::WebM);
        assert!(plan.requires_transcode());
    });
}

#[test]
fn same_format_remux_same_store_copies_object() {
    block_on(async {
        let store = InMemory::new();
        let source = Path::from("clips/input.mp4");
        let target = Path::from("clips/output.mp4");
        store_bytes(&store, &source, b"same format media").await;

        let report = remux_object_same_store(&store, &source, &target, Some(&mp4_h264_aac_info()))
            .await
            .unwrap();

        assert_eq!(report.operation, ObjectRemuxOperation::SameStoreCopy);
        assert_eq!(report.source_format, ContainerFormat::Mp4);
        assert_eq!(report.target_format, ContainerFormat::Mp4);
        assert_eq!(report.bytes_written, 17);
        assert!(report.plan.unwrap().is_packet_copy_only());
        assert_eq!(
            read_object_bytes(&store, &target).await.unwrap(),
            Bytes::from_static(b"same format media")
        );
    });
}

#[test]
fn same_format_remux_between_stores_copies_object_bytes() {
    block_on(async {
        let source_store = InMemory::new();
        let target_store = InMemory::new();
        let source = Path::from("clips/input.webm");
        let target = Path::from("clips/output.webm");
        store_bytes(&source_store, &source, b"same webm bytes").await;

        let report =
            remux_object_between_stores(&source_store, &source, &target_store, &target, None)
                .await
                .unwrap();

        assert_eq!(report.operation, ObjectRemuxOperation::CrossStoreByteCopy);
        assert_eq!(report.source_format, ContainerFormat::WebM);
        assert_eq!(report.target_format, ContainerFormat::WebM);
        assert_eq!(
            read_object_bytes(&target_store, &target).await.unwrap(),
            Bytes::from_static(b"same webm bytes")
        );
    });
}

#[test]
fn cross_container_remux_is_not_byte_copied() {
    block_on(async {
        let store = InMemory::new();
        let source = Path::from("clips/input.mp4");
        let target = Path::from("clips/output.webm");
        store_bytes(&store, &source, b"mp4 object").await;

        let err = remux_object_same_store(&store, &source, &target, Some(&mp4_h264_aac_info()))
            .await
            .unwrap_err();

        assert!(matches!(
            err,
            Error::Unsupported {
                operation: "object remux",
                ..
            }
        ));
        assert!(store.get(&target).await.is_err());
    });
}

async fn store_bytes(store: &InMemory, location: &Path, bytes: &'static [u8]) {
    store
        .put(location, PutPayload::from_static(bytes))
        .await
        .unwrap();
}
