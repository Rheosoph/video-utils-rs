use crate::{
    bitstream::h265::h265_packet_to_annex_b,
    codec::{CodecDescriptor, CodecId, VideoDecoder},
    codecs::yuv420::{yuv420_8_to_rgba, yuv420_16_to_rgba},
    error::{Error, Result},
    frame::RgbaFrame,
    packet::EncodedPacket,
};
use bytes::Bytes;
use rust_h265::{Decoder, Frame, PixelData, parse_annex_b};

/// Rust-native H.265/HEVC decoder backed by `rust_h265`.
///
/// The upstream decoder accepts Annex-B HEVC NAL units. This adapter also
/// accepts length-prefixed MP4/Matroska samples when constructed with
/// `with_hvcc_config`, converting packets to Annex-B before decode.
pub struct RustH265Decoder {
    decoder: Decoder,
    codec_config: Option<Bytes>,
    buffer: Vec<(u32, Frame)>,
    gop_id: u32,
    max_depth: usize,
}

impl RustH265Decoder {
    /// Create a decoder for Annex-B HEVC packets.
    #[must_use]
    pub fn new() -> Self {
        Self {
            decoder: Decoder::new(),
            codec_config: None,
            buffer: Vec::new(),
            gop_id: 0,
            max_depth: 16,
        }
    }

    /// Create a decoder for Annex-B HEVC packets.
    #[must_use]
    pub fn new_annex_b() -> Self {
        Self::new()
    }

    /// Create a decoder from an HEVCDecoderConfigurationRecord (`hvcC`).
    #[must_use]
    pub fn with_hvcc_config(codec_config: impl Into<Bytes>) -> Self {
        Self {
            codec_config: Some(codec_config.into()),
            ..Self::new()
        }
    }

    /// Set the maximum display-order reorder buffer depth.
    #[must_use]
    pub fn with_max_depth(mut self, max_depth: usize) -> Self {
        self.max_depth = max_depth.max(1);
        self
    }

    fn push_decoded_frame(&mut self, frame: Frame) {
        self.buffer.push((self.gop_id, frame));
    }

    fn drain_completed_gops(&mut self) -> Vec<Frame> {
        let mut ready = Vec::new();
        let mut pending = Vec::with_capacity(self.buffer.len());

        for (gop, frame) in self.buffer.drain(..) {
            if gop < self.gop_id {
                ready.push((gop, frame));
            } else {
                pending.push((gop, frame));
            }
        }

        self.buffer = pending;
        ready.sort_by_key(|(gop, frame)| (*gop, frame.pic_order_cnt));
        ready.into_iter().map(|(_, frame)| frame).collect()
    }

    fn pop_lowest(&mut self) -> Frame {
        let index = self
            .buffer
            .iter()
            .enumerate()
            .min_by_key(|(_, (gop, frame))| (*gop, frame.pic_order_cnt))
            .map(|(index, _)| index)
            .expect("buffer is non-empty when popping");
        self.buffer.remove(index).1
    }
}

impl Default for RustH265Decoder {
    fn default() -> Self {
        Self::new()
    }
}

impl CodecDescriptor for RustH265Decoder {
    fn name(&self) -> &'static str {
        "rust-h265-decoder"
    }

    fn codec_id(&self) -> CodecId {
        CodecId::H265
    }
}

impl VideoDecoder for RustH265Decoder {
    fn decode_packet(&mut self, packet: &EncodedPacket) -> Result<Vec<RgbaFrame>> {
        if packet.codec != CodecId::H265 {
            return Err(Error::CodecMismatch {
                expected: CodecId::H265,
                actual: packet.codec.clone(),
            });
        }

        let annex_b = h265_packet_to_annex_b(packet, self.codec_config.as_ref())?;
        let mut output = Vec::new();

        for nal in parse_annex_b(&annex_b) {
            let is_irap = nal.nal_unit_type.is_irap();
            if let Some(frame) = self
                .decoder
                .decode_nal(&nal)
                .map_err(|err| codec_error("decode", err))?
            {
                self.push_decoded_frame(frame);
            }

            if is_irap {
                self.gop_id += 1;
                output.extend(self.drain_completed_gops().into_iter().map(frame_to_rgba));
            }

            while self.buffer.len() > self.max_depth {
                output.push(frame_to_rgba(self.pop_lowest()));
            }
        }

        output.into_iter().collect()
    }

    fn flush(&mut self) -> Result<Vec<RgbaFrame>> {
        while let Some(frame) = self.decoder.flush() {
            self.push_decoded_frame(frame);
        }

        self.buffer
            .sort_by_key(|(gop, frame)| (*gop, frame.pic_order_cnt));
        self.buffer
            .drain(..)
            .map(|(_, frame)| frame_to_rgba(frame))
            .collect()
    }
}

fn frame_to_rgba(frame: Frame) -> Result<RgbaFrame> {
    match (&frame.y, &frame.u, &frame.v) {
        (PixelData::U8(y), PixelData::U8(u), PixelData::U8(v)) => {
            yuv420_8_to_rgba(frame.width, frame.height, y, u, v)
        }
        (PixelData::U16(y), PixelData::U16(u), PixelData::U16(v)) => {
            yuv420_16_to_rgba(frame.width, frame.height, frame.bit_depth, y, u, v)
        }
        _ => Err(Error::CodecBackend {
            codec: CodecId::H265,
            operation: "decode",
            message: "decoded HEVC planes have inconsistent bit depths".to_owned(),
        }),
    }
}

fn codec_error(operation: &'static str, err: rust_h265::DecodeError) -> Error {
    Error::CodecBackend {
        codec: CodecId::H265,
        operation,
        message: err.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::RustH265Decoder;
    use crate::{CodecDescriptor, CodecId};

    #[test]
    fn reports_h265_descriptor() {
        let decoder = RustH265Decoder::new();

        assert_eq!(decoder.name(), "rust-h265-decoder");
        assert_eq!(decoder.codec_id(), CodecId::H265);
    }
}
