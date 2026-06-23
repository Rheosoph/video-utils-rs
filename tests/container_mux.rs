#![cfg(feature = "containers")]

use bytes::Bytes;
use futures::executor::block_on;
use muxide::api::{MuxerBuilder, VideoCodec};
use object_store::{ObjectStore, PutPayload, memory::InMemory, path::Path};
use video_utils_rs::{
    CodecId, ContainerFormat, MediaType, ObjectRemuxOperation, demux_iso_bmff_bytes, demux_object,
    mux_iso_bmff_bytes, mux_object, read_object_bytes, remux_object_same_store,
};

#[test]
fn mp4_mux_round_trips_demuxed_h264_packets() {
    let source = make_h264_mp4();
    let demuxed = demux_iso_bmff_bytes(ContainerFormat::Mp4, &source).unwrap();

    let remuxed =
        mux_iso_bmff_bytes(ContainerFormat::Mp4, &demuxed.media, &demuxed.packets).unwrap();
    assert_eq!(
        ContainerFormat::from_magic(&remuxed[..64]),
        Some(ContainerFormat::Mp4)
    );

    let round_trip = demux_iso_bmff_bytes(ContainerFormat::Mp4, &remuxed).unwrap();
    assert_eq!(round_trip.media.streams.len(), 1);
    assert_eq!(round_trip.media.streams[0].media_type, MediaType::Video);
    assert_eq!(round_trip.media.streams[0].codec, CodecId::H264);
    assert_eq!(round_trip.packets.len(), 2);
    assert!(round_trip.packets[0].is_keyframe);
}

#[test]
fn mux_object_writes_mp4_from_demuxed_packets() {
    block_on(async {
        let store = InMemory::new();
        let source = Path::from("media/source.mp4");
        let target = Path::from("media/muxed.mp4");
        store
            .put(&source, PutPayload::from_bytes(make_h264_mp4()))
            .await
            .unwrap();

        let demuxed = demux_object(&store, &source).await.unwrap();
        let report = mux_object(&store, &target, &demuxed.media, &demuxed.packets)
            .await
            .unwrap();

        assert_eq!(report.target_format, ContainerFormat::Mp4);
        assert_eq!(report.packet_count, 2);
        assert!(report.bytes_written > 0);
        let remuxed = read_object_bytes(&store, &target).await.unwrap();
        let round_trip = demux_iso_bmff_bytes(ContainerFormat::Mp4, &remuxed).unwrap();
        assert_eq!(round_trip.packets.len(), 2);
    });
}

#[test]
fn object_store_mov_to_mp4_uses_packet_copy_mux() {
    block_on(async {
        let store = InMemory::new();
        let source = Path::from("media/source.mov");
        let target = Path::from("media/output.mp4");
        store
            .put(&source, PutPayload::from_bytes(make_h264_mp4()))
            .await
            .unwrap();

        let report = remux_object_same_store(&store, &source, &target, None)
            .await
            .unwrap();

        assert_eq!(report.source_format, ContainerFormat::QuickTime);
        assert_eq!(report.target_format, ContainerFormat::Mp4);
        assert_eq!(
            report.operation,
            ObjectRemuxOperation::SameStorePacketCopyMux
        );
        assert!(report.plan.unwrap().is_packet_copy_only());

        let output = read_object_bytes(&store, &target).await.unwrap();
        let round_trip = demux_iso_bmff_bytes(ContainerFormat::Mp4, &output).unwrap();
        assert_eq!(round_trip.media.streams[0].codec, CodecId::H264);
        assert_eq!(round_trip.packets.len(), 2);
    });
}

#[test]
fn iso_bmff_mux_writes_quicktime_output() {
    let source = make_h264_mp4();
    let demuxed = demux_iso_bmff_bytes(ContainerFormat::Mp4, &source).unwrap();

    let mov =
        mux_iso_bmff_bytes(ContainerFormat::QuickTime, &demuxed.media, &demuxed.packets).unwrap();

    assert_eq!(
        ContainerFormat::from_magic(&mov[..64]),
        Some(ContainerFormat::QuickTime)
    );
    let round_trip = demux_iso_bmff_bytes(ContainerFormat::QuickTime, &mov).unwrap();
    assert_eq!(round_trip.media.streams[0].codec, CodecId::H264);
    assert_eq!(round_trip.packets.len(), 2);
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
