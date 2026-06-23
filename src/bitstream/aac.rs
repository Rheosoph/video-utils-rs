use crate::{
    error::{Error, Result},
    packet::EncodedPacket,
};
use bytes::{Bytes, BytesMut};

/// Convert a raw AAC access unit into an ADTS-framed packet.
pub fn aac_packet_to_adts(
    packet: &EncodedPacket,
    audio_specific_config: Option<&Bytes>,
) -> Result<Bytes> {
    if is_adts(&packet.data) {
        return Ok(packet.data.clone());
    }

    let Some(config) = audio_specific_config else {
        return Err(Error::Unsupported {
            operation: "aac bitstream filter",
            reason: "raw AAC packets need AudioSpecificConfig to build ADTS headers",
        });
    };
    let config = parse_audio_specific_config(config)?;
    let frame_len = packet.data.len() + 7;
    if frame_len > 0x1fff {
        return Err(Error::Parse {
            format: "aac",
            message: "AAC frame is too large for ADTS".to_owned(),
        });
    }

    let profile = config
        .audio_object_type
        .checked_sub(1)
        .ok_or(Error::Parse {
            format: "aac",
            message: "AAC audio object type is invalid for ADTS".to_owned(),
        })?;
    if profile > 3 {
        return Err(Error::Unsupported {
            operation: "aac bitstream filter",
            reason: "only AAC Main/LC/SSR/LTP object types can be wrapped in ADTS",
        });
    }

    let mut out = BytesMut::with_capacity(frame_len);
    let len = frame_len as u16;
    out.extend_from_slice(&[
        0xff,
        0xf1,
        (profile << 6) | (config.frequency_index << 2) | (config.channel_config >> 2),
        ((config.channel_config & 0x03) << 6) | ((len >> 11) as u8),
        ((len >> 3) & 0xff) as u8,
        (((len & 0x07) as u8) << 5) | 0x1f,
        0xfc,
    ]);
    out.extend_from_slice(&packet.data);
    Ok(out.freeze())
}

/// Convert an AAC packet into a raw AAC access unit suitable for MP4/fMP4.
pub fn aac_packet_to_raw(packet: &EncodedPacket) -> Result<Bytes> {
    if !is_adts(&packet.data) {
        return Ok(packet.data.clone());
    }

    if packet.data.len() < 7 {
        return Err(Error::Parse {
            format: "aac",
            message: "ADTS header is too short".to_owned(),
        });
    }
    let protection_absent = packet.data[1] & 0x01 != 0;
    let header_len = if protection_absent { 7 } else { 9 };
    let frame_len = ((usize::from(packet.data[3] & 0x03)) << 11)
        | (usize::from(packet.data[4]) << 3)
        | (usize::from(packet.data[5] & 0xe0) >> 5);

    if frame_len < header_len || frame_len > packet.data.len() {
        return Err(Error::Parse {
            format: "aac",
            message: "ADTS frame length is invalid".to_owned(),
        });
    }
    if frame_len != packet.data.len() {
        return Err(Error::Unsupported {
            operation: "aac bitstream filter",
            reason: "ADTS packets containing multiple frames are not supported",
        });
    }

    Ok(Bytes::copy_from_slice(&packet.data[header_len..frame_len]))
}

/// Build AudioSpecificConfig and audio format metadata from an ADTS AAC frame.
pub fn audio_specific_config_from_adts(data: &[u8]) -> Result<(Bytes, u32, u16)> {
    if !is_adts(data) || data.len() < 7 {
        return Err(Error::Parse {
            format: "aac",
            message: "input is not an ADTS AAC frame".to_owned(),
        });
    }

    let audio_object_type = ((data[2] >> 6) & 0x03) + 1;
    let frequency_index = (data[2] >> 2) & 0x0f;
    let sample_rate = sample_rate_from_frequency_index(frequency_index)?;
    let channel_config = ((data[2] & 0x01) << 2) | ((data[3] >> 6) & 0x03);
    if !(1..=7).contains(&channel_config) {
        return Err(Error::Parse {
            format: "aac",
            message: "ADTS channel configuration is unsupported".to_owned(),
        });
    }

    Ok((
        build_audio_specific_config(audio_object_type, frequency_index, channel_config),
        sample_rate,
        u16::from(channel_config),
    ))
}

/// Build an AAC-LC AudioSpecificConfig from stream format metadata.
pub fn audio_specific_config_from_format(sample_rate: u32, channels: u16) -> Result<Bytes> {
    if !(1..=7).contains(&channels) {
        return Err(Error::Parse {
            format: "aac",
            message: "AAC channel count is unsupported for AudioSpecificConfig".to_owned(),
        });
    }
    let frequency_index = frequency_index_for_sample_rate(sample_rate)?;
    Ok(build_audio_specific_config(
        2,
        frequency_index,
        channels as u8,
    ))
}

fn is_adts(data: &[u8]) -> bool {
    data.len() >= 2 && data[0] == 0xff && (data[1] & 0xf0) == 0xf0
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct AacConfig {
    audio_object_type: u8,
    frequency_index: u8,
    channel_config: u8,
}

fn parse_audio_specific_config(config: &[u8]) -> Result<AacConfig> {
    let mut bits = BitReader::new(config);
    let audio_object_type = bits.read_u8(5)?;
    if audio_object_type == 31 {
        return Err(Error::Unsupported {
            operation: "aac bitstream filter",
            reason: "extended AAC object types are not supported for ADTS wrapping",
        });
    }
    let frequency_index = bits.read_u8(4)?;
    if frequency_index == 15 {
        return Err(Error::Unsupported {
            operation: "aac bitstream filter",
            reason: "explicit AAC sample rates are not supported for ADTS wrapping",
        });
    }
    let channel_config = bits.read_u8(4)?;
    if channel_config > 7 {
        return Err(Error::Parse {
            format: "aac",
            message: "AAC channel configuration is invalid".to_owned(),
        });
    }

    Ok(AacConfig {
        audio_object_type,
        frequency_index,
        channel_config,
    })
}

fn sample_rate_from_frequency_index(frequency_index: u8) -> Result<u32> {
    match frequency_index {
        0 => Ok(96_000),
        1 => Ok(88_200),
        2 => Ok(64_000),
        3 => Ok(48_000),
        4 => Ok(44_100),
        5 => Ok(32_000),
        6 => Ok(24_000),
        7 => Ok(22_050),
        8 => Ok(16_000),
        9 => Ok(12_000),
        10 => Ok(11_025),
        11 => Ok(8_000),
        12 => Ok(7_350),
        _ => Err(Error::Parse {
            format: "aac",
            message: "unsupported ADTS sample-rate index".to_owned(),
        }),
    }
}

fn frequency_index_for_sample_rate(sample_rate: u32) -> Result<u8> {
    match sample_rate {
        96_000 => Ok(0),
        88_200 => Ok(1),
        64_000 => Ok(2),
        48_000 => Ok(3),
        44_100 => Ok(4),
        32_000 => Ok(5),
        24_000 => Ok(6),
        22_050 => Ok(7),
        16_000 => Ok(8),
        12_000 => Ok(9),
        11_025 => Ok(10),
        8_000 => Ok(11),
        7_350 => Ok(12),
        _ => Err(Error::Unsupported {
            operation: "aac config",
            reason: "sample rate has no AAC frequency-index mapping",
        }),
    }
}

fn build_audio_specific_config(
    audio_object_type: u8,
    frequency_index: u8,
    channel_config: u8,
) -> Bytes {
    Bytes::from(vec![
        (audio_object_type << 3) | (frequency_index >> 1),
        ((frequency_index & 0x01) << 7) | (channel_config << 3),
    ])
}

struct BitReader<'a> {
    bytes: &'a [u8],
    bit_offset: usize,
}

impl<'a> BitReader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self {
            bytes,
            bit_offset: 0,
        }
    }

    fn read_u8(&mut self, bits: usize) -> Result<u8> {
        if bits > 8 {
            return Err(Error::Parse {
                format: "aac",
                message: "internal AAC bit-reader request exceeds u8".to_owned(),
            });
        }
        if self.bytes.len() * 8 - self.bit_offset < bits {
            return Err(Error::Parse {
                format: "aac",
                message: "AudioSpecificConfig ended early".to_owned(),
            });
        }

        let mut value = 0u8;
        for _ in 0..bits {
            let byte = self.bytes[self.bit_offset / 8];
            let shift = 7 - (self.bit_offset % 8);
            value = (value << 1) | ((byte >> shift) & 1);
            self.bit_offset += 1;
        }
        Ok(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CodecId, TimeBase};

    #[test]
    fn wraps_raw_aac_in_adts() {
        let packet = EncodedPacket::new(
            2,
            CodecId::Aac,
            0,
            1024,
            TimeBase::new(1, 48_000).unwrap(),
            Bytes::from_static(b"\x11\x22"),
        );
        let out = aac_packet_to_adts(&packet, Some(&Bytes::from_static(&[0x11, 0x90]))).unwrap();

        assert_eq!(out[0], 0xff);
        assert_eq!(out[1] & 0xf0, 0xf0);
        assert_eq!(&out[7..], b"\x11\x22");
    }

    #[test]
    fn strips_adts_for_mp4_samples() {
        let packet = EncodedPacket::new(
            2,
            CodecId::Aac,
            0,
            1024,
            TimeBase::new(1, 48_000).unwrap(),
            Bytes::from_static(b"\xff\xf1\x4c\x80\x01\x3f\xfc\x11\x22"),
        );

        let raw = aac_packet_to_raw(&packet).unwrap();

        assert_eq!(&raw[..], b"\x11\x22");
    }

    #[test]
    fn builds_audio_specific_config_from_adts() {
        let (config, sample_rate, channels) =
            audio_specific_config_from_adts(b"\xff\xf1\x4c\x80\x01\x3f\xfc\x11\x22").unwrap();

        assert_eq!(config, Bytes::from_static(&[0x11, 0x90]));
        assert_eq!(sample_rate, 48_000);
        assert_eq!(channels, 2);
    }

    #[test]
    fn builds_audio_specific_config_from_format() {
        let config = audio_specific_config_from_format(48_000, 2).unwrap();

        assert_eq!(config, Bytes::from_static(&[0x11, 0x90]));
    }
}
