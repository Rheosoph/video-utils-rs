#![cfg(feature = "containers")]

use bytes::Bytes;
use futures::executor::block_on;
use muxide::api::{MuxerBuilder, VideoCodec};
use object_store::{ObjectStore, PutPayload, memory::InMemory, path::Path};
use video_utils_rs::{
    CodecId, ContainerFormat, MediaType, demux_iso_bmff_bytes, demux_object,
    plan_object_remux_from_probe, probe_object_media_info,
};

#[test]
fn object_store_mp4_probe_and_demux_reads_real_packets() {
    block_on(async {
        let store = InMemory::new();
        let source = Path::from("media/input.mp4");
        let target = Path::from("media/output.mov");
        let bytes = make_h264_mp4();
        store
            .put(&source, PutPayload::from_bytes(bytes.clone()))
            .await
            .unwrap();

        let info = probe_object_media_info(&store, &source).await.unwrap();
        assert_eq!(info.streams.len(), 1);
        let stream = &info.streams[0];
        assert_eq!(stream.media_type, MediaType::Video);
        assert_eq!(stream.codec, CodecId::H264);
        assert_eq!(stream.width, Some(320));
        assert_eq!(stream.height, Some(180));
        assert!(
            stream
                .codec_config
                .as_ref()
                .is_some_and(|data| !data.is_empty())
        );

        let demuxed = demux_object(&store, &source).await.unwrap();
        assert_eq!(demuxed.format, ContainerFormat::Mp4);
        assert_eq!(demuxed.media.streams.len(), 1);
        assert_eq!(demuxed.packets.len(), 2);
        assert!(demuxed.packets[0].is_keyframe);
        assert_eq!(demuxed.packets[0].codec, CodecId::H264);
        assert!(!demuxed.packets[0].data.is_empty());
        assert!(demuxed.packets[1].pts > demuxed.packets[0].pts);

        let plan = plan_object_remux_from_probe(&store, &source, &target)
            .await
            .unwrap();
        assert_eq!(plan.source, ContainerFormat::Mp4);
        assert_eq!(plan.target, ContainerFormat::QuickTime);
        assert!(plan.is_packet_copy_only());
    });
}

#[test]
fn iso_bmff_demux_rejects_non_iso_bmff_formats() {
    let err =
        demux_iso_bmff_bytes(ContainerFormat::WebM, &Bytes::from_static(b"not-webm")).unwrap_err();

    assert!(matches!(
        err,
        video_utils_rs::Error::Unsupported {
            operation: "iso-bmff demux",
            ..
        }
    ));
}

fn make_h264_mp4() -> Bytes {
    let mut bytes = Vec::new();
    {
        let mut muxer = MuxerBuilder::new(&mut bytes)
            .video(VideoCodec::H264, 320, 180, 30.0)
            .build()
            .unwrap();
        muxer.write_video(0.0, &h264_keyframe(), true).unwrap();
        muxer
            .write_video(1.0 / 30.0, &h264_delta_frame(), false)
            .unwrap();
        muxer.finish().unwrap();
    }
    Bytes::from(bytes)
}

fn h264_keyframe() -> Vec<u8> {
    let mut data = Vec::new();
    data.extend_from_slice(&[
        0, 0, 0, 1, 0x67, 0x42, 0x00, 0x1e, 0x95, 0xa8, 0x14, 0x01, 0x6e,
    ]);
    data.extend_from_slice(&[0, 0, 0, 1, 0x68, 0xce, 0x3c, 0x80]);
    data.extend_from_slice(&[
        0, 0, 0, 1, 0x65, 0x88, 0x84, 0x00, 0x00, 0x03, 0x00, 0x00, 0x03,
    ]);
    data
}

fn h264_delta_frame() -> Vec<u8> {
    vec![0, 0, 0, 1, 0x41, 0x9a, 0x22, 0x11, 0x00]
}
