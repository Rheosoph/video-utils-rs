use crate::{
    error::{Error, Result},
    frame::RgbaFrame,
};

/// Convert tightly-packed 8-bit YUV 4:2:0 planes into RGBA.
#[cfg(any(feature = "codec-h264-rust", feature = "codec-h265-rust", test))]
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

/// Convert tightly-packed 8-bit NV12 data into RGBA.
#[cfg(any(
    all(
        any(feature = "platform-codecs", feature = "codec-windows"),
        target_os = "windows"
    ),
    test
))]
pub(crate) fn nv12_8_to_rgba(width: u32, height: u32, nv12: &[u8]) -> Result<RgbaFrame> {
    let luma = width as usize * height as usize;
    let chroma_width = chroma_size(width);
    let chroma = chroma_width * chroma_size(height) * 2;
    let expected = luma + chroma;
    if nv12.len() < expected {
        return Err(Error::InvalidFrameBuffer {
            expected,
            actual: nv12.len(),
        });
    }

    let y = &nv12[..luma];
    let uv = &nv12[luma..expected];
    let stride = width as usize * 4;
    let mut data = vec![0; stride * height as usize];

    for row in 0..height as usize {
        let chroma_row = row / 2;
        for col in 0..width as usize {
            let y_sample = y[row * width as usize + col] as i32;
            let chroma_index = (chroma_row * chroma_width + col / 2) * 2;
            let u_sample = uv[chroma_index] as i32;
            let v_sample = uv[chroma_index + 1] as i32;
            let offset = row * stride + col * 4;
            let [r, g, b] = yuv_to_rgb_limited_8(y_sample, u_sample, v_sample);
            data[offset..offset + 4].copy_from_slice(&[r, g, b, 255]);
        }
    }

    RgbaFrame::new(width, height, stride, data)
}

/// Convert RGBA into tightly-packed 8-bit limited-range NV12 data.
#[cfg(any(
    all(
        any(feature = "platform-codecs", feature = "codec-windows"),
        target_os = "windows"
    ),
    test
))]
pub(crate) fn rgba_to_nv12(frame: &RgbaFrame) -> Vec<u8> {
    let width = frame.width as usize;
    let height = frame.height as usize;
    let luma = width * height;
    let chroma_width = chroma_size(frame.width);
    let chroma_height = chroma_size(frame.height);
    let mut out = vec![0; luma + chroma_width * chroma_height * 2];

    for row in 0..height {
        for col in 0..width {
            let offset = row * frame.stride + col * 4;
            out[row * width + col] = rgb_to_y_limited_8(
                frame.data[offset],
                frame.data[offset + 1],
                frame.data[offset + 2],
            );
        }
    }

    for chroma_row in 0..chroma_height {
        for chroma_col in 0..chroma_width {
            let row_start = chroma_row * 2;
            let col_start = chroma_col * 2;
            let row_end = (row_start + 2).min(height);
            let col_end = (col_start + 2).min(width);
            let mut r_sum = 0u32;
            let mut g_sum = 0u32;
            let mut b_sum = 0u32;
            let mut count = 0u32;

            for row in row_start..row_end {
                for col in col_start..col_end {
                    let offset = row * frame.stride + col * 4;
                    r_sum += u32::from(frame.data[offset]);
                    g_sum += u32::from(frame.data[offset + 1]);
                    b_sum += u32::from(frame.data[offset + 2]);
                    count += 1;
                }
            }

            let r = (r_sum / count) as u8;
            let g = (g_sum / count) as u8;
            let b = (b_sum / count) as u8;
            let [u, v] = rgb_to_uv_limited_8(r, g, b);
            let offset = luma + (chroma_row * chroma_width + chroma_col) * 2;
            out[offset] = u;
            out[offset + 1] = v;
        }
    }

    out
}

/// Convert tightly-packed 10/12-bit YUV 4:2:0 planes into 8-bit RGBA.
#[cfg(any(feature = "codec-h265-rust", test))]
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

#[cfg(any(feature = "codec-h264-rust", feature = "codec-h265-rust", test))]
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

#[cfg(any(
    all(
        any(feature = "platform-codecs", feature = "codec-windows"),
        target_os = "windows"
    ),
    test
))]
fn rgb_to_y_limited_8(r: u8, g: u8, b: u8) -> u8 {
    let r = i32::from(r);
    let g = i32::from(g);
    let b = i32::from(b);
    clamp_to_u8(((66 * r + 129 * g + 25 * b + 128) >> 8) + 16)
}

#[cfg(any(
    all(
        any(feature = "platform-codecs", feature = "codec-windows"),
        target_os = "windows"
    ),
    test
))]
fn rgb_to_uv_limited_8(r: u8, g: u8, b: u8) -> [u8; 2] {
    let r = i32::from(r);
    let g = i32::from(g);
    let b = i32::from(b);
    [
        clamp_to_u8(((-38 * r - 74 * g + 112 * b + 128) >> 8) + 128),
        clamp_to_u8(((112 * r - 94 * g - 18 * b + 128) >> 8) + 128),
    ]
}

fn clamp_to_u8(value: i32) -> u8 {
    value.clamp(0, 255) as u8
}

#[cfg(test)]
mod tests {
    use crate::frame::RgbaFrame;

    use super::{nv12_8_to_rgba, rgba_to_nv12, yuv420_8_to_rgba, yuv420_16_to_rgba};

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

    #[test]
    fn converts_nv12_to_rgba() {
        let frame = nv12_8_to_rgba(2, 2, &[16, 235, 81, 145, 128, 128]).unwrap();

        assert_eq!(frame.pixel(0, 0), Some([0, 0, 0, 255]));
        assert_eq!(frame.pixel(1, 0), Some([255, 255, 255, 255]));
    }

    #[test]
    fn converts_rgba_to_nv12_limited_range() {
        let frame = RgbaFrame::solid(2, 2, [0, 0, 0, 255]);

        assert_eq!(rgba_to_nv12(&frame), vec![16, 16, 16, 16, 128, 128]);
    }
}
