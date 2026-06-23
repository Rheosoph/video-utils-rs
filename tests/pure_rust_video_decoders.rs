#![cfg(any(feature = "codec-h264-rust", feature = "codec-h265-rust"))]

use std::{
    fs,
    path::PathBuf,
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

use video_utils_rs::{CodecId, EncodedPacket, TimeBase, VideoDecoder};

#[cfg(feature = "codec-h264-rust")]
#[test]
#[ignore = "requires ffmpeg with libx264"]
fn rust_h264_decoder_decodes_real_annex_b_stream() {
    use video_utils_rs::RustH264Decoder;

    let path = unique_temp_path("h264");
    run_ffmpeg(&[
        "-hide_banner",
        "-loglevel",
        "error",
        "-f",
        "lavfi",
        "-i",
        "testsrc2=size=32x32:rate=1:duration=1",
        "-frames:v",
        "1",
        "-pix_fmt",
        "yuv420p",
        "-c:v",
        "libx264",
        "-profile:v",
        "baseline",
        "-preset",
        "ultrafast",
        "-tune",
        "zerolatency",
        "-f",
        "h264",
        path.to_str().unwrap(),
    ]);

    let bytes = fs::read(&path).unwrap();
    let _ = fs::remove_file(&path);
    let packet = EncodedPacket::new(1, CodecId::H264, 0, 1, TimeBase::new(1, 1).unwrap(), bytes)
        .with_keyframe(true);
    let mut decoder = RustH264Decoder::new_annex_b();
    let mut frames = decoder.decode_packet(&packet).unwrap();
    frames.extend(decoder.flush().unwrap());

    assert!(!frames.is_empty());
    assert_eq!(frames[0].width, 32);
    assert_eq!(frames[0].height, 32);
    assert_eq!(frames[0].data.len(), 32 * 32 * 4);
}

#[cfg(feature = "codec-h265-rust")]
#[test]
#[ignore = "requires ffmpeg with libx265"]
fn rust_h265_decoder_decodes_real_annex_b_stream() {
    use video_utils_rs::RustH265Decoder;

    let path = unique_temp_path("h265");
    run_ffmpeg(&[
        "-hide_banner",
        "-loglevel",
        "error",
        "-f",
        "lavfi",
        "-i",
        "testsrc2=size=32x32:rate=1:duration=1",
        "-frames:v",
        "1",
        "-pix_fmt",
        "yuv420p",
        "-c:v",
        "libx265",
        "-preset",
        "ultrafast",
        "-x265-params",
        "log-level=error:keyint=1:bframes=0",
        "-f",
        "hevc",
        path.to_str().unwrap(),
    ]);

    let bytes = fs::read(&path).unwrap();
    let _ = fs::remove_file(&path);
    let packet = EncodedPacket::new(1, CodecId::H265, 0, 1, TimeBase::new(1, 1).unwrap(), bytes)
        .with_keyframe(true);
    let mut decoder = RustH265Decoder::new_annex_b();
    let mut frames = decoder.decode_packet(&packet).unwrap();
    frames.extend(decoder.flush().unwrap());

    assert!(!frames.is_empty());
    assert_eq!(frames[0].width, 32);
    assert_eq!(frames[0].height, 32);
    assert_eq!(frames[0].data.len(), 32 * 32 * 4);
}

fn unique_temp_path(extension: &str) -> PathBuf {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!(
        "video-utils-rs-{}-{now}.{extension}",
        std::process::id()
    ))
}

fn run_ffmpeg(args: &[&str]) {
    let output = Command::new("ffmpeg")
        .args(args)
        .output()
        .expect("ffmpeg must be installed for this ignored test");
    assert!(
        output.status.success(),
        "ffmpeg failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}
