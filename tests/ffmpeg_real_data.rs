#![cfg(feature = "containers")]

use std::{
    fs,
    path::PathBuf,
    process::{Command, Stdio},
    time::{SystemTime, UNIX_EPOCH},
};

use futures::executor::block_on;
use object_store::{ObjectStore, PutPayload, memory::InMemory, path::Path as ObjectPath};
use video_utils_rs::{
    CodecId, ContainerFormat, MediaType, ObjectRemuxOperation, demux_object, mux_object,
    read_object_bytes, remux_object_same_store,
};

#[test]
#[ignore = "requires local ffmpeg with libx264"]
fn ffmpeg_h264_mp4_demuxes_and_muxes_through_object_store() {
    block_on(async {
        let dir = unique_temp_dir();
        fs::create_dir_all(&dir).unwrap();
        let fixture = dir.join("h264.mp4");
        run_ffmpeg([
            "-hide_banner",
            "-y",
            "-f",
            "lavfi",
            "-i",
            "testsrc=size=96x54:rate=5:duration=1",
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-pix_fmt",
            "yuv420p",
            "-movflags",
            "+faststart",
            fixture.to_str().unwrap(),
        ]);

        let store = InMemory::new();
        let source = ObjectPath::from("real/h264.mp4");
        let target = ObjectPath::from("real/remuxed.mp4");
        store
            .put(
                &source,
                PutPayload::from_bytes(fs::read(&fixture).unwrap().into()),
            )
            .await
            .unwrap();

        let demuxed = demux_object(&store, &source).await.unwrap();
        assert_eq!(demuxed.format, ContainerFormat::Mp4);
        assert_eq!(demuxed.media.streams[0].media_type, MediaType::Video);
        assert_eq!(demuxed.media.streams[0].codec, CodecId::H264);
        assert!(!demuxed.packets.is_empty());

        let report = mux_object(&store, &target, &demuxed.media, &demuxed.packets)
            .await
            .unwrap();
        assert_eq!(report.target_format, ContainerFormat::Mp4);
        let output = read_object_bytes(&store, &target).await.unwrap();
        assert_eq!(
            ContainerFormat::from_magic(&output[..32]),
            Some(ContainerFormat::Mp4)
        );
    });
}

#[test]
#[ignore = "requires local ffmpeg with libvpx-vp9 and libopus"]
fn ffmpeg_webm_remuxes_to_matroska_through_object_store() {
    block_on(async {
        let dir = unique_temp_dir();
        fs::create_dir_all(&dir).unwrap();
        let fixture = dir.join("vp9_opus.webm");
        run_ffmpeg([
            "-hide_banner",
            "-y",
            "-f",
            "lavfi",
            "-i",
            "testsrc=size=96x54:rate=5:duration=1",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=48000:duration=1",
            "-c:v",
            "libvpx-vp9",
            "-b:v",
            "160k",
            "-c:a",
            "libopus",
            fixture.to_str().unwrap(),
        ]);

        let store = InMemory::new();
        let source = ObjectPath::from("real/vp9_opus.webm");
        let target = ObjectPath::from("real/vp9_opus.mkv");
        store
            .put(
                &source,
                PutPayload::from_bytes(fs::read(&fixture).unwrap().into()),
            )
            .await
            .unwrap();

        let demuxed = demux_object(&store, &source).await.unwrap();
        assert_eq!(demuxed.format, ContainerFormat::WebM);
        assert!(
            demuxed
                .media
                .streams
                .iter()
                .any(|stream| stream.codec == CodecId::VP9)
        );
        assert!(
            demuxed
                .media
                .streams
                .iter()
                .any(|stream| stream.codec == CodecId::Opus)
        );

        let report = remux_object_same_store(&store, &source, &target, None)
            .await
            .unwrap();
        assert_eq!(
            report.operation,
            ObjectRemuxOperation::SameStorePacketCopyMux
        );
        assert_eq!(report.target_format, ContainerFormat::Matroska);

        let output = read_object_bytes(&store, &target).await.unwrap();
        assert_eq!(
            ContainerFormat::from_magic(&output[..output.len().min(128)]),
            Some(ContainerFormat::Matroska)
        );
    });
}

fn run_ffmpeg<const N: usize>(args: [&str; N]) {
    let status = Command::new("ffmpeg")
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .status()
        .expect("run ffmpeg");
    assert!(status.success());
}

fn unique_temp_dir() -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!("video-utils-rs-real-data-{nanos}"))
}
