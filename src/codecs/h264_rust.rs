use crate::{
    bitstream::h264::h264_packet_to_annex_b,
    codec::{CodecDescriptor, CodecId, VideoDecoder},
    codecs::yuv420::yuv420_8_to_rgba,
    error::{Error, Result},
    frame::RgbaFrame,
    packet::EncodedPacket,
};
use bytes::Bytes;
use rust_h264::{
    decoder::{Frame, OrderedDecoder},
    nal::{parse_annex_b, parse_avcc_config},
};

/// Rust-native H.264/AVC decoder backed by `rust_h264`.
///
/// The backend accepts Annex-B elementary packets directly. For MP4/Matroska
/// length-prefixed AVC samples, construct it with `with_avcc_config` so the
/// decoder can read codec-private SPS/PPS data and the packet converter can
/// honor the stream's NAL length size.
pub struct RustH264Decoder {
    decoder: OrderedDecoder,
    codec_config: Option<Bytes>,
}

impl RustH264Decoder {
    /// Create a decoder for Annex-B H.264 packets.
    #[must_use]
    pub fn new() -> Self {
        Self {
            decoder: OrderedDecoder::new(),
            codec_config: None,
        }
    }

    /// Create a decoder for Annex-B H.264 packets.
    #[must_use]
    pub fn new_annex_b() -> Self {
        Self::new()
    }

    /// Create a decoder from an AVCDecoderConfigurationRecord (`avcC`).
    pub fn with_avcc_config(codec_config: impl Into<Bytes>) -> Result<Self> {
        let codec_config = codec_config.into();
        let mut decoder = Self {
            decoder: OrderedDecoder::new(),
            codec_config: Some(codec_config.clone()),
        };
        decoder.feed_avcc_config(&codec_config)?;
        Ok(decoder)
    }

    fn feed_avcc_config(&mut self, codec_config: &[u8]) -> Result<()> {
        let config = parse_avcc_config(codec_config).map_err(|message| Error::Parse {
            format: "h264",
            message: message.to_owned(),
        })?;

        for nal in config.sps_nals.iter().chain(config.pps_nals.iter()) {
            let _ = self
                .decoder
                .decode_nal(nal)
                .map_err(|err| codec_error("configure", err))?;
        }

        Ok(())
    }
}

impl Default for RustH264Decoder {
    fn default() -> Self {
        Self::new()
    }
}

impl CodecDescriptor for RustH264Decoder {
    fn name(&self) -> &'static str {
        "rust-h264-decoder"
    }

    fn codec_id(&self) -> CodecId {
        CodecId::H264
    }
}

impl VideoDecoder for RustH264Decoder {
    fn decode_packet(&mut self, packet: &EncodedPacket) -> Result<Vec<RgbaFrame>> {
        if packet.codec != CodecId::H264 {
            return Err(Error::CodecMismatch {
                expected: CodecId::H264,
                actual: packet.codec.clone(),
            });
        }

        let annex_b = h264_packet_to_annex_b(packet, self.codec_config.as_ref())?;
        let mut frames = Vec::new();

        for nal in parse_annex_b(&annex_b) {
            let decoded = self
                .decoder
                .decode_nal(&nal)
                .map_err(|err| codec_error("decode", err))?;
            for frame in decoded {
                frames.push(frame_to_rgba(frame)?);
            }
        }

        Ok(frames)
    }

    fn flush(&mut self) -> Result<Vec<RgbaFrame>> {
        self.decoder
            .flush()
            .into_iter()
            .map(frame_to_rgba)
            .collect()
    }
}

fn frame_to_rgba(frame: Frame) -> Result<RgbaFrame> {
    yuv420_8_to_rgba(frame.width, frame.height, &frame.y, &frame.u, &frame.v)
}

fn codec_error(operation: &'static str, err: rust_h264::error::DecodeError) -> Error {
    Error::CodecBackend {
        codec: CodecId::H264,
        operation,
        message: err.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::RustH264Decoder;
    use crate::{CodecDescriptor, CodecId};

    #[test]
    fn reports_h264_descriptor() {
        let decoder = RustH264Decoder::new();

        assert_eq!(decoder.name(), "rust-h264-decoder");
        assert_eq!(decoder.codec_id(), CodecId::H264);
    }
}
