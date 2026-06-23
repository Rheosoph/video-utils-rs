#![cfg(feature = "containers")]

use futures::executor::block_on;
use object_store::{ObjectStore, PutPayload, memory::InMemory, path::Path};
#[cfg(feature = "audio-io")]
use video_utils_rs::transform_object_audio_file_to_wav_same_store;
use video_utils_rs::{
    AudioFrame, AudioTransform, AudioTransformPipeline, CodecId, MediaInfo, MediaType,
    ObjectAudioTransformJob, PcmEncoding, StreamInfo, TimeBase, decode_pcm_packet, demux_wav_bytes,
    encode_pcm_packet, mux_wav_bytes, read_object_bytes, set_pcm_tags,
    transform_object_audio_same_store,
};

#[test]
fn wav_audio_transform_job_filters_pcm_frames_in_object_store() {
    block_on(async {
        let store = InMemory::new();
        let source = Path::from("audio/input.wav");
        let target = Path::from("audio/output.wav");

        let time_base = TimeBase::new(1, 48_000).unwrap();
        let mut stream = StreamInfo::new(1, MediaType::Audio, CodecId::Pcm, time_base)
            .with_audio_format(48_000, 1);
        set_pcm_tags(&mut stream, PcmEncoding::signed_16(), 2);
        let mut media = MediaInfo::default();
        media.push_stream(stream);
        let frame = AudioFrame::new(48_000, 1, 0, vec![0.25, -0.25, 0.5, -0.5]).unwrap();
        let packet = encode_pcm_packet(&frame, 1, 0, PcmEncoding::signed_16()).unwrap();
        let bytes = mux_wav_bytes(&media, &[packet]).unwrap();
        store
            .put(&source, PutPayload::from_bytes(bytes))
            .await
            .unwrap();

        let pipeline = AudioTransformPipeline::new()
            .with(AudioTransform::Gain { factor: 2.0 })
            .with(AudioTransform::NormalizePeak { target_peak: 0.5 });
        let job = ObjectAudioTransformJob::new(pipeline);

        let report = transform_object_audio_same_store(&store, &source, &target, &job)
            .await
            .unwrap();

        assert_eq!(report.audio_track_id, 1);
        assert_eq!(report.decoded_frames, 1);
        assert_eq!(report.encoded_audio_packets, 1);
        assert!(report.bytes_written > 44);

        let output = read_object_bytes(&store, &target).await.unwrap();
        let demuxed = demux_wav_bytes(&output).unwrap();
        let decoded = decode_pcm_packet(&demuxed.media.streams[0], &demuxed.packets[0]).unwrap();
        assert!((decoded.samples_f32_interleaved[0] - 0.25).abs() < 0.001);
        assert!((decoded.samples_f32_interleaved[2] - 0.5).abs() < 0.001);
    });
}

#[cfg(feature = "audio-io")]
#[test]
fn symphonia_audio_file_transform_decodes_object_bytes_to_wav() {
    block_on(async {
        let store = InMemory::new();
        let source = Path::from("audio/file-input.wav");
        let target = Path::from("audio/file-output.wav");

        let time_base = TimeBase::new(1, 44_100).unwrap();
        let mut stream = StreamInfo::new(1, MediaType::Audio, CodecId::Pcm, time_base)
            .with_audio_format(44_100, 1);
        set_pcm_tags(&mut stream, PcmEncoding::signed_16(), 2);
        let mut media = MediaInfo::default();
        media.push_stream(stream);
        let frame = AudioFrame::new(44_100, 1, 0, vec![0.125, -0.125, 0.25, -0.25]).unwrap();
        let packet = encode_pcm_packet(&frame, 1, 0, PcmEncoding::signed_16()).unwrap();
        let bytes = mux_wav_bytes(&media, &[packet]).unwrap();
        store
            .put(&source, PutPayload::from_bytes(bytes))
            .await
            .unwrap();

        let job = ObjectAudioTransformJob::new(
            AudioTransformPipeline::new().with(AudioTransform::Gain { factor: 2.0 }),
        );
        let report = transform_object_audio_file_to_wav_same_store(&store, &source, &target, &job)
            .await
            .unwrap();

        assert_eq!(report.audio_track_id, 1);
        assert_eq!(report.decoded_frames, 1);
        let output = read_object_bytes(&store, &target).await.unwrap();
        let demuxed = demux_wav_bytes(&output).unwrap();
        let decoded = decode_pcm_packet(&demuxed.media.streams[0], &demuxed.packets[0]).unwrap();
        assert!((decoded.samples_f32_interleaved[0] - 0.25).abs() < 0.001);
        assert!((decoded.samples_f32_interleaved[2] - 0.5).abs() < 0.001);
    });
}
