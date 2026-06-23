#![cfg(feature = "containers")]

use bytes::Bytes;
use futures::executor::block_on;
use object_store::{ObjectStore, PutPayload, memory::InMemory, path::Path};
use video_utils_rs::{
    AudioFrame, CodecId, ContainerFormat, EncodedPacket, MediaInfo, MediaType, PcmEncoding,
    StreamInfo, TimeBase, decode_pcm_packet, demux_object, demux_ogg_bytes, demux_wav_bytes,
    encode_pcm_packet, mux_object, mux_ogg_bytes, mux_wav_bytes, read_object_bytes,
    remux_object_same_store, set_pcm_tags,
};

#[test]
fn wav_object_demux_mux_and_remux_round_trip() {
    block_on(async {
        let store = InMemory::new();
        let source = Path::from("audio/source.wav");
        let target = Path::from("audio/remuxed.wav");
        let (media, packets) = pcm_media_packets();
        let bytes = mux_wav_bytes(&media, &packets).unwrap();
        store
            .put(&source, PutPayload::from_bytes(bytes))
            .await
            .unwrap();

        let demuxed = demux_object(&store, &source).await.unwrap();
        assert_eq!(demuxed.format, ContainerFormat::Wav);
        assert_eq!(demuxed.media.streams[0].codec, CodecId::Pcm);
        let decoded = decode_pcm_packet(&demuxed.media.streams[0], &demuxed.packets[0]).unwrap();
        assert_eq!(decoded.sample_rate, 48_000);
        assert_eq!(decoded.channels, 2);

        let report = mux_object(&store, &target, &demuxed.media, &demuxed.packets)
            .await
            .unwrap();
        assert_eq!(report.target_format, ContainerFormat::Wav);
        assert_eq!(report.packet_count, 1);
        let remuxed = read_object_bytes(&store, &target).await.unwrap();
        let round_trip = demux_wav_bytes(&remuxed).unwrap();
        assert_eq!(round_trip.packets.len(), 1);

        let copy_target = Path::from("audio/copied.wav");
        let copy = remux_object_same_store(&store, &source, &copy_target, None)
            .await
            .unwrap();
        assert_eq!(copy.source_format, ContainerFormat::Wav);
        assert_eq!(copy.target_format, ContainerFormat::Wav);
    });
}

#[test]
fn ogg_object_demux_mux_and_remux_round_trip() {
    block_on(async {
        let store = InMemory::new();
        let source = Path::from("audio/source.ogg");
        let target = Path::from("audio/remuxed.ogg");
        let (media, packets) = opus_media_packets();
        let bytes = mux_ogg_bytes(&media, &packets).unwrap();
        store
            .put(&source, PutPayload::from_bytes(bytes))
            .await
            .unwrap();

        let demuxed = demux_object(&store, &source).await.unwrap();
        assert_eq!(demuxed.format, ContainerFormat::Ogg);
        assert_eq!(demuxed.media.streams[0].codec, CodecId::Opus);
        assert_eq!(demuxed.packets.len(), 2);

        let report = mux_object(&store, &target, &demuxed.media, &demuxed.packets)
            .await
            .unwrap();
        assert_eq!(report.target_format, ContainerFormat::Ogg);
        let remuxed = read_object_bytes(&store, &target).await.unwrap();
        let round_trip = demux_ogg_bytes(&remuxed).unwrap();
        assert_eq!(round_trip.packets.len(), 2);
        assert_eq!(round_trip.packets[0].data, Bytes::from_static(b"opus-a"));

        let copy_target = Path::from("audio/copied.ogg");
        let copy = remux_object_same_store(&store, &source, &copy_target, None)
            .await
            .unwrap();
        assert_eq!(copy.source_format, ContainerFormat::Ogg);
        assert_eq!(copy.target_format, ContainerFormat::Ogg);
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

fn opus_media_packets() -> (MediaInfo, Vec<EncodedPacket>) {
    let time_base = TimeBase::new(1, 48_000).unwrap();
    let mut media = MediaInfo::default();
    media.push_stream(
        StreamInfo::new(1, MediaType::Audio, CodecId::Opus, time_base).with_audio_format(48_000, 2),
    );
    let packets = vec![
        EncodedPacket::new(
            1,
            CodecId::Opus,
            0,
            960,
            time_base,
            Bytes::from_static(b"opus-a"),
        )
        .with_keyframe(true),
        EncodedPacket::new(
            1,
            CodecId::Opus,
            960,
            960,
            time_base,
            Bytes::from_static(b"opus-b"),
        )
        .with_keyframe(true),
    ];
    (media, packets)
}
