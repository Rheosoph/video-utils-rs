use crate::{
    codec::{CodecDescriptor, CodecId, VideoDecoder, VideoEncoder},
    error::{Error, Result},
    frame::RgbaFrame,
    packet::EncodedPacket,
    time::TimeBase,
};
use bytes::BytesMut;

/// Decoder for tightly-packed raw RGBA video packets.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RawRgbaVideoDecoder {
    width: u32,
    height: u32,
}

impl RawRgbaVideoDecoder {
    /// Create a raw RGBA decoder for the declared frame size.
    #[must_use]
    pub const fn new(width: u32, height: u32) -> Self {
        Self { width, height }
    }
}

impl CodecDescriptor for RawRgbaVideoDecoder {
    fn name(&self) -> &'static str {
        "raw-rgba-video-decoder"
    }

    fn codec_id(&self) -> CodecId {
        CodecId::RawVideo
    }
}

impl VideoDecoder for RawRgbaVideoDecoder {
    fn decode_packet(&mut self, packet: &EncodedPacket) -> Result<Vec<RgbaFrame>> {
        if packet.codec != CodecId::RawVideo {
            return Err(Error::CodecMismatch {
                expected: CodecId::RawVideo,
                actual: packet.codec.clone(),
            });
        }

        let stride = self.width as usize * 4;
        let expected = stride * self.height as usize;
        if packet.data.len() != expected {
            return Err(Error::InvalidFrameBuffer {
                expected,
                actual: packet.data.len(),
            });
        }

        Ok(vec![RgbaFrame::new(
            self.width,
            self.height,
            stride,
            packet.data.to_vec(),
        )?])
    }
}

/// Encoder for tightly-packed raw RGBA video packets.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RawRgbaVideoEncoder {
    track_id: u32,
    time_base: TimeBase,
    frame_duration: i64,
}

impl RawRgbaVideoEncoder {
    /// Create a raw RGBA encoder for one output track.
    #[must_use]
    pub const fn new(track_id: u32, time_base: TimeBase, frame_duration: i64) -> Self {
        Self {
            track_id,
            time_base,
            frame_duration,
        }
    }
}

impl CodecDescriptor for RawRgbaVideoEncoder {
    fn name(&self) -> &'static str {
        "raw-rgba-video-encoder"
    }

    fn codec_id(&self) -> CodecId {
        CodecId::RawVideo
    }
}

impl VideoEncoder for RawRgbaVideoEncoder {
    fn encode_frame(&mut self, frame: &RgbaFrame, pts: i64) -> Result<Vec<EncodedPacket>> {
        let row_len = frame.width as usize * 4;
        let mut data = BytesMut::with_capacity(row_len * frame.height as usize);
        for row in 0..frame.height as usize {
            let start = row * frame.stride;
            data.extend_from_slice(&frame.data[start..start + row_len]);
        }

        Ok(vec![
            EncodedPacket::new(
                self.track_id,
                CodecId::RawVideo,
                pts,
                self.frame_duration,
                self.time_base,
                data.freeze(),
            )
            .with_keyframe(true),
        ])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raw_rgba_video_round_trips_frame_packets() {
        let frame = RgbaFrame::solid(2, 1, [10, 20, 30, 255]);
        let mut encoder = RawRgbaVideoEncoder::new(1, TimeBase::milliseconds(), 33);
        let packets = encoder.encode_frame(&frame, 100).unwrap();
        let mut decoder = RawRgbaVideoDecoder::new(2, 1);

        let decoded = decoder.decode_packet(&packets[0]).unwrap();

        assert_eq!(packets[0].codec, CodecId::RawVideo);
        assert_eq!(packets[0].pts, 100);
        assert_eq!(decoded, vec![frame]);
    }
}
