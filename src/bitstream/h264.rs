use crate::{
    error::{Error, Result},
    packet::EncodedPacket,
};
use bytes::{Bytes, BytesMut};

const START_CODE: &[u8] = &[0, 0, 0, 1];

/// Convert an H.264 packet into Annex-B form suitable for elementary writers.
pub fn h264_packet_to_annex_b(packet: &EncodedPacket, avcc: Option<&Bytes>) -> Result<Bytes> {
    let length_size = avcc
        .and_then(|config| avcc_length_size(config).ok())
        .unwrap_or(4);
    let frame = if is_annex_b(&packet.data) {
        packet.data.clone()
    } else {
        length_prefixed_sample_to_annex_b(&packet.data, length_size, "h264")?
    };

    if packet.is_keyframe {
        prepend_missing_parameter_sets(frame, avcc)
    } else {
        Ok(frame)
    }
}

/// Convert an H.264 packet into AVC length-prefixed form suitable for Matroska.
pub fn h264_packet_to_length_prefixed(packet: &EncodedPacket, avcc: &Bytes) -> Result<Bytes> {
    let length_size = avcc_length_size(avcc)?;
    if is_annex_b(&packet.data) {
        return annex_b_sample_to_length_prefixed(&packet.data, length_size, "h264", |nal| {
            !nal.is_empty() && !matches!(nal[0] & 0x1f, 7 | 8)
        });
    }

    validate_length_prefixed_sample(&packet.data, length_size, "h264")?;
    Ok(packet.data.clone())
}

/// Return the NAL length prefix size declared by an AVCDecoderConfigurationRecord.
pub fn avcc_length_size(avcc: &[u8]) -> Result<usize> {
    if avcc.len() < 5 {
        return Err(Error::Parse {
            format: "h264",
            message: "AVC decoder config is too short".to_owned(),
        });
    }

    Ok(usize::from((avcc[4] & 0x03) + 1))
}

/// Build an AVCDecoderConfigurationRecord from Annex-B H.264 bytes.
///
/// The first SPS and PPS found in the sample are used. The resulting config
/// declares a 4-byte NAL length prefix, which is the crate's default muxing
/// convention for MP4/MOV-family output.
pub fn avcc_from_annex_b_sample(sample: &[u8]) -> Result<Bytes> {
    let mut sps = None::<Bytes>;
    let mut pps = None::<Bytes>;

    for nal in AnnexBNalIter::new(sample) {
        if nal.is_empty() {
            continue;
        }
        match nal[0] & 0x1f {
            7 if sps.is_none() => sps = Some(Bytes::copy_from_slice(nal)),
            8 if pps.is_none() => pps = Some(Bytes::copy_from_slice(nal)),
            _ => {}
        }
        if sps.is_some() && pps.is_some() {
            break;
        }
    }

    let sps = sps.ok_or(Error::Parse {
        format: "h264",
        message: "Annex-B sample has no SPS NAL unit".to_owned(),
    })?;
    let pps = pps.ok_or(Error::Parse {
        format: "h264",
        message: "Annex-B sample has no PPS NAL unit".to_owned(),
    })?;
    if sps.len() < 4 {
        return Err(Error::Parse {
            format: "h264",
            message: "SPS NAL unit is too short for AVC decoder config".to_owned(),
        });
    }
    if sps.len() > u16::MAX as usize || pps.len() > u16::MAX as usize {
        return Err(Error::Parse {
            format: "h264",
            message: "SPS/PPS NAL unit is too large for AVC decoder config".to_owned(),
        });
    }

    let mut out = BytesMut::new();
    out.extend_from_slice(&[1, sps[1], sps[2], sps[3], 0xff, 0xe1]);
    out.extend_from_slice(&(sps.len() as u16).to_be_bytes());
    out.extend_from_slice(&sps);
    out.extend_from_slice(&[1]);
    out.extend_from_slice(&(pps.len() as u16).to_be_bytes());
    out.extend_from_slice(&pps);
    Ok(out.freeze())
}

/// Parse coded dimensions from the first SPS in Annex-B H.264 bytes.
pub fn dimensions_from_annex_b_sample(sample: &[u8]) -> Result<(u32, u32)> {
    let sps = AnnexBNalIter::new(sample)
        .find(|nal| !nal.is_empty() && (nal[0] & 0x1f) == 7)
        .ok_or(Error::Parse {
            format: "h264",
            message: "Annex-B sample has no SPS NAL unit".to_owned(),
        })?;
    dimensions_from_sps(sps)
}

/// Parse coded dimensions from the SPS stored in AVC decoder config bytes.
pub fn dimensions_from_avcc(avcc: &[u8]) -> Result<(u32, u32)> {
    let sps = avcc_parameter_sets(avcc)?
        .into_iter()
        .find(|nal| !nal.is_empty() && (nal[0] & 0x1f) == 7)
        .ok_or(Error::Parse {
            format: "h264",
            message: "AVC decoder config has no SPS NAL unit".to_owned(),
        })?;
    dimensions_from_sps(&sps)
}

fn prepend_missing_parameter_sets(frame: Bytes, avcc: Option<&Bytes>) -> Result<Bytes> {
    if annex_b_contains_nal_type(&frame, 7) && annex_b_contains_nal_type(&frame, 8) {
        return Ok(frame);
    }

    let Some(avcc) = avcc else {
        return Ok(frame);
    };
    let parameter_sets = avcc_parameter_sets(avcc)?;
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

fn dimensions_from_sps(sps: &[u8]) -> Result<(u32, u32)> {
    if sps.len() < 4 || (sps[0] & 0x1f) != 7 {
        return Err(Error::Parse {
            format: "h264",
            message: "input is not an H.264 SPS NAL unit".to_owned(),
        });
    }

    let rbsp = rbsp_without_emulation_prevention(&sps[1..]);
    let mut bits = BitReader::new(&rbsp);
    let profile_idc = bits.read_bits(8)? as u8;
    bits.read_bits(8)?;
    bits.read_bits(8)?;
    bits.read_ue()?; // seq_parameter_set_id

    let mut chroma_format_idc = 1u32;
    let high_profile = matches!(
        profile_idc,
        100 | 110 | 122 | 244 | 44 | 83 | 86 | 118 | 128 | 138 | 139 | 134 | 135
    );
    if high_profile {
        chroma_format_idc = bits.read_ue()?;
        if chroma_format_idc == 3 {
            bits.read_bit()?;
        }
        bits.read_ue()?; // bit_depth_luma_minus8
        bits.read_ue()?; // bit_depth_chroma_minus8
        bits.read_bit()?; // qpprime_y_zero_transform_bypass_flag
        if bits.read_bit()? {
            let scaling_lists = if chroma_format_idc == 3 { 12 } else { 8 };
            for index in 0..scaling_lists {
                if bits.read_bit()? {
                    skip_scaling_list(&mut bits, if index < 6 { 16 } else { 64 })?;
                }
            }
        }
    }

    bits.read_ue()?; // log2_max_frame_num_minus4
    let pic_order_cnt_type = bits.read_ue()?;
    if pic_order_cnt_type == 0 {
        bits.read_ue()?; // log2_max_pic_order_cnt_lsb_minus4
    } else if pic_order_cnt_type == 1 {
        bits.read_bit()?; // delta_pic_order_always_zero_flag
        bits.read_se()?; // offset_for_non_ref_pic
        bits.read_se()?; // offset_for_top_to_bottom_field
        let cycle = bits.read_ue()?;
        for _ in 0..cycle {
            bits.read_se()?;
        }
    }

    bits.read_ue()?; // max_num_ref_frames
    bits.read_bit()?; // gaps_in_frame_num_value_allowed_flag
    let pic_width_in_mbs_minus1 = bits.read_ue()?;
    let pic_height_in_map_units_minus1 = bits.read_ue()?;
    let frame_mbs_only_flag = bits.read_bit()?;
    if !frame_mbs_only_flag {
        bits.read_bit()?; // mb_adaptive_frame_field_flag
    }
    bits.read_bit()?; // direct_8x8_inference_flag

    let mut crop_left = 0u32;
    let mut crop_right = 0u32;
    let mut crop_top = 0u32;
    let mut crop_bottom = 0u32;
    if bits.read_bit()? {
        crop_left = bits.read_ue()?;
        crop_right = bits.read_ue()?;
        crop_top = bits.read_ue()?;
        crop_bottom = bits.read_ue()?;
    }

    let mut width = (pic_width_in_mbs_minus1 + 1) * 16;
    let mut height =
        (2 - u32::from(frame_mbs_only_flag)) * (pic_height_in_map_units_minus1 + 1) * 16;
    let (crop_unit_x, crop_unit_y) = crop_units(chroma_format_idc, frame_mbs_only_flag);
    width = width.saturating_sub((crop_left + crop_right) * crop_unit_x);
    height = height.saturating_sub((crop_top + crop_bottom) * crop_unit_y);

    if width == 0 || height == 0 {
        return Err(Error::Parse {
            format: "h264",
            message: "SPS dimensions resolve to zero".to_owned(),
        });
    }
    Ok((width, height))
}

fn crop_units(chroma_format_idc: u32, frame_mbs_only_flag: bool) -> (u32, u32) {
    let frame_factor = 2 - u32::from(frame_mbs_only_flag);
    match chroma_format_idc {
        0 => (1, frame_factor),
        1 => (2, 2 * frame_factor),
        2 => (2, frame_factor),
        3 => (1, frame_factor),
        _ => (1, frame_factor),
    }
}

fn rbsp_without_emulation_prevention(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len());
    let mut zeros = 0usize;
    for &byte in data {
        if zeros >= 2 && byte == 0x03 {
            zeros = 0;
            continue;
        }
        out.push(byte);
        zeros = if byte == 0 { zeros + 1 } else { 0 };
    }
    out
}

fn skip_scaling_list(bits: &mut BitReader<'_>, size: usize) -> Result<()> {
    let mut last_scale = 8i32;
    let mut next_scale = 8i32;
    for _ in 0..size {
        if next_scale != 0 {
            let delta_scale = bits.read_se()?;
            next_scale = (last_scale + delta_scale + 256) % 256;
        }
        last_scale = if next_scale == 0 {
            last_scale
        } else {
            next_scale
        };
    }
    Ok(())
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

    fn read_bit(&mut self) -> Result<bool> {
        Ok(self.read_bits(1)? != 0)
    }

    fn read_bits(&mut self, bits: usize) -> Result<u32> {
        if bits > 32 {
            return Err(Error::Parse {
                format: "h264",
                message: "internal H.264 bit-reader request exceeds u32".to_owned(),
            });
        }
        if self.bytes.len() * 8 - self.bit_offset < bits {
            return Err(Error::Parse {
                format: "h264",
                message: "SPS ended before expected field".to_owned(),
            });
        }

        let mut value = 0u32;
        for _ in 0..bits {
            let byte = self.bytes[self.bit_offset / 8];
            let shift = 7 - (self.bit_offset % 8);
            value = (value << 1) | u32::from((byte >> shift) & 1);
            self.bit_offset += 1;
        }
        Ok(value)
    }

    fn read_ue(&mut self) -> Result<u32> {
        let mut leading_zero_bits = 0usize;
        while !self.read_bit()? {
            leading_zero_bits += 1;
            if leading_zero_bits > 31 {
                return Err(Error::Parse {
                    format: "h264",
                    message: "Exp-Golomb value is too large".to_owned(),
                });
            }
        }
        let suffix = if leading_zero_bits == 0 {
            0
        } else {
            self.read_bits(leading_zero_bits)?
        };
        Ok((1u32 << leading_zero_bits) - 1 + suffix)
    }

    fn read_se(&mut self) -> Result<i32> {
        let value = self.read_ue()? as i32;
        if value % 2 == 0 {
            Ok(-(value / 2))
        } else {
            Ok((value + 1) / 2)
        }
    }
}

pub(crate) fn avcc_parameter_sets(avcc: &[u8]) -> Result<Vec<Bytes>> {
    if avcc.len() < 7 {
        return Err(Error::Parse {
            format: "h264",
            message: "AVC decoder config is too short for SPS/PPS data".to_owned(),
        });
    }

    let mut out = Vec::new();
    let mut offset = 5;
    let sps_count = avcc[offset] & 0x1f;
    offset += 1;
    for _ in 0..sps_count {
        out.push(read_config_nal(avcc, &mut offset, "h264")?);
    }

    if offset >= avcc.len() {
        return Err(Error::Parse {
            format: "h264",
            message: "AVC decoder config is missing PPS count".to_owned(),
        });
    }
    let pps_count = avcc[offset];
    offset += 1;
    for _ in 0..pps_count {
        out.push(read_config_nal(avcc, &mut offset, "h264")?);
    }

    Ok(out)
}

fn read_config_nal(config: &[u8], offset: &mut usize, format: &'static str) -> Result<Bytes> {
    if config.len().saturating_sub(*offset) < 2 {
        return Err(Error::Parse {
            format,
            message: "codec config ended before NAL length".to_owned(),
        });
    }
    let len = u16::from_be_bytes([config[*offset], config[*offset + 1]]) as usize;
    *offset += 2;
    if config.len().saturating_sub(*offset) < len {
        return Err(Error::Parse {
            format,
            message: "codec config NAL length exceeds config bytes".to_owned(),
        });
    }
    let nal = Bytes::copy_from_slice(&config[*offset..*offset + len]);
    *offset += len;
    Ok(nal)
}

pub(crate) fn annex_b_sample_to_length_prefixed(
    sample: &[u8],
    length_size: usize,
    format: &'static str,
    keep_nal: impl Fn(&[u8]) -> bool,
) -> Result<Bytes> {
    if !(1..=4).contains(&length_size) {
        return Err(Error::Parse {
            format,
            message: "NAL length size must be between 1 and 4 bytes".to_owned(),
        });
    }

    let mut out = BytesMut::with_capacity(sample.len());
    let mut saw_nal = false;
    for nal in AnnexBNalIter::new(sample).filter(|nal| keep_nal(nal)) {
        saw_nal = true;
        write_nal_length(&mut out, nal.len(), length_size, format)?;
        out.extend_from_slice(nal);
    }

    if out.is_empty() {
        for nal in AnnexBNalIter::new(sample) {
            saw_nal = true;
            write_nal_length(&mut out, nal.len(), length_size, format)?;
            out.extend_from_slice(nal);
        }
    }

    if out.is_empty() {
        return Err(Error::Parse {
            format,
            message: if saw_nal {
                "sample contained no non-empty NAL units".to_owned()
            } else {
                "sample contained no muxable NAL units".to_owned()
            },
        });
    }

    Ok(out.freeze())
}

pub(crate) fn length_prefixed_sample_to_annex_b(
    sample: &[u8],
    length_size: usize,
    format: &'static str,
) -> Result<Bytes> {
    if !(1..=4).contains(&length_size) {
        return Err(Error::Parse {
            format,
            message: "NAL length size must be between 1 and 4 bytes".to_owned(),
        });
    }

    let mut offset = 0;
    let mut out = BytesMut::with_capacity(sample.len() + START_CODE.len() * 4);
    while offset < sample.len() {
        if sample.len().saturating_sub(offset) < length_size {
            return Err(Error::Parse {
                format,
                message: "length-prefixed sample ended inside a NAL length".to_owned(),
            });
        }

        let mut len = 0usize;
        for byte in &sample[offset..offset + length_size] {
            len = (len << 8) | usize::from(*byte);
        }
        offset += length_size;

        if len == 0 {
            continue;
        }
        if sample.len().saturating_sub(offset) < len {
            return Err(Error::Parse {
                format,
                message: "length-prefixed NAL length exceeds sample bytes".to_owned(),
            });
        }

        out.extend_from_slice(START_CODE);
        out.extend_from_slice(&sample[offset..offset + len]);
        offset += len;
    }

    if out.is_empty() {
        return Err(Error::Parse {
            format,
            message: "sample contained no NAL units".to_owned(),
        });
    }

    Ok(out.freeze())
}

pub(crate) fn validate_length_prefixed_sample(
    sample: &[u8],
    length_size: usize,
    format: &'static str,
) -> Result<()> {
    if !(1..=4).contains(&length_size) {
        return Err(Error::Parse {
            format,
            message: "NAL length size must be between 1 and 4 bytes".to_owned(),
        });
    }

    let mut offset = 0usize;
    let mut nal_count = 0usize;
    while offset < sample.len() {
        if sample.len().saturating_sub(offset) < length_size {
            return Err(Error::Parse {
                format,
                message: "length-prefixed sample ended inside a NAL length".to_owned(),
            });
        }

        let mut len = 0usize;
        for byte in &sample[offset..offset + length_size] {
            len = (len << 8) | usize::from(*byte);
        }
        offset += length_size;

        if len == 0 {
            continue;
        }
        if sample.len().saturating_sub(offset) < len {
            return Err(Error::Parse {
                format,
                message: "length-prefixed NAL length exceeds sample bytes".to_owned(),
            });
        }

        offset += len;
        nal_count += 1;
    }

    if nal_count == 0 {
        return Err(Error::Parse {
            format,
            message: "sample contained no NAL units".to_owned(),
        });
    }

    Ok(())
}

pub(crate) fn is_annex_b(sample: &[u8]) -> bool {
    sample.starts_with(&[0, 0, 1]) || sample.starts_with(START_CODE)
}

fn write_nal_length(
    out: &mut BytesMut,
    len: usize,
    length_size: usize,
    format: &'static str,
) -> Result<()> {
    let max = (1usize << (length_size * 8)) - 1;
    if len > max {
        return Err(Error::Parse {
            format,
            message: "NAL unit is too large for configured length size".to_owned(),
        });
    }

    for shift in (0..length_size).rev() {
        out.extend_from_slice(&[((len >> (shift * 8)) & 0xff) as u8]);
    }
    Ok(())
}

fn annex_b_contains_nal_type(sample: &[u8], target: u8) -> bool {
    AnnexBNalIter::new(sample).any(|nal| !nal.is_empty() && (nal[0] & 0x1f) == target)
}

pub(crate) struct AnnexBNalIter<'a> {
    data: &'a [u8],
    offset: usize,
}

impl<'a> AnnexBNalIter<'a> {
    pub(crate) fn new(data: &'a [u8]) -> Self {
        Self { data, offset: 0 }
    }
}

impl<'a> Iterator for AnnexBNalIter<'a> {
    type Item = &'a [u8];

    fn next(&mut self) -> Option<Self::Item> {
        let start_code = find_start_code(self.data, self.offset)?;
        let nal_start = start_code.0 + start_code.1;
        let next_start = find_start_code(self.data, nal_start)
            .map(|(offset, _)| offset)
            .unwrap_or(self.data.len());
        self.offset = next_start;
        Some(&self.data[nal_start..next_start])
    }
}

fn find_start_code(data: &[u8], from: usize) -> Option<(usize, usize)> {
    let mut index = from;
    while index + 3 <= data.len() {
        if data[index..].starts_with(START_CODE) {
            return Some((index, 4));
        }
        if data[index..].starts_with(&[0, 0, 1]) {
            return Some((index, 3));
        }
        index += 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CodecId, TimeBase};

    #[test]
    fn converts_avcc_length_prefixed_sample_to_annex_b() {
        let sample = Bytes::from_static(b"\0\0\0\x02\x65\x88\0\0\0\x01\x41");
        let packet =
            EncodedPacket::new(1, CodecId::H264, 0, 1, TimeBase::new(1, 1).unwrap(), sample);

        let annex_b = h264_packet_to_annex_b(&packet, None).unwrap();

        assert_eq!(&annex_b[..], b"\0\0\0\x01\x65\x88\0\0\0\x01\x41");
    }

    #[test]
    fn converts_annex_b_sample_to_length_prefixed_without_parameter_sets() {
        let avcc = Bytes::from_static(b"\x01\x42\x00\x1e\xff\xe1\0\x02\x67\x42\x01\0\x02\x68\xce");
        let sample = Bytes::from_static(b"\0\0\0\x01\x67\x42\0\0\0\x01\x68\xce\0\0\0\x01\x65\x88");
        let packet =
            EncodedPacket::new(1, CodecId::H264, 0, 1, TimeBase::new(1, 1).unwrap(), sample);

        let avc = h264_packet_to_length_prefixed(&packet, &avcc).unwrap();

        assert_eq!(&avc[..], b"\0\0\0\x02\x65\x88");
    }

    #[test]
    fn builds_avcc_from_annex_b_parameter_sets() {
        let sample =
            b"\0\0\0\x01\x67\x42\x00\x1e\x95\xa8\x14\x01\x6e\0\0\0\x01\x68\xce\x3c\x80\0\0\0\x01\x65\x88";

        let avcc = avcc_from_annex_b_sample(sample).unwrap();

        assert_eq!(avcc[0], 1);
        assert_eq!(avcc[1], 0x42);
        assert_eq!(avcc[4] & 0x03, 3);
        assert_eq!(avcc[5] & 0x1f, 1);
    }

    #[test]
    fn parses_dimensions_from_annex_b_sps() {
        let sample = b"\0\0\0\x01\x67\x42\xc0\x0a\xda\x0a\x11\xf9\x70\x11\0\0\0\x01\x68\xce\x0f\x2c\x80\0\0\0\x01\x65\x88";

        let (width, height) = dimensions_from_annex_b_sample(sample).unwrap();

        assert_eq!((width, height), (160, 120));
    }

    #[test]
    fn parses_dimensions_from_avcc_sps() {
        let avcc = avcc_from_annex_b_sample(
            b"\0\0\0\x01\x67\x42\xc0\x0a\xda\x0a\x11\xf9\x70\x11\0\0\0\x01\x68\xce\x0f\x2c\x80",
        )
        .unwrap();

        let (width, height) = dimensions_from_avcc(&avcc).unwrap();

        assert_eq!((width, height), (160, 120));
    }
}
