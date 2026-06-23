use crate::{
    bitstream::h264::{
        AnnexBNalIter, annex_b_sample_to_length_prefixed, is_annex_b,
        length_prefixed_sample_to_annex_b, validate_length_prefixed_sample,
    },
    error::{Error, Result},
    packet::EncodedPacket,
};
use bytes::{Bytes, BytesMut};

const START_CODE: &[u8] = &[0, 0, 0, 1];

/// Convert an H.265/HEVC packet into Annex-B form suitable for elementary writers.
pub fn h265_packet_to_annex_b(packet: &EncodedPacket, hvcc: Option<&Bytes>) -> Result<Bytes> {
    let length_size = hvcc
        .and_then(|config| hvcc_length_size(config).ok())
        .unwrap_or(4);
    let frame = if is_annex_b(&packet.data) {
        packet.data.clone()
    } else {
        length_prefixed_sample_to_annex_b(&packet.data, length_size, "h265")?
    };

    if packet.is_keyframe {
        prepend_missing_parameter_sets(frame, hvcc)
    } else {
        Ok(frame)
    }
}

/// Convert an H.265/HEVC packet into length-prefixed form suitable for Matroska.
pub fn h265_packet_to_length_prefixed(packet: &EncodedPacket, hvcc: &Bytes) -> Result<Bytes> {
    let length_size = hvcc_length_size(hvcc)?;
    if is_annex_b(&packet.data) {
        return annex_b_sample_to_length_prefixed(&packet.data, length_size, "h265", |nal| {
            nal.len() >= 2 && !matches!((nal[0] >> 1) & 0x3f, 32..=34)
        });
    }

    validate_length_prefixed_sample(&packet.data, length_size, "h265")?;
    Ok(packet.data.clone())
}

/// Return the NAL length prefix size declared by an HEVCDecoderConfigurationRecord.
pub fn hvcc_length_size(hvcc: &[u8]) -> Result<usize> {
    if hvcc.len() < 22 {
        return Err(Error::Parse {
            format: "h265",
            message: "HEVC decoder config is too short".to_owned(),
        });
    }

    Ok(usize::from((hvcc[21] & 0x03) + 1))
}

fn prepend_missing_parameter_sets(frame: Bytes, hvcc: Option<&Bytes>) -> Result<Bytes> {
    if annex_b_contains_nal_type(&frame, 32)
        && annex_b_contains_nal_type(&frame, 33)
        && annex_b_contains_nal_type(&frame, 34)
    {
        return Ok(frame);
    }

    let Some(hvcc) = hvcc else {
        return Ok(frame);
    };
    let parameter_sets = hvcc_parameter_sets(hvcc)?;
    if parameter_sets.is_empty() {
        return Ok(frame);
    }

    let mut out = BytesMut::with_capacity(
        frame.len()
            + parameter_sets
                .iter()
                .map(|nal| START_CODE.len() + nal.len())
                .sum::<usize>(),
    );
    for nal in parameter_sets {
        out.extend_from_slice(START_CODE);
        out.extend_from_slice(&nal);
    }
    out.extend_from_slice(&frame);
    Ok(out.freeze())
}

pub(crate) fn hvcc_parameter_sets(hvcc: &[u8]) -> Result<Vec<Bytes>> {
    if hvcc.len() < 23 {
        return Err(Error::Parse {
            format: "h265",
            message: "HEVC decoder config is too short for parameter sets".to_owned(),
        });
    }

    let mut out = Vec::new();
    let mut offset = 22;
    let array_count = hvcc[offset];
    offset += 1;

    for _ in 0..array_count {
        if hvcc.len().saturating_sub(offset) < 3 {
            return Err(Error::Parse {
                format: "h265",
                message: "HEVC decoder config ended inside a parameter-set array".to_owned(),
            });
        }
        let nal_type = hvcc[offset] & 0x3f;
        let nal_count = u16::from_be_bytes([hvcc[offset + 1], hvcc[offset + 2]]);
        offset += 3;

        for _ in 0..nal_count {
            let nal = read_config_nal(hvcc, &mut offset)?;
            if matches!(nal_type, 32..=34) {
                out.push(nal);
            }
        }
    }

    Ok(out)
}

fn read_config_nal(config: &[u8], offset: &mut usize) -> Result<Bytes> {
    if config.len().saturating_sub(*offset) < 2 {
        return Err(Error::Parse {
            format: "h265",
            message: "HEVC decoder config ended before NAL length".to_owned(),
        });
    }
    let len = u16::from_be_bytes([config[*offset], config[*offset + 1]]) as usize;
    *offset += 2;
    if config.len().saturating_sub(*offset) < len {
        return Err(Error::Parse {
            format: "h265",
            message: "HEVC decoder config NAL length exceeds config bytes".to_owned(),
        });
    }
    let nal = Bytes::copy_from_slice(&config[*offset..*offset + len]);
    *offset += len;
    Ok(nal)
}

fn annex_b_contains_nal_type(sample: &[u8], target: u8) -> bool {
    AnnexBNalIter::new(sample).any(|nal| {
        nal.len() >= 2 && {
            let nal_type = (nal[0] >> 1) & 0x3f;
            nal_type == target
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CodecId, TimeBase};

    #[test]
    fn converts_annex_b_sample_to_length_prefixed_without_parameter_sets() {
        let mut hvcc = vec![0u8; 23];
        hvcc[21] = 0xff;
        let hvcc = Bytes::from(hvcc);
        let sample = Bytes::from_static(
            b"\0\0\0\x01\x40\x01\0\0\0\x01\x42\x01\0\0\0\x01\x44\x01\0\0\0\x01\x26\x01",
        );
        let packet =
            EncodedPacket::new(1, CodecId::H265, 0, 1, TimeBase::new(1, 1).unwrap(), sample);

        let hevc = h265_packet_to_length_prefixed(&packet, &hvcc).unwrap();

        assert_eq!(&hevc[..], b"\0\0\0\x02\x26\x01");
    }
}
