use video_utils_rs::{
    CodecId, CodecImplementationKind, CodecRegistry, platform_codec_api_names,
    platform_delegated_codec_ids,
};

#[cfg(feature = "platform-codecs")]
use video_utils_rs::{
    BackendKind, BackendProbe, BackendSource, CodecDirection, PlatformAudioDecoder,
    PlatformAudioDecoderConfig, PlatformVideoDecoder, PlatformVideoDecoderConfig,
    preferred_backend_for_codec, probe_platform_codec, recommended_backends_for_current_target,
};

#[cfg(all(feature = "platform-codecs", target_os = "macos"))]
use video_utils_rs::{
    AudioDecoder, AudioEncoder, AudioFrame, PlatformAudioEncoder, PlatformAudioEncoderConfig,
};

#[cfg(all(
    feature = "platform-codecs",
    any(target_os = "linux", target_os = "macos", target_os = "windows")
))]
use video_utils_rs::{
    PlatformVideoEncoder, PlatformVideoEncoderConfig, RgbaFrame, TimeBase, VideoEncoder,
};

#[cfg(all(
    feature = "platform-codecs",
    any(target_os = "linux", target_os = "macos")
))]
use video_utils_rs::VideoDecoder;

#[test]
fn platform_delegated_codec_set_covers_policy_sensitive_codecs() {
    let codecs = platform_delegated_codec_ids();

    for codec in [
        CodecId::Dts,
        CodecId::Wma,
        CodecId::ProRes,
        CodecId::Aac,
        CodecId::Eac3,
        CodecId::H264,
        CodecId::H265,
    ] {
        assert!(
            codecs.contains(&codec),
            "{codec} should be platform delegated"
        );
    }
}

#[test]
fn platform_api_names_cover_native_adapters() {
    let h264 = platform_codec_api_names(&CodecId::H264);
    assert_eq!(h264.apple, Some("kCMVideoCodecType_H264"));
    assert_eq!(h264.android_mime, Some("video/avc"));
    assert_eq!(h264.web_codecs, Some("avc1.* or avc3.*"));
    assert_eq!(h264.windows_media_foundation, Some("MFVideoFormat_H264"));
    assert_eq!(h264.gstreamer_caps, Some("video/x-h264"));

    let h265 = platform_codec_api_names(&CodecId::H265);
    assert_eq!(h265.apple, Some("kCMVideoCodecType_HEVC"));
    assert_eq!(h265.android_mime, Some("video/hevc"));
    assert_eq!(h265.web_codecs, Some("hev1.* or hvc1.*"));
    assert_eq!(h265.windows_media_foundation, Some("MFVideoFormat_HEVC"));
    assert_eq!(h265.gstreamer_caps, Some("video/x-h265"));

    let aac = platform_codec_api_names(&CodecId::Aac);
    assert_eq!(aac.apple, Some("kAudioFormatMPEG4AAC"));
    assert_eq!(aac.android_mime, Some("audio/mp4a-latm"));
    assert_eq!(aac.web_codecs, Some("mp4a.*"));
    assert_eq!(aac.windows_media_foundation, Some("MFAudioFormat_AAC"));

    let eac3 = platform_codec_api_names(&CodecId::Eac3);
    assert_eq!(eac3.apple, Some("kAudioFormatEnhancedAC3"));
    assert_eq!(eac3.android_mime, Some("audio/eac3"));
    assert_eq!(
        eac3.windows_media_foundation,
        Some("MFAudioFormat_Dolby_DDPlus")
    );

    let dts = platform_codec_api_names(&CodecId::Dts);
    assert_eq!(dts.android_mime, Some("audio/vnd.dts"));
    assert_eq!(dts.windows_media_foundation, Some("MFAudioFormat_DTS"));

    let wma = platform_codec_api_names(&CodecId::Wma);
    assert_eq!(
        wma.windows_media_foundation,
        Some("MFAudioFormat_WMAudioV8/MFAudioFormat_WMAudioV9")
    );
    assert_eq!(wma.gstreamer_caps, Some("audio/x-wma"));
}

#[cfg(feature = "platform-codecs")]
#[test]
fn platform_feature_reports_runtime_probed_current_target_backends() {
    let backends = recommended_backends_for_current_target();

    if cfg!(target_os = "linux") {
        assert_platform_backend(&backends, BackendKind::GStreamer);
        assert_eq!(
            preferred_backend_for_codec(&CodecId::H264, CodecDirection::Decode),
            Some(BackendKind::GStreamer)
        );
    } else if cfg!(target_os = "windows") {
        assert_platform_backend(&backends, BackendKind::WindowsMediaFoundation);
        assert_eq!(
            preferred_backend_for_codec(&CodecId::Wma, CodecDirection::Decode),
            Some(BackendKind::WindowsMediaFoundation)
        );
    } else if cfg!(any(
        target_os = "macos",
        target_os = "ios",
        target_os = "tvos"
    )) {
        assert_platform_backend(&backends, BackendKind::AppleVideoToolbox);
        assert_platform_backend(&backends, BackendKind::AppleAudioToolbox);
        assert_eq!(
            preferred_backend_for_codec(&CodecId::ProRes, CodecDirection::Decode),
            Some(BackendKind::AppleVideoToolbox)
        );
    } else if cfg!(target_os = "android") {
        assert_platform_backend(&backends, BackendKind::AndroidMediaCodec);
    } else if cfg!(target_family = "wasm") {
        assert_platform_backend(&backends, BackendKind::WebCodecs);
    }
}

#[cfg(feature = "platform-codecs")]
fn assert_platform_backend(backends: &[video_utils_rs::BackendCapability], kind: BackendKind) {
    let backend = backends
        .iter()
        .find(|backend| backend.kind == kind)
        .unwrap_or_else(|| panic!("{kind:?} backend should be recommended"));

    assert_eq!(backend.source, BackendSource::Platform);
    assert_eq!(backend.probe, BackendProbe::Runtime);
}

#[test]
fn registry_keeps_platform_backends_separate_from_bundled_backends() {
    let registry = CodecRegistry::builtin();

    assert!(registry.supports_decode(&CodecId::H264, CodecImplementationKind::PacketCopy));

    #[cfg(feature = "platform-codecs")]
    if cfg!(any(
        target_os = "android",
        target_os = "linux",
        target_os = "macos",
        target_os = "ios",
        target_os = "tvos",
        target_os = "windows",
        target_family = "wasm"
    )) {
        assert!(registry.supports_decode(&CodecId::H264, CodecImplementationKind::PlatformBackend));
    }

    #[cfg(not(feature = "platform-codecs"))]
    assert!(!registry.supports_decode(&CodecId::H264, CodecImplementationKind::PlatformBackend));
}

#[cfg(feature = "platform-codecs")]
#[test]
fn platform_probe_uses_real_current_target_adapter() {
    let probe = probe_platform_codec(&CodecId::H264, CodecDirection::Decode);
    assert_eq!(probe.codec, CodecId::H264);
    assert_eq!(probe.direction, CodecDirection::Decode);
    assert!(!probe.detail.is_empty());

    if cfg!(target_os = "linux") {
        assert_eq!(probe.backend, Some(BackendKind::GStreamer));
    } else if cfg!(target_os = "windows") {
        assert_eq!(probe.backend, Some(BackendKind::WindowsMediaFoundation));
    } else if cfg!(any(
        target_os = "macos",
        target_os = "ios",
        target_os = "tvos"
    )) {
        assert_eq!(probe.backend, Some(BackendKind::AppleVideoToolbox));
    } else if cfg!(target_os = "android") {
        assert_eq!(probe.backend, Some(BackendKind::AndroidMediaCodec));
    } else if cfg!(target_family = "wasm") {
        assert_eq!(probe.backend, Some(BackendKind::WebCodecs));
    }
}

#[cfg(feature = "platform-codecs")]
#[test]
fn platform_adapters_validate_media_type_before_opening_backend() {
    let err = match PlatformVideoDecoder::new(PlatformVideoDecoderConfig::new(CodecId::Aac, 16, 16))
    {
        Ok(_) => panic!("audio codec must not open as a video decoder"),
        Err(err) => err,
    };
    assert!(err.to_string().contains("not a video codec"));

    let err = match PlatformAudioDecoder::new(PlatformAudioDecoderConfig::new(
        CodecId::H264,
        48_000,
        2,
    )) {
        Ok(_) => panic!("video codec must not open as an audio decoder"),
        Err(err) => err,
    };
    assert!(err.to_string().contains("not an audio codec"));
}

#[cfg(feature = "platform-codecs")]
#[test]
fn successful_platform_probe_can_open_matching_native_handle() {
    let probe = probe_platform_codec(&CodecId::H264, CodecDirection::Decode);
    if !probe.supported {
        return;
    }

    let decoder = PlatformVideoDecoder::new(PlatformVideoDecoderConfig::new(CodecId::H264, 16, 16))
        .expect("successful H.264 platform probe should open a decoder handle");
    assert_eq!(Some(decoder.backend()), probe.backend);
}

#[cfg(all(feature = "platform-codecs", target_os = "linux"))]
#[test]
fn gstreamer_platform_backend_encodes_and_decodes_h264_frame() {
    let encode_probe = probe_platform_codec(&CodecId::H264, CodecDirection::Encode);
    let decode_probe = probe_platform_codec(&CodecId::H264, CodecDirection::Decode);
    if !encode_probe.supported || !decode_probe.supported {
        return;
    }

    let time_base = TimeBase::milliseconds();
    let frame = RgbaFrame::solid(16, 16, [24, 120, 210, 255]);
    let mut encoder = PlatformVideoEncoder::new(PlatformVideoEncoderConfig::new(
        CodecId::H264,
        16,
        16,
        time_base,
        33,
    ))
    .expect("successful H.264 GStreamer encode probe should open an encoder");

    let mut packets = encoder
        .encode_frame(&frame, 0)
        .expect("GStreamer should encode one RGBA frame into H.264");
    packets.extend(
        encoder
            .finish()
            .expect("GStreamer should flush delayed H.264 packets"),
    );
    assert!(
        !packets.is_empty(),
        "GStreamer H.264 encoder should emit packets after EOS"
    );

    let mut decoder =
        PlatformVideoDecoder::new(PlatformVideoDecoderConfig::new(CodecId::H264, 16, 16))
            .expect("successful H.264 GStreamer decode probe should open a decoder");
    let mut decoded = Vec::new();
    for packet in &packets {
        decoded.extend(
            decoder
                .decode_packet(packet)
                .expect("GStreamer should decode encoder output"),
        );
    }
    decoded.extend(
        decoder
            .flush()
            .expect("GStreamer should flush delayed decoded frames"),
    );

    assert!(
        !decoded.is_empty(),
        "GStreamer H.264 decoder should emit a frame"
    );
    assert_eq!(decoded[0].width, 16);
    assert_eq!(decoded[0].height, 16);
    assert_eq!(decoded[0].stride, 16 * 4);
}

#[cfg(all(
    feature = "platform-codecs",
    any(target_os = "macos", target_os = "windows")
))]
#[test]
fn native_platform_video_encoder_emits_h264_packet_when_probe_succeeds() {
    let encode_probe = probe_platform_codec(&CodecId::H264, CodecDirection::Encode);
    if !encode_probe.supported {
        return;
    }

    let time_base = TimeBase::milliseconds();
    let frame = RgbaFrame::solid(64, 64, [24, 120, 210, 255]);
    let mut encoder = PlatformVideoEncoder::new(PlatformVideoEncoderConfig::new(
        CodecId::H264,
        64,
        64,
        time_base,
        33,
    ))
    .expect("successful H.264 platform encode probe should open an encoder");

    let mut packets = encoder
        .encode_frame(&frame, 0)
        .expect("native H.264 encoder should accept one RGBA frame");
    packets.extend(
        encoder
            .finish()
            .expect("native H.264 encoder should flush delayed packets"),
    );

    assert!(
        !packets.is_empty(),
        "native H.264 encoder should emit at least one packet"
    );
}

#[cfg(all(feature = "platform-codecs", target_os = "macos"))]
#[test]
fn apple_videotoolbox_h264_encoder_decoder_round_trips_frame_when_probe_succeeds() {
    let encode_probe = probe_platform_codec(&CodecId::H264, CodecDirection::Encode);
    let decode_probe = probe_platform_codec(&CodecId::H264, CodecDirection::Decode);
    if !encode_probe.supported || !decode_probe.supported {
        return;
    }

    let time_base = TimeBase::milliseconds();
    let frame = RgbaFrame::solid(64, 64, [24, 120, 210, 255]);
    let mut encoder = PlatformVideoEncoder::new(PlatformVideoEncoderConfig::new(
        CodecId::H264,
        64,
        64,
        time_base,
        33,
    ))
    .expect("successful H.264 VideoToolbox encode probe should open an encoder");

    let mut packets = encoder
        .encode_frame(&frame, 0)
        .expect("VideoToolbox should accept one RGBA frame for H.264 encode");
    packets.extend(
        encoder
            .finish()
            .expect("VideoToolbox should flush delayed H.264 packets"),
    );
    assert!(
        !packets.is_empty(),
        "VideoToolbox H.264 encoder should emit at least one packet"
    );
    let codec_config = encoder
        .codec_config()
        .expect("VideoToolbox H.264 encoder should expose avcC config");

    let mut decoder = PlatformVideoDecoder::new(
        PlatformVideoDecoderConfig::new(CodecId::H264, 64, 64).with_extra_data(codec_config),
    )
    .expect("successful H.264 VideoToolbox decode probe should open a decoder");
    let mut decoded = Vec::new();
    for packet in &packets {
        decoded.extend(
            decoder
                .decode_packet(packet)
                .expect("VideoToolbox should decode its H.264 encoder output"),
        );
    }
    decoded.extend(
        decoder
            .flush()
            .expect("VideoToolbox should flush delayed decoded frames"),
    );

    assert!(
        !decoded.is_empty(),
        "VideoToolbox H.264 decoder should emit decoded frames"
    );
    assert_eq!(decoded[0].width, 64);
    assert_eq!(decoded[0].height, 64);
    assert_eq!(decoded[0].stride, 64 * 4);
}

#[cfg(all(feature = "platform-codecs", target_os = "macos"))]
#[test]
fn apple_audiotoolbox_aac_encoder_decoder_round_trips_pcm_when_probe_succeeds() {
    let encode_probe = probe_platform_codec(&CodecId::Aac, CodecDirection::Encode);
    let decode_probe = probe_platform_codec(&CodecId::Aac, CodecDirection::Decode);
    if !encode_probe.supported || !decode_probe.supported {
        return;
    }

    let sample_rate = 48_000;
    let channels = 2;
    let samples = vec![0.0; 1024 * channels as usize];
    let frame =
        AudioFrame::new(sample_rate, channels, 0, samples).expect("test PCM frame should be valid");
    let mut encoder = PlatformAudioEncoder::new(PlatformAudioEncoderConfig::new(
        CodecId::Aac,
        sample_rate,
        channels,
    ))
    .expect("successful AAC AudioToolbox encode probe should open an encoder");

    let mut packets = encoder
        .encode_frame(&frame)
        .expect("AudioToolbox should accept one PCM frame for AAC encode");
    packets.extend(
        encoder
            .finish()
            .expect("AudioToolbox should flush delayed AAC packets"),
    );
    assert!(
        !packets.is_empty(),
        "AudioToolbox AAC encoder should emit at least one packet"
    );

    let mut decoder = PlatformAudioDecoder::new(PlatformAudioDecoderConfig::new(
        CodecId::Aac,
        sample_rate,
        channels,
    ))
    .expect("successful AAC AudioToolbox decode probe should open a decoder");
    let mut decoded = Vec::new();
    for packet in &packets {
        decoded.extend(
            decoder
                .decode_packet(packet)
                .expect("AudioToolbox should decode its AAC encoder output"),
        );
    }
    decoded.extend(
        decoder
            .flush()
            .expect("AudioToolbox should flush AAC decoder state"),
    );

    assert!(
        !decoded.is_empty(),
        "AudioToolbox AAC decoder should emit decoded PCM"
    );
    assert_eq!(decoded[0].sample_rate, sample_rate);
    assert_eq!(decoded[0].channels, channels);
    assert!(decoded[0].sample_frames() > 0);
}
