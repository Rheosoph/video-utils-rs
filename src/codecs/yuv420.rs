use crate::{
    error::{Error, Result},
    frame::RgbaFrame,
};

/// Convert tightly-packed 8-bit YUV 4:2:0 planes into RGBA.
pub(crate) fn yuv420_8_to_rgba(
    width: u32,
    height: u32,
    y: &[u8],
    u: &[u8],
    v: &[u8],
) -> Result<RgbaFrame> {
    validate_yuv420_planes(width, height, y.len(), u.len(), v.len())?;

    let stride = width as usize * 4;
    let mut data = vec![0; stride * height as usize];
    let chroma_width = chroma_size(width);

    for row in 0..height as usize {
        let chroma_row = row / 2;
        for col in 0..width as usize {
            let y_sample = y[row * width as usize + col] as i32;
            let chroma_index = chroma_row * chroma_width + col / 2;
            let u_sample = u[chroma_index] as i32;
            let v_sample = v[chroma_index] as i32;
            let offset = row * stride + col * 4;
            let [r, g, b] = yuv_to_rgb_limited_8(y_sample, u_sample, v_sample);
            data[offset..offset + 4].copy_from_slice(&[r, g, b, 255]);
        }
    }

    RgbaFrame::new(width, height, stride, data)
}

/// Convert tightly-packed 10/12-bit YUV 4:2:0 planes into 8-bit RGBA.
pub(crate) fn yuv420_16_to_rgba(
    width: u32,
    height: u32,
    bit_depth: u8,
    y: &[u16],
    u: &[u16],
    v: &[u16],
) -> Result<RgbaFrame> {
    validate_yuv420_planes(width, height, y.len(), u.len(), v.len())?;
    if !(9..=16).contains(&bit_depth) {
        return Err(Error::CodecBackend {
            codec: crate::codec::CodecId::H265,
            operation: "decode",
            message: format!("unsupported YUV bit depth {bit_depth}"),
        });
    }

    let shift = bit_depth.saturating_sub(8);
    let stride = width as usize * 4;
    let mut data = vec![0; stride * height as usize];
    let chroma_width = chroma_size(width);

    for row in 0..height as usize {
        let chroma_row = row / 2;
        for col in 0..width as usize {
            let y_sample = (y[row * width as usize + col] >> shift) as i32;
            let chroma_index = chroma_row * chroma_width + col / 2;
            let u_sample = (u[chroma_index] >> shift) as i32;
            let v_sample = (v[chroma_index] >> shift) as i32;
            let offset = row * stride + col * 4;
            let [r, g, b] = yuv_to_rgb_limited_8(y_sample, u_sample, v_sample);
            data[offset..offset + 4].copy_from_slice(&[r, g, b, 255]);
        }
    }

    RgbaFrame::new(width, height, stride, data)
}

fn validate_yuv420_planes(
    width: u32,
    height: u32,
    y_len: usize,
    u_len: usize,
    v_len: usize,
) -> Result<()> {
    let luma = width as usize * height as usize;
    let chroma = chroma_size(width) * chroma_size(height);

    if y_len < luma {
        return Err(Error::InvalidFrameBuffer {
            expected: luma,
            actual: y_len,
        });
    }
    if u_len < chroma {
        return Err(Error::InvalidFrameBuffer {
            expected: chroma,
            actual: u_len,
        });
    }
    if v_len < chroma {
        return Err(Error::InvalidFrameBuffer {
            expected: chroma,
            actual: v_len,
        });
    }

    Ok(())
}

fn chroma_size(value: u32) -> usize {
    value.div_ceil(2) as usize
}

fn yuv_to_rgb_limited_8(y: i32, u: i32, v: i32) -> [u8; 3] {
    let c = y - 16;
    let d = u - 128;
    let e = v - 128;

    [
        clamp_to_u8((298 * c + 409 * e + 128) >> 8),
        clamp_to_u8((298 * c - 100 * d - 208 * e + 128) >> 8),
        clamp_to_u8((298 * c + 516 * d + 128) >> 8),
    ]
}

fn clamp_to_u8(value: i32) -> u8 {
    value.clamp(0, 255) as u8
}

#[cfg(test)]
mod tests {
    use super::{yuv420_8_to_rgba, yuv420_16_to_rgba};

    #[test]
    fn converts_neutral_limited_range_yuv_to_rgba() {
        let frame = yuv420_8_to_rgba(2, 2, &[16, 235, 81, 145], &[128], &[128]).unwrap();

        assert_eq!(frame.pixel(0, 0), Some([0, 0, 0, 255]));
        assert_eq!(frame.pixel(1, 0), Some([255, 255, 255, 255]));
    }

    #[test]
    fn converts_high_bit_depth_yuv_to_rgba() {
        let frame = yuv420_16_to_rgba(2, 2, 10, &[64, 940, 64, 940], &[512], &[512]).unwrap();

        assert_eq!(frame.pixel(0, 0), Some([0, 0, 0, 255]));
        assert_eq!(frame.pixel(1, 0), Some([255, 255, 255, 255]));
    }
}
