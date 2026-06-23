use std::{
    fs,
    path::PathBuf,
    process::{Command, Stdio},
    time::{SystemTime, UNIX_EPOCH},
};

#[test]
#[ignore = "requires local ffmpeg/ffprobe and is intended as a fixture-level smoke test"]
fn ffmpeg_can_generate_soft_subtitled_mp4_fixture() {
    let dir = unique_temp_dir();
    fs::create_dir_all(&dir).unwrap();
    let subtitles = dir.join("sample.srt");
    let output = dir.join("sample_with_subs.mp4");
    fs::write(
        &subtitles,
        "1\n00:00:00,100 --> 00:00:00,900\nhello fixture\n\n",
    )
    .unwrap();

    let status = Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-y",
            "-f",
            "lavfi",
            "-i",
            "testsrc=size=128x72:rate=10:duration=1",
            "-i",
            subtitles.to_str().unwrap(),
            "-map",
            "0:v:0",
            "-map",
            "1:s:0",
            "-c:v",
            "mpeg4",
            "-q:v",
            "5",
            "-c:s",
            "mov_text",
            output.to_str().unwrap(),
        ])
        .stdin(Stdio::null())
        .status()
        .expect("run ffmpeg");

    assert!(status.success());
    assert!(output.exists());

    let probe = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-select_streams",
            "s:0",
            "-show_entries",
            "stream=codec_name",
            "-of",
            "default=noprint_wrappers=1:nokey=1",
            output.to_str().unwrap(),
        ])
        .output()
        .expect("run ffprobe");

    assert!(probe.status.success());
    assert_eq!(String::from_utf8(probe.stdout).unwrap().trim(), "mov_text");
}

fn unique_temp_dir() -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!("video-utils-rs-ffmpeg-{nanos}"))
}
