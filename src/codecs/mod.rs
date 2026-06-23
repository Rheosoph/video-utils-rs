//! Concrete codec adapters.
//!
//! `crate::codec` owns the public codec traits and support registry. This
//! module contains the implementations that can be used directly by callers.

pub mod packet_copy;
pub mod raw_rgba;
pub mod subtitle_text;
pub mod unsupported;

#[cfg(any(
    feature = "codec-h264-rust",
    feature = "codec-h265-rust",
    all(
        any(feature = "platform-codecs", feature = "codec-windows"),
        target_os = "windows"
    )
))]
pub(crate) mod yuv420;

#[cfg(feature = "codec-av1-rust")]
pub mod av1_rav1e;
#[cfg(feature = "codec-h264-rust")]
pub mod h264_rust;
#[cfg(feature = "codec-h265-rust")]
pub mod h265_rust;

#[cfg(feature = "codec-av1-rust")]
pub use av1_rav1e::{Rav1eAv1Encoder, Rav1eAv1EncoderOptions};
#[cfg(feature = "codec-h264-rust")]
pub use h264_rust::RustH264Decoder;
#[cfg(feature = "codec-h265-rust")]
pub use h265_rust::RustH265Decoder;
#[cfg(feature = "image-io")]
pub mod image_rgba;

#[cfg(feature = "audio-io")]
pub mod symphonia_audio;

#[cfg(feature = "audio-io")]
pub mod wav_pcm;

#[cfg(feature = "image-io")]
pub use image_rgba::{ImageRgbaDecoder, ImageRgbaEncoder, ImageStillFormat};
pub use packet_copy::PacketCopyCodec;
pub use raw_rgba::{RawRgbaVideoDecoder, RawRgbaVideoEncoder};
pub use subtitle_text::SubtitleTextCodec;
#[cfg(feature = "audio-io")]
pub use symphonia_audio::{SymphoniaAudioDecoder, SymphoniaPacketAudioDecoder};
pub use unsupported::{
    UnsupportedAudioDecoder, UnsupportedAudioEncoder, UnsupportedVideoDecoder,
    UnsupportedVideoEncoder,
};
#[cfg(feature = "audio-io")]
pub use wav_pcm::{WavPcmDecoder, WavPcmEncoder, WavPcmSampleFormat};
