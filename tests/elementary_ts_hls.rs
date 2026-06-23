#![cfg(feature = "containers")]

use bytes::Bytes;
use futures::executor::block_on;
use object_store::{ObjectStore, PutPayload, memory::InMemory, path::Path};
use video_utils_rs::{
    CodecId, ContainerFormat, EncodedPacket, HlsSegmentContainer, MediaInfo, MediaType,
    ObjectHlsVodJob, ObjectRemuxOperation, StreamInfo, TimeBase, demux_mpeg_ts_bytes,
    mux_mpeg_ts_bytes, package_object_hls_vod_same_store, read_object_bytes,
    remux_object_same_store, write_hls_vod_same_store,
};

#[test]
fn raw_h264_object_remuxes_to_transport_stream() {
    block_on(async {
        let store = InMemory::new();
        let source = Path::from("media/input.h264");
        let target = Path::from("media/output.ts");
        store
            .put(&source, PutPayload::from_static(h264_annex_b()))
            .await
            .unwrap();

        let report = remux_object_same_store(&store, &source, &target, None)
            .await
            .unwrap();

        assert_eq!(report.source_format, ContainerFormat::RawElementary);
        assert_eq!(report.target_format, ContainerFormat::MpegTs);
        assert_eq!(
            report.operation,
            ObjectRemuxOperation::SameStorePacketCopyMux
        );

        let output = read_object_bytes(&store, &target).await.unwrap();
        let demuxed = demux_mpeg_ts_bytes(&output).unwrap();
        assert_eq!(demuxed.format, ContainerFormat::MpegTs);
        assert_eq!(demuxed.media.streams[0].codec, CodecId::H264);
        assert_eq!(demuxed.packets.len(), 2);
    });
}

#[test]
fn transport_stream_mux_demux_round_trips_h264_and_aac_packets() {
    let video_time_base = TimeBase::new(1, 90_000).unwrap();
    let audio_time_base = TimeBase::new(1, 48_000).unwrap();
    let mut media = MediaInfo {
        duration_seconds: Some(0.067),
        ..Default::default()
    };
    media.push_stream(
        StreamInfo::new(1, MediaType::Video, CodecId::H264, video_time_base)
            .with_dimensions(16, 16),
    );
    let mut audio = StreamInfo::new(2, MediaType::Audio, CodecId::Aac, audio_time_base)
        .with_audio_format(48_000, 2);
    audio.codec_config = Some(Bytes::from_static(&[0x11, 0x90]));
    media.push_stream(audio);

    let packets = vec![
        EncodedPacket::new(
            1,
            CodecId::H264,
            0,
            3_000,
            video_time_base,
            Bytes::from_static(b"\0\0\0\x01\x65\x88"),
        )
        .with_keyframe(true),
        EncodedPacket::new(
            2,
            CodecId::Aac,
            0,
            1024,
            audio_time_base,
            Bytes::from_static(b"\x11\x22\x33"),
        )
        .with_keyframe(true),
        EncodedPacket::new(
            1,
            CodecId::H264,
            3_000,
            3_000,
            video_time_base,
            Bytes::from_static(b"\0\0\0\x01\x41\x99"),
        ),
    ];

    let bytes = mux_mpeg_ts_bytes(&media, &packets).unwrap();
    assert_eq!(bytes.len() % 188, 0);

    let demuxed = demux_mpeg_ts_bytes(&bytes).unwrap();
    assert_eq!(demuxed.media.streams.len(), 2);
    assert!(
        demuxed
            .media
            .streams
            .iter()
            .any(|stream| stream.codec == CodecId::H264)
    );
    assert!(
        demuxed
            .media
            .streams
            .iter()
            .any(|stream| stream.codec == CodecId::Aac)
    );
    assert!(
        demuxed
            .packets
            .iter()
            .any(|packet| packet.codec == CodecId::Aac)
    );
}

#[test]
fn transport_stream_demux_resyncs_around_junk_bytes() {
    let (media, packets) = hls_media();
    let bytes = mux_mpeg_ts_bytes(&media, &packets).unwrap();
    let mut noisy = Vec::from(&b"junk-prefix"[..]);
    noisy.extend_from_slice(&bytes);
    noisy.extend_from_slice(&bytes[..37]);

    let demuxed = demux_mpeg_ts_bytes(&Bytes::from(noisy)).unwrap();

    assert_eq!(demuxed.media.streams[0].codec, CodecId::H264);
    assert_eq!(demuxed.packets.len(), packets.len());
}

#[test]
fn transport_stream_mux_writes_pcr_on_video_pid() {
    let (media, packets) = hls_media();
    let bytes = mux_mpeg_ts_bytes(&media, &packets).unwrap();

    let has_pcr = bytes.chunks_exact(188).any(|packet| {
        let pid = (u16::from(packet[1] & 0x1f) << 8) | u16::from(packet[2]);
        let adaptation_control = (packet[3] >> 4) & 0x03;
        pid == 0x0101
            && matches!(adaptation_control, 2 | 3)
            && packet[4] >= 7
            && packet[5] & 0x10 != 0
    });

    assert!(has_pcr);
}

#[test]
fn hls_writer_creates_playlist_and_ts_segment_objects() {
    block_on(async {
        let store = InMemory::new();
        let playlist = Path::from("hls/out.m3u8");
        let (media, packets) = hls_media();
        let job = ObjectHlsVodJob::new()
            .with_target_duration(2.0)
            .with_segment_format(HlsSegmentContainer::MpegTs);

        let report = write_hls_vod_same_store(&store, &playlist, &media, &packets, &job)
            .await
            .unwrap();

        assert_eq!(report.segment_count, 2);
        assert_eq!(report.segments.len(), 2);
        let playlist_bytes = read_object_bytes(&store, &playlist).await.unwrap();
        let playlist_text = String::from_utf8(playlist_bytes.to_vec()).unwrap();
        assert!(playlist_text.contains("#EXTM3U"));
        assert!(playlist_text.contains("segment-00000.ts"));
        for segment in &report.segments {
            let bytes = read_object_bytes(&store, segment).await.unwrap();
            assert_eq!(bytes.len() % 188, 0);
        }
    });
}

#[test]
fn hls_writer_creates_fmp4_init_and_media_segments() {
    block_on(async {
        let store = InMemory::new();
        let playlist = Path::from("hls/fmp4.m3u8");
        let (media, packets) = hls_media();
        let job = ObjectHlsVodJob::new()
            .with_target_duration(2.0)
            .with_segment_format(HlsSegmentContainer::Mp4);

        let report = write_hls_vod_same_store(&store, &playlist, &media, &packets, &job)
            .await
            .unwrap();

        assert_eq!(report.segment_count, 2);
        assert_eq!(report.segments.len(), 2);
        let init_segment = report.init_segment.as_ref().unwrap();
        let init = read_object_bytes(&store, init_segment).await.unwrap();
        assert!(init.windows(4).any(|window| window == b"ftyp"));
        assert!(init.windows(4).any(|window| window == b"moov"));

        let playlist_text =
            String::from_utf8(read_object_bytes(&store, &playlist).await.unwrap().to_vec())
                .unwrap();
        assert!(playlist_text.contains("#EXT-X-MAP:URI=\"init.mp4\""));
        assert!(playlist_text.contains("segment-00000.m4s"));
        for segment in &report.segments {
            let bytes = read_object_bytes(&store, segment).await.unwrap();
            assert!(bytes.windows(4).any(|window| window == b"moof"));
            assert!(bytes.windows(4).any(|window| window == b"mdat"));
        }
    });
}

#[test]
fn hls_writer_creates_multitrack_fmp4_audio_video_segments() {
    block_on(async {
        let store = InMemory::new();
        let playlist = Path::from("hls/av-fmp4.m3u8");
        let (media, packets) = hls_av_media();
        let job = ObjectHlsVodJob::new()
            .with_target_duration(2.0)
            .with_segment_format(HlsSegmentContainer::Mp4);

        let report = write_hls_vod_same_store(&store, &playlist, &media, &packets, &job)
            .await
            .unwrap();

        assert_eq!(report.segment_count, 2);
        let init = read_object_bytes(&store, report.init_segment.as_ref().unwrap())
            .await
            .unwrap();
        assert!(contains_box(&init, b"avc1"));
        assert!(contains_box(&init, b"mp4a"));
        assert!(contains_box(&init, b"esds"));

        let first_segment = read_object_bytes(&store, &report.segments[0])
            .await
            .unwrap();
        assert!(contains_box(&first_segment, b"moof"));
        assert_eq!(count_box_name(&first_segment, b"traf"), 2);

        let playlist_text =
            String::from_utf8(read_object_bytes(&store, &playlist).await.unwrap().to_vec())
                .unwrap();
        assert!(playlist_text.contains("#EXT-X-MAP:URI=\"init.mp4\""));
        assert!(playlist_text.contains("segment-00000.m4s"));
    });
}

#[test]
fn hls_packager_demuxes_raw_elementary_source_from_object_store() {
    block_on(async {
        let store = InMemory::new();
        let source = Path::from("media/source.h264");
        let playlist = Path::from("vod/index.m3u8");
        store
            .put(&source, PutPayload::from_static(h264_annex_b()))
            .await
            .unwrap();

        let report = package_object_hls_vod_same_store(
            &store,
            &source,
            &playlist,
            &ObjectHlsVodJob::new().with_target_duration(0.03),
        )
        .await
        .unwrap();

        assert!(!report.segments.is_empty());
        assert!(read_object_bytes(&store, &playlist).await.unwrap().len() > 20);
    });
}

fn h264_annex_b() -> &'static [u8] {
    b"\0\0\0\x01\x67\x42\0\0\0\x01\x68\xce\0\0\0\x01\x65\x88\0\0\0\x01\x41\x99"
}

fn hls_media() -> (MediaInfo, Vec<EncodedPacket>) {
    let time_base = TimeBase::milliseconds();
    let mut media = MediaInfo {
        duration_seconds: Some(4.0),
        ..Default::default()
    };
    media.push_stream({
        let mut stream =
            StreamInfo::new(1, MediaType::Video, CodecId::H264, time_base).with_dimensions(32, 18);
        stream.codec_config = Some(Bytes::from_static(
            b"\x01\x42\x00\x1e\xff\xe1\0\x02\x67\x42\x01\0\x02\x68\xce",
        ));
        stream
    });
    let packets = vec![
        video_packet(0, true),
        video_packet(1_000, false),
        video_packet(2_000, true),
        video_packet(3_000, false),
    ];
    (media, packets)
}

fn hls_av_media() -> (MediaInfo, Vec<EncodedPacket>) {
    let video_time_base = TimeBase::milliseconds();
    let audio_time_base = TimeBase::new(1, 48_000).unwrap();
    let mut media = MediaInfo {
        duration_seconds: Some(4.0),
        ..Default::default()
    };
    media.push_stream({
        let mut stream = StreamInfo::new(1, MediaType::Video, CodecId::H264, video_time_base)
            .with_dimensions(32, 18);
        stream.codec_config = Some(Bytes::from_static(
            b"\x01\x42\x00\x1e\xff\xe1\0\x02\x67\x42\x01\0\x02\x68\xce",
        ));
        stream
    });
    media.push_stream({
        let mut stream = StreamInfo::new(2, MediaType::Audio, CodecId::Aac, audio_time_base)
            .with_audio_format(48_000, 2);
        stream.codec_config = Some(Bytes::from_static(&[0x11, 0x90]));
        stream
    });

    let packets = vec![
        video_packet(0, true),
        audio_packet(0),
        audio_packet(1024),
        video_packet(1_000, false),
        audio_packet(48_000),
        audio_packet(49_024),
        video_packet(2_000, true),
        audio_packet(96_000),
        audio_packet(97_024),
        video_packet(3_000, false),
        audio_packet(144_000),
        audio_packet(145_024),
    ];
    (media, packets)
}

fn video_packet(pts: i64, keyframe: bool) -> EncodedPacket {
    EncodedPacket::new(
        1,
        CodecId::H264,
        pts,
        1_000,
        TimeBase::milliseconds(),
        if keyframe {
            Bytes::from_static(b"\0\0\0\x01\x65\x88")
        } else {
            Bytes::from_static(b"\0\0\0\x01\x41\x99")
        },
    )
    .with_keyframe(keyframe)
}

fn audio_packet(pts: i64) -> EncodedPacket {
    EncodedPacket::new(
        2,
        CodecId::Aac,
        pts,
        1024,
        TimeBase::new(1, 48_000).unwrap(),
        Bytes::from_static(b"\x11\x22\x33"),
    )
    .with_keyframe(true)
}

fn contains_box(bytes: &[u8], name: &[u8; 4]) -> bool {
    bytes.windows(4).any(|window| window == name)
}

fn count_box_name(bytes: &[u8], name: &[u8; 4]) -> usize {
    bytes.windows(4).filter(|window| *window == name).count()
}
