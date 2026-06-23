use crate::{
    codec::{CodecDescriptor, CodecId, Decoder, Encoder},
    error::{Error, Result},
    frame::RgbaFrame,
};
use image::{DynamicImage, ImageFormat, RgbImage, RgbaImage};
use std::io::Cursor;

/// Still-image format supported by the RGBA adapter.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ImageStillFormat {
    /// PNG image.
    Png,
    /// JPEG image.
    Jpeg,
    /// GIF image.
    Gif,
    /// WebP image.
    WebP,
    /// AVIF image.
    Avif,
}

impl ImageStillFormat {
    fn image_format(self) -> ImageFormat {
        match self {
            Self::Png => ImageFormat::Png,
            Self::Jpeg => ImageFormat::Jpeg,
            Self::Gif => ImageFormat::Gif,
            Self::WebP => ImageFormat::WebP,
            Self::Avif => ImageFormat::Avif,
        }
    }

    fn codec_id(self) -> CodecId {
        match self {
            Self::Png => CodecId::Png,
            Self::Jpeg => CodecId::Jpeg,
            Self::Gif => CodecId::Gif,
            Self::WebP => CodecId::WebP,
            Self::Avif => CodecId::Avif,
        }
    }
}

/// Still-image decoder backed by the `image` crate.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ImageRgbaDecoder {
    format: Option<ImageStillFormat>,
}

impl ImageRgbaDecoder {
    /// Create an image decoder that auto-detects supported formats.
    #[must_use]
    pub const fn new() -> Self {
        Self { format: None }
    }

    /// Create an image decoder pinned to one format.
    #[must_use]
    pub const fn with_format(format: ImageStillFormat) -> Self {
        Self {
            format: Some(format),
        }
    }

    /// Configured format, or `None` when auto-detecting.
    #[must_use]
    pub const fn format(self) -> Option<ImageStillFormat> {
        self.format
    }
}

impl CodecDescriptor for ImageRgbaDecoder {
    fn name(&self) -> &'static str {
        "image-rgba/decoder"
    }

    fn codec_id(&self) -> CodecId {
        self.format
            .map(ImageStillFormat::codec_id)
            .unwrap_or_else(|| CodecId::Unknown("image".to_owned()))
    }
}

impl Decoder for ImageRgbaDecoder {
    type Input = [u8];
    type Output = RgbaFrame;

    fn decode(&mut self, input: &Self::Input) -> Result<Self::Output> {
        let image = match self.format {
            Some(format) => image::load_from_memory_with_format(input, format.image_format()),
            None => image::load_from_memory(input),
        }
        .map_err(image_error)?;

        let rgba = image.to_rgba8();
        let (width, height) = rgba.dimensions();
        RgbaFrame::new(width, height, width as usize * 4, rgba.into_raw())
    }
}

/// Still-image encoder backed by the `image` crate.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ImageRgbaEncoder {
    format: ImageStillFormat,
}

impl ImageRgbaEncoder {
    /// Create a PNG encoder.
    #[must_use]
    pub const fn png() -> Self {
        Self {
            format: ImageStillFormat::Png,
        }
    }

    /// Create a JPEG encoder.
    #[must_use]
    pub const fn jpeg() -> Self {
        Self {
            format: ImageStillFormat::Jpeg,
        }
    }

    /// Create a GIF encoder.
    #[must_use]
    pub const fn gif() -> Self {
        Self {
            format: ImageStillFormat::Gif,
        }
    }

    /// Create a WebP encoder.
    #[must_use]
    pub const fn webp() -> Self {
        Self {
            format: ImageStillFormat::WebP,
        }
    }

    /// Create an AVIF encoder.
    #[must_use]
    pub const fn avif() -> Self {
        Self {
            format: ImageStillFormat::Avif,
        }
    }

    /// Create an encoder for a specific still-image format.
    #[must_use]
    pub const fn with_format(format: ImageStillFormat) -> Self {
        Self { format }
    }

    /// Still-image format written by this encoder.
    #[must_use]
    pub const fn format(self) -> ImageStillFormat {
        self.format
    }
}

impl CodecDescriptor for ImageRgbaEncoder {
    fn name(&self) -> &'static str {
        "image-rgba/encoder"
    }

    fn codec_id(&self) -> CodecId {
        self.format.codec_id()
    }
}

impl Encoder for ImageRgbaEncoder {
    type Input = RgbaFrame;
    type Output = Vec<u8>;

    fn encode(&mut self, input: &Self::Input) -> Result<Self::Output> {
        let rgba = RgbaImage::from_raw(input.width, input.height, tightly_packed_rgba(input)?)
            .ok_or(Error::InvalidFrameBuffer {
                expected: input.width as usize * input.height as usize * 4,
                actual: input.data.len(),
            })?;

        let image = match self.format {
            ImageStillFormat::Png => DynamicImage::ImageRgba8(rgba),
            ImageStillFormat::Jpeg | ImageStillFormat::Gif => {
                DynamicImage::ImageRgb8(rgba_to_rgb(&rgba))
            }
            ImageStillFormat::WebP | ImageStillFormat::Avif => DynamicImage::ImageRgba8(rgba),
        };

        let mut output = Cursor::new(Vec::new());
        image
            .write_to(&mut output, self.format.image_format())
            .map_err(image_error)?;
        Ok(output.into_inner())
    }
}

fn tightly_packed_rgba(frame: &RgbaFrame) -> Result<Vec<u8>> {
    let row_len = frame.width as usize * 4;
    let expected = frame.stride * frame.height as usize;
    if frame.data.len() < expected {
        return Err(Error::InvalidFrameBuffer {
            expected,
            actual: frame.data.len(),
        });
    }

    if frame.stride == row_len && frame.data.len() == row_len * frame.height as usize {
        return Ok(frame.data.clone());
    }

    let mut output = vec![0; row_len * frame.height as usize];
    for y in 0..frame.height as usize {
        let src = y * frame.stride;
        let dst = y * row_len;
        output[dst..dst + row_len].copy_from_slice(&frame.data[src..src + row_len]);
    }
    Ok(output)
}

fn rgba_to_rgb(rgba: &RgbaImage) -> RgbImage {
    let mut rgb = RgbImage::new(rgba.width(), rgba.height());
    for (x, y, pixel) in rgba.enumerate_pixels() {
        rgb.put_pixel(x, y, image::Rgb([pixel[0], pixel[1], pixel[2]]));
    }
    rgb
}

fn image_error(err: image::ImageError) -> Error {
    Error::Parse {
        format: "image",
        message: err.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::{ImageRgbaDecoder, ImageRgbaEncoder, ImageStillFormat};
    use crate::{Decoder, Encoder, RgbaFrame};

    #[test]
    fn png_round_trips_rgba_frames() {
        let mut frame = RgbaFrame::solid(3, 2, [0, 0, 0, 0]);
        frame.set_pixel(1, 0, [10, 20, 30, 255]);
        frame.set_pixel(2, 1, [50, 60, 70, 128]);
        let mut encoder = ImageRgbaEncoder::png();
        let mut decoder = ImageRgbaDecoder::with_format(ImageStillFormat::Png);

        let bytes = encoder.encode(&frame).unwrap();
        let decoded = decoder.decode(&bytes).unwrap();

        assert_eq!(decoded.width, 3);
        assert_eq!(decoded.height, 2);
        assert_eq!(decoded.pixel(1, 0), Some([10, 20, 30, 255]));
        assert_eq!(decoded.pixel(2, 1), Some([50, 60, 70, 128]));
    }

    #[test]
    fn jpeg_writes_rgb_image_bytes() {
        let frame = RgbaFrame::solid(8, 8, [20, 40, 80, 128]);
        let mut encoder = ImageRgbaEncoder::jpeg();
        let mut decoder = ImageRgbaDecoder::with_format(ImageStillFormat::Jpeg);

        let bytes = encoder.encode(&frame).unwrap();
        let decoded = decoder.decode(&bytes).unwrap();

        assert_eq!(decoded.width, 8);
        assert_eq!(decoded.height, 8);
        assert_eq!(decoded.pixel(0, 0).unwrap()[3], 255);
    }

    #[test]
    fn gif_and_webp_write_image_bytes() {
        let frame = RgbaFrame::solid(6, 6, [20, 40, 80, 255]);

        for (mut encoder, format) in [
            (ImageRgbaEncoder::gif(), ImageStillFormat::Gif),
            (ImageRgbaEncoder::webp(), ImageStillFormat::WebP),
        ] {
            let bytes = encoder.encode(&frame).unwrap();
            let decoded = ImageRgbaDecoder::with_format(format)
                .decode(&bytes)
                .unwrap();

            assert_eq!(decoded.width, 6);
            assert_eq!(decoded.height, 6);
        }
    }
}
