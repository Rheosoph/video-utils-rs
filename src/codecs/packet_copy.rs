use crate::{
    codec::{CodecDescriptor, CodecId, Decoder, Encoder},
    error::{Error, Result},
    packet::EncodedPacket,
};

/// Packet-copy adapter for already encoded packets.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PacketCopyCodec {
    codec: CodecId,
}

impl PacketCopyCodec {
    /// Create a packet-copy adapter for one codec.
    #[must_use]
    pub fn new(codec: CodecId) -> Self {
        Self { codec }
    }

    fn validate(&self, packet: &EncodedPacket) -> Result<()> {
        if packet.codec != self.codec {
            return Err(Error::CodecMismatch {
                expected: self.codec.clone(),
                actual: packet.codec.clone(),
            });
        }
        Ok(())
    }
}

impl CodecDescriptor for PacketCopyCodec {
    fn name(&self) -> &'static str {
        "packet-copy"
    }

    fn codec_id(&self) -> CodecId {
        self.codec.clone()
    }
}

impl Decoder for PacketCopyCodec {
    type Input = EncodedPacket;
    type Output = EncodedPacket;

    fn decode(&mut self, input: &Self::Input) -> Result<Self::Output> {
        self.validate(input)?;
        Ok(input.clone())
    }
}

impl Encoder for PacketCopyCodec {
    type Input = EncodedPacket;
    type Output = EncodedPacket;

    fn encode(&mut self, input: &Self::Input) -> Result<Self::Output> {
        self.validate(input)?;
        Ok(input.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::PacketCopyCodec;
    use crate::{CodecId, Decoder, EncodedPacket, Encoder, Error, TimeBase};

    #[test]
    fn round_trips_matching_packets() {
        let packet = EncodedPacket::new(
            1,
            CodecId::H264,
            0,
            1_000,
            TimeBase::milliseconds(),
            vec![1, 2, 3],
        );
        let mut codec = PacketCopyCodec::new(CodecId::H264);

        assert_eq!(codec.decode(&packet).unwrap(), packet);
        assert_eq!(codec.encode(&packet).unwrap(), packet);
    }

    #[test]
    fn rejects_wrong_codec() {
        let packet = EncodedPacket::new(
            1,
            CodecId::H265,
            0,
            1_000,
            TimeBase::milliseconds(),
            vec![1, 2, 3],
        );
        let mut codec = PacketCopyCodec::new(CodecId::H264);

        assert!(matches!(
            codec.decode(&packet),
            Err(Error::CodecMismatch { expected, actual })
                if expected == CodecId::H264 && actual == CodecId::H265
        ));
    }
}
