//! Concrete container adapters backed by pure Rust parser crates.

pub mod aiff;
pub mod elementary;
pub mod flv;
pub mod fmp4;
pub mod iso_bmff;
pub mod iso_bmff_mux;
pub mod matroska;
pub mod matroska_mux;
pub mod mpeg_ts;
pub mod ogg;
pub mod wav;

pub use aiff::{AiffDemuxer, AiffMuxer, demux_aiff_bytes, mux_aiff_bytes, probe_aiff_bytes};
pub use elementary::{
    ElementaryDemuxer, ElementaryMuxer, demux_elementary_bytes, demux_elementary_bytes_from_path,
    detect_elementary_codec_from_extension, detect_elementary_codec_from_path,
    mux_elementary_bytes,
};
pub use flv::{FlvDemuxer, FlvMuxer, demux_flv_bytes, mux_flv_bytes, probe_flv_bytes};
pub use fmp4::{
    FragmentedMp4Demuxer, FragmentedMp4Output, demux_fragmented_mp4_bytes,
    demux_fragmented_mp4_segments, mux_fragmented_mp4_segments, probe_fragmented_mp4_bytes,
};
pub use iso_bmff::{IsoBmffDemuxer, demux_iso_bmff_bytes, probe_iso_bmff_bytes};
pub use iso_bmff_mux::{IsoBmffMuxer, mux_iso_bmff_bytes};
pub use matroska::{MatroskaDemuxer, demux_matroska_bytes, probe_matroska_bytes};
pub use matroska_mux::{MatroskaMuxer, mux_matroska_bytes};
pub use mpeg_ts::{MpegTsDemuxer, MpegTsMuxer, demux_mpeg_ts_bytes, mux_mpeg_ts_bytes};
pub use ogg::{OggDemuxer, OggMuxer, demux_ogg_bytes, mux_ogg_bytes, probe_ogg_bytes};
pub use wav::{
    PcmEncoding, PcmSampleFormat, WavDemuxer, WavMuxer, decode_pcm_packet, demux_wav_bytes,
    encode_pcm_packet, mux_wav_bytes, pcm_encoding_from_stream, probe_wav_bytes, set_pcm_tags,
};
