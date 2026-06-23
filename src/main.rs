#[cfg(feature = "cli")]
use clap::{Parser, Subcommand};

#[cfg(feature = "cli")]
#[derive(Debug, Parser)]
#[command(author, version, about)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[cfg(feature = "cli")]
#[derive(Debug, Subcommand)]
enum Command {
    /// Print compiled features and recommended backend lanes.
    Capabilities,
    /// Parse SRT/WebVTT from stdin or a file, shift it, and write SRT to stdout.
    SrtShift {
        /// Input subtitle path. Reads stdin when omitted.
        path: Option<std::path::PathBuf>,
        /// Offset in milliseconds. Negative values shift earlier and saturate at zero.
        #[arg(long)]
        offset_ms: i64,
        /// Treat input as WebVTT instead of SRT.
        #[arg(long)]
        webvtt: bool,
    },
}

#[cfg(feature = "cli")]
fn main() -> video_utils_rs::Result<()> {
    let cli = Cli::parse();

    match cli.command.unwrap_or(Command::Capabilities) {
        Command::Capabilities => {
            print_capabilities();
            Ok(())
        }
        Command::SrtShift {
            path,
            offset_ms,
            webvtt,
        } => {
            let input = read_input(path)?;
            let format = if webvtt {
                video_utils_rs::SubtitleFormat::WebVtt
            } else {
                video_utils_rs::SubtitleFormat::Srt
            };
            let events = video_utils_rs::parse_subtitles(format, &input)?;
            let shifted = video_utils_rs::shift_events(&events, offset_ms);
            print!("{}", video_utils_rs::write_srt(&shifted));
            Ok(())
        }
    }
}

#[cfg(not(feature = "cli"))]
fn main() {
    println!("{}", video_utils_rs::crate_profile());
}

#[cfg(feature = "cli")]
fn read_input(path: Option<std::path::PathBuf>) -> video_utils_rs::Result<String> {
    use std::io::Read;

    match path {
        Some(path) => std::fs::read_to_string(&path).map_err(|err| video_utils_rs::Error::Parse {
            format: "subtitle",
            message: format!("failed to read {}: {err}", path.display()),
        }),
        None => {
            let mut input = String::new();
            std::io::stdin().read_to_string(&mut input).map_err(|err| {
                video_utils_rs::Error::Parse {
                    format: "subtitle",
                    message: format!("failed to read stdin: {err}"),
                }
            })?;
            Ok(input)
        }
    }
}

#[cfg(feature = "cli")]
fn print_capabilities() {
    let features = video_utils_rs::compiled_features();
    println!("{}", video_utils_rs::crate_profile());
    println!("features:");
    println!("  packet-ops: {}", features.packet_ops);
    println!("  audio-core: {}", features.audio_core);
    println!("  audio-io: {}", features.audio_io);
    println!("  frame-core: {}", features.frame_core);
    println!("  image-io: {}", features.image_io);
    println!("  preview: {}", features.preview);
    println!("  subtitles: {}", features.subtitles);
    println!("  streaming: {}", features.streaming);
    println!("  platform-codecs: {}", features.platform_codecs);
    println!("  codec-apple: {}", features.codec_apple);
    println!("  codec-android: {}", features.codec_android);
    println!("  codec-windows: {}", features.codec_windows);
    println!("  codec-gstreamer: {}", features.codec_gstreamer);
    println!("  codec-web: {}", features.codec_web);
    println!("  codec-h264-rust: {}", features.codec_h264_rust);
    println!("  codec-h265-rust: {}", features.codec_h265_rust);
    println!("  codec-av1-rust: {}", features.codec_av1_rust);
    println!("  codec-openh264-ffi: {}", features.codec_openh264_ffi);

    let backends = video_utils_rs::recommended_backends_for_current_target();
    println!("recommended backends:");
    if backends.is_empty() {
        println!("  none compiled for this target/features");
    } else {
        for backend in backends {
            println!(
                "  {:?} ({:?}, {:?}, {:?}, hardware={})",
                backend.kind,
                backend.target,
                backend.source,
                backend.probe,
                backend.hardware_accelerated
            );
        }
    }

    println!("implemented codec surfaces:");
    for support in video_utils_rs::builtin_codec_support() {
        println!(
            "  {} {:?}: decode={}, encode={} ({})",
            support.codec, support.kind, support.can_decode, support.can_encode, support.note
        );
    }
}
