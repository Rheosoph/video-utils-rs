#![cfg(feature = "containers")]

mod support;

use futures::executor::block_on;
use object_store::{ObjectStore, PutPayload, memory::InMemory, path::Path as ObjectPath};
use support::{FfmpegFixture, path_str};
use video_utils_rs::{
    CodecId, ContainerFormat, Error, MediaInfo, MediaType, ObjectRemuxOperation, demux_object,
    read_object_bytes, remux_object_same_store,
};

#[test]
#[ignore = "requires local ffmpeg with libx264 and AAC support"]
fn e2e_mov_to_mp4_packet_copy() {
    block_on(async {
        let fixture = FfmpegFixture::new("mov-to-mp4");
        generate_h264_aac(&fixture, "source.mov");

        let store = InMemory::new();
        let source = ObjectPath::from("incoming/source.mov");
        let target = ObjectPath::from("out/source.mp4");
        put_fixture(&store, &source, fixture.read("source.mov")).await;

        let source_demuxed = demux_object(&store, &source).await.unwrap();
        assert_eq!(source_demuxed.format, ContainerFormat::QuickTime);
        assert_h264_aac(&source_demuxed.media);

        let report = remux_object_same_store(&store, &source, &target, None)
            .await
            .unwrap();
        assert_eq!(
            report.operation,
            ObjectRemuxOperation::SameStorePacketCopyMux
        );
        assert_eq!(report.source_format, ContainerFormat::QuickTime);
        assert_eq!(report.target_format, ContainerFormat::Mp4);
        assert!(report.plan.unwrap().is_packet_copy_only());

        let output = demux_object(&store, &target).await.unwrap();
        assert_eq!(output.format, ContainerFormat::Mp4);
        assert_h264_aac(&output.media);
        assert_packet_floor(&output, &source_demuxed);
    });
}

#[test]
#[ignore = "requires local ffmpeg with libx264 and AAC support"]
fn e2e_mp4_to_mov_packet_copy() {
    block_on(async {
        let fixture = FfmpegFixture::new("mp4-to-mov");
        generate_h264_aac(&fixture, "source.mp4");

        let store = InMemory::new();
        let source = ObjectPath::from("incoming/source.mp4");
        let target = ObjectPath::from("out/source.mov");
        put_fixture(&store, &source, fixture.read("source.mp4")).await;

        let report = remux_object_same_store(&store, &source, &target, None)
            .await
            .unwrap();
        assert_eq!(report.source_format, ContainerFormat::Mp4);
        assert_eq!(report.target_format, ContainerFormat::QuickTime);

        let output_bytes = read_object_bytes(&store, &target).await.unwrap();
        assert_eq!(
            ContainerFormat::from_magic(&output_bytes[..output_bytes.len().min(64)]),
            Some(ContainerFormat::QuickTime)
        );
        let output = demux_object(&store, &target).await.unwrap();
        assert_eq!(output.format, ContainerFormat::QuickTime);
        assert_h264_aac(&output.media);
    });
}

#[test]
#[ignore = "requires local ffmpeg with libx264 and AAC support"]
fn e2e_mp4_to_mkv_packet_copy() {
    block_on(async {
        let fixture = FfmpegFixture::new("mp4-to-mkv");
        generate_h264_aac(&fixture, "source.mp4");

        let store = InMemory::new();
        let source = ObjectPath::from("incoming/source.mp4");
        let target = ObjectPath::from("out/source.mkv");
        put_fixture(&store, &source, fixture.read("source.mp4")).await;

        let report = remux_object_same_store(&store, &source, &target, None)
            .await
            .unwrap();
        assert_eq!(report.source_format, ContainerFormat::Mp4);
        assert_eq!(report.target_format, ContainerFormat::Matroska);

        let output = demux_object(&store, &target).await.unwrap();
        assert_eq!(output.format, ContainerFormat::Matroska);
        assert_h264_aac(&output.media);
    });
}

#[test]
#[ignore = "requires local ffmpeg with libx264 and AAC support"]
fn e2e_mkv_to_mp4_packet_copy() {
    block_on(async {
        let fixture = FfmpegFixture::new("mkv-to-mp4");
        generate_h264_aac(&fixture, "source.mkv");

        let store = InMemory::new();
        let source = ObjectPath::from("incoming/source.mkv");
        let target = ObjectPath::from("out/source.mp4");
        put_fixture(&store, &source, fixture.read("source.mkv")).await;

        let source_demuxed = demux_object(&store, &source).await.unwrap();
        assert_eq!(source_demuxed.format, ContainerFormat::Matroska);
        assert_h264_aac(&source_demuxed.media);

        let report = remux_object_same_store(&store, &source, &target, None)
            .await
            .unwrap();
        assert_eq!(report.source_format, ContainerFormat::Matroska);
        assert_eq!(report.target_format, ContainerFormat::Mp4);

        let output = demux_object(&store, &target).await.unwrap();
        assert_eq!(output.format, ContainerFormat::Mp4);
        assert_h264_aac(&output.media);
        assert_packet_floor(&output, &source_demuxed);
    });
}

#[test]
#[ignore = "requires local ffmpeg with libx264 and AAC support"]
fn e2e_mp4_to_ts_packet_copy() {
    block_on(async {
        let fixture = FfmpegFixture::new("mp4-to-ts");
        generate_h264_aac(&fixture, "source.mp4");

        let store = InMemory::new();
        let source = ObjectPath::from("incoming/source.mp4");
        let target = ObjectPath::from("out/source.ts");
        put_fixture(&store, &source, fixture.read("source.mp4")).await;

        let report = remux_object_same_store(&store, &source, &target, None)
            .await
            .unwrap();
        assert_eq!(report.source_format, ContainerFormat::Mp4);
        assert_eq!(report.target_format, ContainerFormat::MpegTs);

        let output = demux_object(&store, &target).await.unwrap();
        assert_eq!(output.format, ContainerFormat::MpegTs);
        assert_h264_aac(&output.media);
    });
}

#[test]
#[ignore = "requires local ffmpeg with libx264 and AAC support"]
fn e2e_ts_to_mp4_packet_copy_recovers_mux_metadata() {
    block_on(async {
        let fixture = FfmpegFixture::new("ts-to-mp4");
        generate_h264_aac(&fixture, "source.ts");

        let store = InMemory::new();
        let source = ObjectPath::from("incoming/source.ts");
        let target = ObjectPath::from("out/source.mp4");
        put_fixture(&store, &source, fixture.read("source.ts")).await;

        let source_demuxed = demux_object(&store, &source).await.unwrap();
        assert_eq!(source_demuxed.format, ContainerFormat::MpegTs);
        assert_h264_aac(&source_demuxed.media);

        let report = remux_object_same_store(&store, &source, &target, None)
            .await
            .unwrap();
        assert_eq!(report.source_format, ContainerFormat::MpegTs);
        assert_eq!(report.target_format, ContainerFormat::Mp4);

        let output = demux_object(&store, &target).await.unwrap();
        assert_eq!(output.format, ContainerFormat::Mp4);
        assert_h264_aac(&output.media);
        assert_video_dimensions(&output.media, 320, 180);
    });
}

#[test]
#[ignore = "requires local ffmpeg with libx264 and AAC support"]
fn e2e_mp4_to_flv_packet_copy() {
    block_on(async {
        let fixture = FfmpegFixture::new("mp4-to-flv");
        generate_h264_aac(&fixture, "source.mp4");

        let store = InMemory::new();
        let source = ObjectPath::from("incoming/source.mp4");
        let target = ObjectPath::from("out/source.flv");
        put_fixture(&store, &source, fixture.read("source.mp4")).await;

        let report = remux_object_same_store(&store, &source, &target, None)
            .await
            .unwrap();
        assert_eq!(report.source_format, ContainerFormat::Mp4);
        assert_eq!(report.target_format, ContainerFormat::Flv);

        let output = demux_object(&store, &target).await.unwrap();
        assert_eq!(output.format, ContainerFormat::Flv);
        assert_h264_aac(&output.media);
    });
}

#[test]
#[ignore = "requires local ffmpeg with libx264 and AAC support"]
fn e2e_flv_to_mp4_packet_copy_recovers_dimensions_from_avcc() {
    block_on(async {
        let fixture = FfmpegFixture::new("flv-to-mp4");
        generate_h264_aac(&fixture, "source.flv");

        let store = InMemory::new();
        let source = ObjectPath::from("incoming/source.flv");
        let target = ObjectPath::from("out/source.mp4");
        put_fixture(&store, &source, fixture.read("source.flv")).await;

        let source_demuxed = demux_object(&store, &source).await.unwrap();
        assert_eq!(source_demuxed.format, ContainerFormat::Flv);
        assert_h264_aac(&source_demuxed.media);

        let report = remux_object_same_store(&store, &source, &target, None)
            .await
            .unwrap();
        assert_eq!(report.source_format, ContainerFormat::Flv);
        assert_eq!(report.target_format, ContainerFormat::Mp4);

        let output = demux_object(&store, &target).await.unwrap();
        assert_eq!(output.format, ContainerFormat::Mp4);
        assert_h264_aac(&output.media);
        assert_video_dimensions(&output.media, 320, 180);
    });
}

#[test]
#[ignore = "requires local ffmpeg with libx264 and AAC support"]
fn e2e_incompatible_mp4_to_webm_remux_is_rejected() {
    block_on(async {
        let fixture = FfmpegFixture::new("mp4-to-webm-rejected");
        generate_h264_aac(&fixture, "source.mp4");

        let store = InMemory::new();
        let source = ObjectPath::from("incoming/source.mp4");
        let target = ObjectPath::from("out/source.webm");
        put_fixture(&store, &source, fixture.read("source.mp4")).await;

        let err = remux_object_same_store(&store, &source, &target, None)
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

fn generate_h264_aac(fixture: &FfmpegFixture, filename: &str) {
    let output = fixture.path(filename);
    let mut args = vec![
        "-hide_banner",
        "-y",
        "-f",
        "lavfi",
        "-i",
        "testsrc2=size=320x180:rate=30:duration=2",
        "-f",
        "lavfi",
        "-i",
        "sine=frequency=1000:sample_rate=48000:duration=2",
        "-c:v",
        "libx264",
        "-preset",
        "ultrafast",
        "-tune",
        "zerolatency",
        "-pix_fmt",
        "yuv420p",
        "-bf",
        "0",
        "-g",
        "30",
        "-keyint_min",
        "30",
        "-sc_threshold",
        "0",
        "-c:a",
        "aac",
        "-b:a",
        "96k",
        "-shortest",
    ];
    match filename.rsplit_once('.').map(|(_, extension)| extension) {
        Some("mkv") => args.extend(["-f", "matroska"]),
        Some("ts") => args.extend(["-f", "mpegts"]),
        Some("flv") => args.extend(["-f", "flv"]),
        _ => {}
    }
    args.push(path_str(&output));
    fixture.run_ffmpeg(&args);
}

async fn put_fixture(store: &InMemory, location: &ObjectPath, bytes: Vec<u8>) {
    store
        .put(location, PutPayload::from_bytes(bytes.into()))
        .await
        .unwrap();
}

fn assert_h264_aac(media: &MediaInfo) {
    assert!(
        media
            .streams
            .iter()
            .any(|stream| stream.media_type == MediaType::Video && stream.codec == CodecId::H264),
        "expected H.264 video stream, got {:?}",
        media.streams
    );
    assert!(
        media
            .streams
            .iter()
            .any(|stream| stream.media_type == MediaType::Audio && stream.codec == CodecId::Aac),
        "expected AAC audio stream, got {:?}",
        media.streams
    );
}

fn assert_video_dimensions(media: &MediaInfo, width: u32, height: u32) {
    let video = media.video_streams().next().expect("video stream");
    assert_eq!(video.width, Some(width));
    assert_eq!(video.height, Some(height));
}

fn assert_packet_floor(
    output: &video_utils_rs::DemuxedMedia,
    source: &video_utils_rs::DemuxedMedia,
) {
    for stream in &source.media.streams {
        let source_packets = source
            .packets
            .iter()
            .filter(|packet| packet.track_id == stream.track_id)
            .count();
        let output_packets = output
            .packets
            .iter()
            .filter(|packet| packet.codec == stream.codec)
            .count();
        assert!(
            output_packets >= source_packets.saturating_sub(1),
            "expected at least {} {:?} packets, got {}",
            source_packets.saturating_sub(1),
            stream.codec,
            output_packets
        );
    }
}
