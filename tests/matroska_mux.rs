#![cfg(feature = "containers")]

use bytes::Bytes;
use futures::executor::block_on;
use object_store::{ObjectStore, PutPayload, memory::InMemory, path::Path};
use video_utils_rs::{
    CodecId, ContainerFormat, EncodedPacket, Error, MediaInfo, MediaType, ObjectRemuxOperation,
    StreamInfo, TimeBase, demux_matroska_bytes, mux_matroska_bytes, read_object_bytes,
    remux_object_same_store,
};

#[test]
fn webm_mux_round_trips_vp9_packets() {
    let (media, packets) = vp9_media();

    let bytes = mux_matroska_bytes(ContainerFormat::WebM, &media, &packets).unwrap();
    assert_eq!(
        ContainerFormat::from_magic(&bytes[..bytes.len().min(64)]),
        Some(ContainerFormat::WebM)
    );

    let demuxed = demux_matroska_bytes(ContainerFormat::WebM, &bytes).unwrap();
    assert_eq!(demuxed.format, ContainerFormat::WebM);
    assert_eq!(demuxed.media.streams.len(), 1);
    assert_eq!(demuxed.media.streams[0].codec, CodecId::VP9);
    assert_eq!(demuxed.packets.len(), 2);
    assert_eq!(demuxed.packets[0].pts, 0);
    assert_eq!(demuxed.packets[1].pts, 33);
}

#[test]
fn webm_mux_round_trips_vp9_and_opus_packets() {
    let video_time_base = TimeBase::new(1, 1000).unwrap();
    let audio_time_base = TimeBase::new(1, 48_000).unwrap();
    let mut media = MediaInfo {
        duration_seconds: Some(0.040),
        ..Default::default()
    };
    media.push_stream(
        StreamInfo::new(1, MediaType::Video, CodecId::VP9, video_time_base).with_dimensions(64, 36),
    );
    media.push_stream(
        StreamInfo::new(2, MediaType::Audio, CodecId::Opus, audio_time_base)
            .with_audio_format(48_000, 2),
    );
    let packets = vec![
        packet(1, CodecId::VP9, 0, 33, video_time_base, b"\x82\x49", true),
        packet(2, CodecId::Opus, 0, 960, audio_time_base, b"opus-a", true),
        packet(2, CodecId::Opus, 960, 960, audio_time_base, b"opus-b", true),
        packet(1, CodecId::VP9, 33, 33, video_time_base, b"\x83\x42", false),
    ];

    let bytes = mux_matroska_bytes(ContainerFormat::WebM, &media, &packets).unwrap();
    let demuxed = demux_matroska_bytes(ContainerFormat::WebM, &bytes).unwrap();

    assert_eq!(demuxed.media.streams.len(), 2);
    assert_eq!(demuxed.media.streams[0].codec, CodecId::VP9);
    assert_eq!(demuxed.media.streams[1].codec, CodecId::Opus);
    assert_eq!(demuxed.media.streams[1].sample_rate, Some(48_000));
    assert_eq!(demuxed.media.streams[1].channels, Some(2));
    assert_eq!(demuxed.packets.len(), 4);
    assert!(demuxed.packets.iter().any(|packet| packet.track_id == 2));
}

#[test]
fn webm_mux_round_trips_av1_packets() {
    let time_base = TimeBase::new(1, 1000).unwrap();
    let mut media = MediaInfo {
        duration_seconds: Some(0.033),
        ..Default::default()
    };
    media.push_stream(
        StreamInfo::new(1, MediaType::Video, CodecId::AV1, time_base).with_dimensions(64, 36),
    );
    let packets = vec![packet(
        1,
        CodecId::AV1,
        0,
        33,
        time_base,
        b"\x12\x00\x0a\x0b",
        true,
    )];

    let bytes = mux_matroska_bytes(ContainerFormat::WebM, &media, &packets).unwrap();
    let demuxed = demux_matroska_bytes(ContainerFormat::WebM, &bytes).unwrap();

    assert_eq!(demuxed.media.streams[0].codec, CodecId::AV1);
    assert_eq!(demuxed.packets.len(), 1);
    assert_eq!(
        demuxed.packets[0].data,
        Bytes::from_static(b"\x12\x00\x0a\x0b")
    );
}

#[test]
fn webm_mux_accepts_vorbis_with_codec_private() {
    let time_base = TimeBase::new(1, 48_000).unwrap();
    let mut media = MediaInfo {
        duration_seconds: Some(0.020),
        ..Default::default()
    };
    let mut stream = StreamInfo::new(1, MediaType::Audio, CodecId::Vorbis, time_base)
        .with_audio_format(48_000, 2);
    stream.codec_config = Some(Bytes::from_static(
        b"\x02\x1e\x1fvorbis-identvorbis-commentvorbis-setup",
    ));
    media.push_stream(stream);
    let packets = vec![packet(
        1,
        CodecId::Vorbis,
        0,
        960,
        time_base,
        b"vorbis-packet",
        true,
    )];

    let bytes = mux_matroska_bytes(ContainerFormat::WebM, &media, &packets).unwrap();
    let demuxed = demux_matroska_bytes(ContainerFormat::WebM, &bytes).unwrap();

    assert_eq!(demuxed.media.streams[0].codec, CodecId::Vorbis);
    assert!(
        demuxed.media.streams[0]
            .codec_config
            .as_ref()
            .is_some_and(|config| !config.is_empty())
    );
    assert_eq!(
        demuxed.packets[0].data,
        Bytes::from_static(b"vorbis-packet")
    );
}

#[test]
fn matroska_mux_normalizes_h264_annex_b_to_avc_samples() {
    let time_base = TimeBase::new(1, 1000).unwrap();
    let mut media = MediaInfo {
        duration_seconds: Some(0.033),
        ..Default::default()
    };
    let mut stream =
        StreamInfo::new(1, MediaType::Video, CodecId::H264, time_base).with_dimensions(64, 36);
    stream.codec_config = Some(Bytes::from_static(
        b"\x01\x42\x00\x1e\xff\xe1\0\x02\x67\x42\x01\0\x02\x68\xce",
    ));
    media.push_stream(stream);
    let packets = vec![packet(
        1,
        CodecId::H264,
        0,
        33,
        time_base,
        b"\0\0\0\x01\x67\x42\0\0\0\x01\x68\xce\0\0\0\x01\x65\x88",
        true,
    )];

    let bytes = mux_matroska_bytes(ContainerFormat::Matroska, &media, &packets).unwrap();
    let demuxed = demux_matroska_bytes(ContainerFormat::Matroska, &bytes).unwrap();

    assert_eq!(demuxed.media.streams[0].codec, CodecId::H264);
    assert_eq!(
        demuxed.packets[0].data,
        Bytes::from_static(b"\0\0\0\x02\x65\x88")
    );
}

#[test]
fn matroska_mux_round_trips_srt_subtitle_packets() {
    let time_base = TimeBase::milliseconds();
    let mut media = MediaInfo {
        duration_seconds: Some(2.0),
        ..Default::default()
    };
    let mut stream = StreamInfo::new(3, MediaType::Subtitle, CodecId::Srt, time_base);
    stream.language = Some("eng".to_owned());
    media.push_stream(stream);
    let packets = vec![packet(
        3,
        CodecId::Srt,
        100,
        1_500,
        time_base,
        b"Hello from Matroska",
        true,
    )];

    let bytes = mux_matroska_bytes(ContainerFormat::Matroska, &media, &packets).unwrap();
    let demuxed = demux_matroska_bytes(ContainerFormat::Matroska, &bytes).unwrap();

    assert_eq!(demuxed.media.streams[0].media_type, MediaType::Subtitle);
    assert_eq!(demuxed.media.streams[0].codec, CodecId::Srt);
    assert_eq!(demuxed.media.streams[0].language.as_deref(), Some("eng"));
    assert_eq!(demuxed.packets.len(), 1);
    assert_eq!(
        demuxed.packets[0].data,
        Bytes::from_static(b"Hello from Matroska")
    );
}

#[test]
fn object_store_webm_to_mkv_uses_packet_copy_mux() {
    block_on(async {
        let store = InMemory::new();
        let source = Path::from("media/input.webm");
        let target = Path::from("media/output.mkv");
        let (media, packets) = vp9_media();
        let source_bytes = mux_matroska_bytes(ContainerFormat::WebM, &media, &packets).unwrap();
        store
            .put(&source, PutPayload::from_bytes(source_bytes))
            .await
            .unwrap();

        let report = remux_object_same_store(&store, &source, &target, None)
            .await
            .unwrap();

        assert_eq!(report.source_format, ContainerFormat::WebM);
        assert_eq!(report.target_format, ContainerFormat::Matroska);
        assert_eq!(
            report.operation,
            ObjectRemuxOperation::SameStorePacketCopyMux
        );
        assert!(report.plan.unwrap().is_packet_copy_only());

        let output = read_object_bytes(&store, &target).await.unwrap();
        let demuxed = demux_matroska_bytes(ContainerFormat::Matroska, &output).unwrap();
        assert_eq!(demuxed.format, ContainerFormat::Matroska);
        assert_eq!(demuxed.media.streams[0].codec, CodecId::VP9);
        assert_eq!(demuxed.packets.len(), 2);
    });
}

#[test]
fn object_store_mkv_to_webm_requires_webm_compatible_codecs() {
    block_on(async {
        let store = InMemory::new();
        let source = Path::from("media/input.mkv");
        let target = Path::from("media/output.webm");
        let (media, packets) = vp9_media();
        let source_bytes = mux_matroska_bytes(ContainerFormat::Matroska, &media, &packets).unwrap();
        store
            .put(&source, PutPayload::from_bytes(source_bytes))
            .await
            .unwrap();

        let report = remux_object_same_store(&store, &source, &target, None)
            .await
            .unwrap();

        assert_eq!(report.target_format, ContainerFormat::WebM);
        assert_eq!(
            report.operation,
            ObjectRemuxOperation::SameStorePacketCopyMux
        );
        let output = read_object_bytes(&store, &target).await.unwrap();
        let demuxed = demux_matroska_bytes(ContainerFormat::WebM, &output).unwrap();
        assert_eq!(demuxed.format, ContainerFormat::WebM);
        assert_eq!(demuxed.media.streams[0].codec, CodecId::VP9);
    });
}

#[test]
fn webm_mux_rejects_h264_packets_without_transcoding() {
    let mut media = MediaInfo::default();
    media.push_stream(
        StreamInfo::new(
            1,
            MediaType::Video,
            CodecId::H264,
            TimeBase::new(1, 90_000).unwrap(),
        )
        .with_dimensions(64, 36),
    );
    let packets = vec![video_packet(CodecId::H264, 0, true)];

    let err = mux_matroska_bytes(ContainerFormat::WebM, &media, &packets).unwrap_err();

    assert!(matches!(
        err,
        Error::Unsupported {
            operation: "matroska mux",
            ..
        }
    ));
}

fn vp9_media() -> (MediaInfo, Vec<video_utils_rs::EncodedPacket>) {
    let time_base = TimeBase::new(1, 1000).unwrap();
    let mut media = MediaInfo {
        duration_seconds: Some(0.066),
        ..Default::default()
    };
    media.push_stream(
        StreamInfo::new(1, MediaType::Video, CodecId::VP9, time_base).with_dimensions(64, 36),
    );
    let packets = vec![
        video_packet(CodecId::VP9, 0, true),
        video_packet(CodecId::VP9, 33, false),
    ];
    (media, packets)
}

fn video_packet(codec: CodecId, pts: i64, is_keyframe: bool) -> video_utils_rs::EncodedPacket {
    packet(
        1,
        codec,
        pts,
        33,
        TimeBase::new(1, 1000).unwrap(),
        b"\x82\x49\x83\x42",
        is_keyframe,
    )
}

fn packet(
    track_id: u32,
    codec: CodecId,
    pts: i64,
    duration: i64,
    time_base: TimeBase,
    data: &'static [u8],
    is_keyframe: bool,
) -> EncodedPacket {
    EncodedPacket::new(
        track_id,
        codec,
        pts,
        duration,
        time_base,
        Bytes::from_static(data),
    )
    .with_keyframe(is_keyframe)
}
