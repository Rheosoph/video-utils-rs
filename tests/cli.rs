#![cfg(feature = "cli")]

use std::{
    io::Write,
    path::PathBuf,
    process::{Command, Stdio},
};

fn bin() -> PathBuf {
    if let Ok(path) = std::env::var("CARGO_BIN_EXE_video-utils") {
        return PathBuf::from(path);
    }

    let mut path = std::env::current_exe().expect("current test exe path");
    path.pop();
    if path.ends_with("deps") {
        path.pop();
    }
    path.push(format!("video-utils{}", std::env::consts::EXE_SUFFIX));
    path
}

#[test]
fn capabilities_command_prints_feature_summary() {
    let output = Command::new(bin())
        .arg("capabilities")
        .output()
        .expect("run video-utils capabilities");

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("video-utils-rs portable media core"));
    assert!(stdout.contains("packet-ops: true"));
    assert!(stdout.contains("implemented codec surfaces:"));
    assert!(stdout.contains("srt TextSubtitle: decode=true, encode=true"));
    assert!(stdout.contains("h264 PacketCopy: decode=true, encode=true"));
}

#[test]
fn srt_shift_reads_stdin_and_writes_shifted_srt() {
    let mut child = Command::new(bin())
        .args(["srt-shift", "--offset-ms", "250"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn video-utils srt-shift");

    child
        .stdin
        .as_mut()
        .expect("stdin")
        .write_all(b"1\n00:00:01,000 --> 00:00:02,000\nhello\n\n")
        .unwrap();

    let output = child.wait_with_output().expect("wait for srt-shift");

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("00:00:01,250 --> 00:00:02,250"));
    assert!(stdout.contains("hello"));
}

#[test]
fn srt_shift_accepts_webvtt_input() {
    let mut child = Command::new(bin())
        .args(["srt-shift", "--webvtt", "--offset-ms=-500"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn video-utils srt-shift webvtt");

    child
        .stdin
        .as_mut()
        .expect("stdin")
        .write_all(b"WEBVTT\n\n00:00:01.000 --> 00:00:02.000\nhello\n\n")
        .unwrap();

    let output = child.wait_with_output().expect("wait for srt-shift");

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("00:00:00,500 --> 00:00:01,500"));
}
