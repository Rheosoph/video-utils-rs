use crate::{
    audio::AudioFrame,
    codec::{AudioDecoder, AudioEncoder, CodecDescriptor, CodecId, VideoDecoder, VideoEncoder},
    error::{Error, Result},
    frame::RgbaFrame,
    packet::EncodedPacket,
};

/// Video decoder stub for codecs that are recognized but not compiled in.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UnsupportedVideoDecoder {
    codec: CodecId,
    reason: &'static str,
}

impl UnsupportedVideoDecoder {
    /// Create a video decoder stub for a codec.
    #[must_use]
    pub fn new(codec: CodecId, reason: &'static str) -> Self {
        Self { codec, reason }
    }
}

impl CodecDescriptor for UnsupportedVideoDecoder {
    fn name(&self) -> &'static str {
        "unsupported-video-decoder"
    }

    fn codec_id(&self) -> CodecId {
        self.codec.clone()
    }
}

impl VideoDecoder for UnsupportedVideoDecoder {
    fn decode_packet(&mut self, _packet: &EncodedPacket) -> Result<Vec<RgbaFrame>> {
        Err(Error::Unsupported {
            operation: "video decode",
            reason: self.reason,
        })
    }
}

/// Video encoder stub for codecs that are recognized but not compiled in.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UnsupportedVideoEncoder {
    codec: CodecId,
    reason: &'static str,
}

impl UnsupportedVideoEncoder {
    /// Create a video encoder stub for a codec.
    #[must_use]
    pub fn new(codec: CodecId, reason: &'static str) -> Self {
        Self { codec, reason }
    }
}

impl CodecDescriptor for UnsupportedVideoEncoder {
    fn name(&self) -> &'static str {
        "unsupported-video-encoder"
    }

    fn codec_id(&self) -> CodecId {
        self.codec.clone()
    }
}

impl VideoEncoder for UnsupportedVideoEncoder {
    fn encode_frame(&mut self, _frame: &RgbaFrame, _pts: i64) -> Result<Vec<EncodedPacket>> {
        Err(Error::Unsupported {
            operation: "video encode",
            reason: self.reason,
        })
    }
}

/// Audio decoder stub for codecs that are recognized but not compiled in.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UnsupportedAudioDecoder {
    codec: CodecId,
    reason: &'static str,
}

impl UnsupportedAudioDecoder {
    /// Create an audio decoder stub for a codec.
    #[must_use]
    pub fn new(codec: CodecId, reason: &'static str) -> Self {
        Self { codec, reason }
    }
}

impl CodecDescriptor for UnsupportedAudioDecoder {
    fn name(&self) -> &'static str {
        "unsupported-audio-decoder"
    }

    fn codec_id(&self) -> CodecId {
        self.codec.clone()
    }
}

impl AudioDecoder for UnsupportedAudioDecoder {
    fn decode_packet(&mut self, _packet: &EncodedPacket) -> Result<Vec<AudioFrame>> {
        Err(Error::Unsupported {
            operation: "audio decode",
            reason: self.reason,
        })
    }
}

/// Audio encoder stub for codecs that are recognized but not compiled in.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UnsupportedAudioEncoder {
    codec: CodecId,
    reason: &'static str,
}

impl UnsupportedAudioEncoder {
    /// Create an audio encoder stub for a codec.
    #[must_use]
    pub fn new(codec: CodecId, reason: &'static str) -> Self {
        Self { codec, reason }
    }
}

impl CodecDescriptor for UnsupportedAudioEncoder {
    fn name(&self) -> &'static str {
        "unsupported-audio-encoder"
    }

    fn codec_id(&self) -> CodecId {
        self.codec.clone()
    }
}

impl AudioEncoder for UnsupportedAudioEncoder {
    fn encode_frame(&mut self, _frame: &AudioFrame) -> Result<Vec<EncodedPacket>> {
        Err(Error::Unsupported {
            operation: "audio encode",
            reason: self.reason,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::UnsupportedVideoDecoder;
    use crate::{CodecId, EncodedPacket, Error, TimeBase, VideoDecoder};

    #[test]
    fn unsupported_decoder_reports_unsupported() {
        let packet = EncodedPacket::new(
            1,
            CodecId::H264,
            0,
            1,
            TimeBase::milliseconds(),
            vec![0, 0, 1],
        );
        let mut decoder = UnsupportedVideoDecoder::new(CodecId::H264, "test");

        assert!(matches!(
            decoder.decode_packet(&packet),
            Err(Error::Unsupported {
                operation: "video decode",
                reason: "test"
            })
        ));
    }
}
