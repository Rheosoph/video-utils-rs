use crate::error::{Error, Result};

/// Rectangle used for crop-like frame operations.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CropRect {
    /// Left coordinate in pixels.
    pub x: u32,
    /// Top coordinate in pixels.
    pub y: u32,
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
}

impl CropRect {
    /// Create a crop rectangle.
    #[must_use]
    pub const fn new(x: u32, y: u32, width: u32, height: u32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }
}

/// Detected black border sizes in pixels.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BlackBars {
    /// Top black rows.
    pub top: u32,
    /// Bottom black rows.
    pub bottom: u32,
    /// Left black columns.
    pub left: u32,
    /// Right black columns.
    pub right: u32,
}

/// Color adjustment applied to every pixel in a decoded RGBA frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ColorFilter {
    /// Additive brightness in the range `-1.0..=1.0`, where `1.0` adds 255.
    pub brightness: f32,
    /// Contrast multiplier. `1.0` is unchanged.
    pub contrast: f32,
    /// Saturation multiplier. `1.0` is unchanged and `0.0` is grayscale.
    pub saturation: f32,
    /// Alpha multiplier. `1.0` is unchanged.
    pub alpha: f32,
}

impl ColorFilter {
    /// Neutral color filter.
    #[must_use]
    pub const fn identity() -> Self {
        Self {
            brightness: 0.0,
            contrast: 1.0,
            saturation: 1.0,
            alpha: 1.0,
        }
    }

    /// Grayscale color filter.
    #[must_use]
    pub const fn grayscale() -> Self {
        Self {
            brightness: 0.0,
            contrast: 1.0,
            saturation: 0.0,
            alpha: 1.0,
        }
    }

    /// Sepia-like warm color filter.
    #[must_use]
    pub const fn sepia() -> Self {
        Self {
            brightness: 0.03,
            contrast: 1.05,
            saturation: 0.35,
            alpha: 1.0,
        }
    }
}

impl Default for ColorFilter {
    fn default() -> Self {
        Self::identity()
    }
}

/// Anchor point for placing a watermark frame over another frame.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum WatermarkAnchor {
    /// Top-left corner.
    TopLeft,
    /// Top-right corner.
    TopRight,
    /// Bottom-left corner.
    BottomLeft,
    /// Bottom-right corner.
    BottomRight,
    /// Center of the base frame.
    Center,
}

/// Watermark image and placement options.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Watermark {
    /// RGBA watermark frame.
    pub frame: RgbaFrame,
    /// Placement anchor.
    pub anchor: WatermarkAnchor,
    /// Horizontal margin or center offset in pixels.
    pub margin_x: i32,
    /// Vertical margin or center offset in pixels.
    pub margin_y: i32,
    /// Extra opacity multiplier applied to the watermark alpha channel.
    pub opacity: u8,
}

impl Watermark {
    /// Create a watermark with bottom-right placement and full opacity.
    #[must_use]
    pub fn new(frame: RgbaFrame) -> Self {
        Self {
            frame,
            anchor: WatermarkAnchor::BottomRight,
            margin_x: 0,
            margin_y: 0,
            opacity: 255,
        }
    }

    /// Set placement anchor.
    #[must_use]
    pub const fn with_anchor(mut self, anchor: WatermarkAnchor) -> Self {
        self.anchor = anchor;
        self
    }

    /// Set placement margins.
    #[must_use]
    pub const fn with_margin(mut self, margin_x: i32, margin_y: i32) -> Self {
        self.margin_x = margin_x;
        self.margin_y = margin_y;
        self
    }

    /// Set opacity multiplier from `0.0..=1.0`.
    #[must_use]
    pub fn with_opacity(mut self, opacity: f32) -> Self {
        self.opacity = (opacity.clamp(0.0, 1.0) * 255.0).round() as u8;
        self
    }
}

/// One decoded-frame transformation step.
#[derive(Clone, Debug, PartialEq)]
pub enum FrameTransform {
    /// Crop to a rectangle.
    Crop(CropRect),
    /// Resize with nearest-neighbor sampling.
    Resize { width: u32, height: u32 },
    /// Pad onto a larger canvas.
    Pad {
        width: u32,
        height: u32,
        offset_x: u32,
        offset_y: u32,
        color: [u8; 4],
    },
    /// Flip horizontally.
    FlipHorizontal,
    /// Flip vertically.
    FlipVertical,
    /// Rotate 90 degrees.
    Rotate90 { clockwise: bool },
    /// Apply a simple box blur.
    BoxBlur { radius: u32 },
    /// Apply color adjustment.
    ColorFilter(ColorFilter),
    /// Composite a watermark.
    Watermark(Watermark),
}

impl FrameTransform {
    /// Apply this transform to a frame.
    pub fn apply(&self, frame: &RgbaFrame) -> Result<RgbaFrame> {
        match self {
            Self::Crop(rect) => frame.crop(*rect),
            Self::Resize { width, height } => frame.resize_nearest(*width, *height),
            Self::Pad {
                width,
                height,
                offset_x,
                offset_y,
                color,
            } => frame.pad(*width, *height, *offset_x, *offset_y, *color),
            Self::FlipHorizontal => Ok(frame.flip_horizontal()),
            Self::FlipVertical => Ok(frame.flip_vertical()),
            Self::Rotate90 { clockwise } => Ok(frame.rotate_90(*clockwise)),
            Self::BoxBlur { radius } => frame.box_blur(*radius),
            Self::ColorFilter(filter) => Ok(frame.apply_color_filter(*filter)),
            Self::Watermark(watermark) => Ok(frame.apply_watermark(watermark)),
        }
    }
}

/// Ordered decoded-frame transformation pipeline.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FrameTransformPipeline {
    /// Transform steps applied in order.
    pub steps: Vec<FrameTransform>,
}

impl FrameTransformPipeline {
    /// Create an empty pipeline.
    #[must_use]
    pub const fn new() -> Self {
        Self { steps: Vec::new() }
    }

    /// Append a transform step.
    pub fn push(&mut self, transform: FrameTransform) {
        self.steps.push(transform);
    }

    /// Return a pipeline with an additional transform step.
    #[must_use]
    pub fn with(mut self, transform: FrameTransform) -> Self {
        self.push(transform);
        self
    }

    /// Apply all steps to one frame.
    pub fn apply(&self, frame: &RgbaFrame) -> Result<RgbaFrame> {
        let mut current = frame.clone();
        for step in &self.steps {
            current = step.apply(&current)?;
        }
        Ok(current)
    }

    /// Apply all steps to a sequence of frames.
    pub fn apply_all<'a>(
        &self,
        frames: impl IntoIterator<Item = &'a RgbaFrame>,
    ) -> Result<Vec<RgbaFrame>> {
        frames.into_iter().map(|frame| self.apply(frame)).collect()
    }
}

/// Owned 8-bit RGBA frame.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RgbaFrame {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// Bytes per row.
    pub stride: usize,
    /// RGBA byte data.
    pub data: Vec<u8>,
}

impl RgbaFrame {
    /// Create a validated frame from RGBA bytes.
    pub fn new(width: u32, height: u32, stride: usize, data: Vec<u8>) -> Result<Self> {
        let min_stride = width as usize * 4;
        if stride < min_stride {
            return Err(Error::InvalidFrameBuffer {
                expected: min_stride,
                actual: stride,
            });
        }

        let expected = stride * height as usize;
        if data.len() < expected {
            return Err(Error::InvalidFrameBuffer {
                expected,
                actual: data.len(),
            });
        }

        Ok(Self {
            width,
            height,
            stride,
            data,
        })
    }

    /// Create a solid-color RGBA frame.
    #[must_use]
    pub fn solid(width: u32, height: u32, rgba: [u8; 4]) -> Self {
        let stride = width as usize * 4;
        let mut data = vec![0; stride * height as usize];
        for pixel in data.chunks_exact_mut(4) {
            pixel.copy_from_slice(&rgba);
        }
        Self {
            width,
            height,
            stride,
            data,
        }
    }

    /// Read one pixel.
    #[must_use]
    pub fn pixel(&self, x: u32, y: u32) -> Option<[u8; 4]> {
        if x >= self.width || y >= self.height {
            return None;
        }

        let offset = y as usize * self.stride + x as usize * 4;
        Some([
            self.data[offset],
            self.data[offset + 1],
            self.data[offset + 2],
            self.data[offset + 3],
        ])
    }

    /// Write one pixel when coordinates are inside the frame.
    pub fn set_pixel(&mut self, x: u32, y: u32, rgba: [u8; 4]) {
        if x >= self.width || y >= self.height {
            return;
        }

        let offset = y as usize * self.stride + x as usize * 4;
        self.data[offset..offset + 4].copy_from_slice(&rgba);
    }

    /// Crop a frame.
    pub fn crop(&self, rect: CropRect) -> Result<Self> {
        if rect.width == 0
            || rect.height == 0
            || rect.x + rect.width > self.width
            || rect.y + rect.height > self.height
        {
            return Err(Error::InvalidRange {
                start: rect.x as i64,
                end: (rect.x + rect.width) as i64,
            });
        }

        let stride = rect.width as usize * 4;
        let mut data = vec![0; stride * rect.height as usize];
        for row in 0..rect.height as usize {
            let src_offset = (rect.y as usize + row) * self.stride + rect.x as usize * 4;
            let dst_offset = row * stride;
            data[dst_offset..dst_offset + stride]
                .copy_from_slice(&self.data[src_offset..src_offset + stride]);
        }

        Self::new(rect.width, rect.height, stride, data)
    }

    /// Pad into a larger canvas at the given offset.
    pub fn pad(
        &self,
        width: u32,
        height: u32,
        offset_x: u32,
        offset_y: u32,
        rgba: [u8; 4],
    ) -> Result<Self> {
        if width < self.width + offset_x || height < self.height + offset_y {
            return Err(Error::InvalidRange {
                start: 0,
                end: width as i64,
            });
        }

        let mut output = Self::solid(width, height, rgba);
        output.overlay(self, offset_x as i32, offset_y as i32);
        Ok(output)
    }

    /// Flip horizontally.
    #[must_use]
    pub fn flip_horizontal(&self) -> Self {
        let mut output = Self::solid(self.width, self.height, [0, 0, 0, 0]);
        for y in 0..self.height {
            for x in 0..self.width {
                if let Some(pixel) = self.pixel(x, y) {
                    output.set_pixel(self.width - 1 - x, y, pixel);
                }
            }
        }
        output
    }

    /// Flip vertically.
    #[must_use]
    pub fn flip_vertical(&self) -> Self {
        let mut output = Self::solid(self.width, self.height, [0, 0, 0, 0]);
        for y in 0..self.height {
            let src_offset = y as usize * self.stride;
            let dst_y = self.height - 1 - y;
            let dst_offset = dst_y as usize * output.stride;
            let len = self.width as usize * 4;
            output.data[dst_offset..dst_offset + len]
                .copy_from_slice(&self.data[src_offset..src_offset + len]);
        }
        output
    }

    /// Rotate by 90 degrees.
    #[must_use]
    pub fn rotate_90(&self, clockwise: bool) -> Self {
        let mut output = Self::solid(self.height, self.width, [0, 0, 0, 0]);
        for y in 0..self.height {
            for x in 0..self.width {
                if let Some(pixel) = self.pixel(x, y) {
                    let (dst_x, dst_y) = if clockwise {
                        (self.height - 1 - y, x)
                    } else {
                        (y, self.width - 1 - x)
                    };
                    output.set_pixel(dst_x, dst_y, pixel);
                }
            }
        }
        output
    }

    /// Resize with nearest-neighbor sampling.
    pub fn resize_nearest(&self, width: u32, height: u32) -> Result<Self> {
        if width == 0 || height == 0 {
            return Err(Error::InvalidRange {
                start: width as i64,
                end: height as i64,
            });
        }

        let mut output = Self::solid(width, height, [0, 0, 0, 0]);
        for y in 0..height {
            let src_y = y as u64 * self.height as u64 / height as u64;
            for x in 0..width {
                let src_x = x as u64 * self.width as u64 / width as u64;
                if let Some(pixel) = self.pixel(src_x as u32, src_y as u32) {
                    output.set_pixel(x, y, pixel);
                }
            }
        }
        Ok(output)
    }

    /// Alpha-composite an overlay into this frame.
    pub fn overlay(&mut self, overlay: &RgbaFrame, x: i32, y: i32) {
        for oy in 0..overlay.height as i32 {
            let dy = y + oy;
            if dy < 0 || dy >= self.height as i32 {
                continue;
            }
            for ox in 0..overlay.width as i32 {
                let dx = x + ox;
                if dx < 0 || dx >= self.width as i32 {
                    continue;
                }

                let src = overlay.pixel(ox as u32, oy as u32).unwrap();
                let dst = self.pixel(dx as u32, dy as u32).unwrap();
                self.set_pixel(dx as u32, dy as u32, alpha_blend(dst, src));
            }
        }
    }

    /// Apply a box blur with the given radius.
    pub fn box_blur(&self, radius: u32) -> Result<Self> {
        if radius == 0 {
            return Ok(self.clone());
        }
        if self.width == 0 || self.height == 0 {
            return Err(Error::EmptyInput);
        }

        let radius = radius.min(self.width.max(self.height));
        let mut output = Self::solid(self.width, self.height, [0, 0, 0, 0]);
        for y in 0..self.height {
            let start_y = y.saturating_sub(radius);
            let end_y = (y + radius).min(self.height - 1);
            for x in 0..self.width {
                let start_x = x.saturating_sub(radius);
                let end_x = (x + radius).min(self.width - 1);
                let mut sum = [0u64; 4];
                let mut count = 0u64;
                for sy in start_y..=end_y {
                    for sx in start_x..=end_x {
                        let pixel = self.pixel(sx, sy).unwrap_or([0, 0, 0, 0]);
                        sum[0] += u64::from(pixel[0]);
                        sum[1] += u64::from(pixel[1]);
                        sum[2] += u64::from(pixel[2]);
                        sum[3] += u64::from(pixel[3]);
                        count += 1;
                    }
                }
                output.set_pixel(
                    x,
                    y,
                    [
                        (sum[0] / count) as u8,
                        (sum[1] / count) as u8,
                        (sum[2] / count) as u8,
                        (sum[3] / count) as u8,
                    ],
                );
            }
        }

        Ok(output)
    }

    /// Apply a color filter to every pixel.
    #[must_use]
    pub fn apply_color_filter(&self, filter: ColorFilter) -> Self {
        let mut output = self.clone();
        let contrast = finite_or(filter.contrast, 1.0).max(0.0);
        let saturation = finite_or(filter.saturation, 1.0).max(0.0);
        let brightness = finite_or(filter.brightness, 0.0).clamp(-1.0, 1.0) * 255.0;
        let alpha = finite_or(filter.alpha, 1.0).clamp(0.0, 1.0);

        for y in 0..self.height {
            for x in 0..self.width {
                let [r, g, b, a] = self.pixel(x, y).unwrap_or([0, 0, 0, 0]);
                let luma = 0.2126 * r as f32 + 0.7152 * g as f32 + 0.0722 * b as f32;
                let adjust = |channel: u8| -> u8 {
                    let saturated = luma + (channel as f32 - luma) * saturation;
                    let contrasted = (saturated - 128.0) * contrast + 128.0;
                    clamp_u8(contrasted + brightness)
                };
                output.set_pixel(
                    x,
                    y,
                    [adjust(r), adjust(g), adjust(b), clamp_u8(a as f32 * alpha)],
                );
            }
        }

        output
    }

    /// Composite a watermark over this frame.
    #[must_use]
    pub fn apply_watermark(&self, watermark: &Watermark) -> Self {
        let mut output = self.clone();
        let mut mark = watermark.frame.clone();
        if watermark.opacity < 255 {
            for pixel in mark.data.chunks_exact_mut(4) {
                pixel[3] = ((u16::from(pixel[3]) * u16::from(watermark.opacity)) / 255) as u8;
            }
        }
        let (x, y) = watermark_position(self, &mark, watermark);
        output.overlay(&mark, x, y);
        output
    }

    /// Detect fully or mostly black borders.
    #[must_use]
    pub fn detect_black_bars(&self, luma_threshold: u8, required_fraction: f32) -> BlackBars {
        let required_fraction = required_fraction.clamp(0.0, 1.0);
        let mut top = 0;
        while top < self.height && self.row_black_fraction(top, luma_threshold) >= required_fraction
        {
            top += 1;
        }

        let mut bottom = 0;
        while bottom < self.height.saturating_sub(top)
            && self.row_black_fraction(self.height - 1 - bottom, luma_threshold)
                >= required_fraction
        {
            bottom += 1;
        }

        let mut left = 0;
        while left < self.width
            && self.column_black_fraction(left, top, bottom, luma_threshold) >= required_fraction
        {
            left += 1;
        }

        let mut right = 0;
        while right < self.width.saturating_sub(left)
            && self.column_black_fraction(self.width - 1 - right, top, bottom, luma_threshold)
                >= required_fraction
        {
            right += 1;
        }

        BlackBars {
            top,
            bottom,
            left,
            right,
        }
    }

    fn row_black_fraction(&self, y: u32, threshold: u8) -> f32 {
        let mut black = 0usize;
        for x in 0..self.width {
            if self.pixel_luma(x, y) <= threshold {
                black += 1;
            }
        }
        black as f32 / self.width.max(1) as f32
    }

    fn column_black_fraction(&self, x: u32, top: u32, bottom: u32, threshold: u8) -> f32 {
        let end = self.height.saturating_sub(bottom);
        if top >= end {
            return 0.0;
        }

        let mut black = 0usize;
        for y in top..end {
            if self.pixel_luma(x, y) <= threshold {
                black += 1;
            }
        }
        black as f32 / (end - top) as f32
    }

    fn pixel_luma(&self, x: u32, y: u32) -> u8 {
        let [r, g, b, _] = self.pixel(x, y).unwrap_or([0, 0, 0, 0]);
        (0.2126 * r as f32 + 0.7152 * g as f32 + 0.0722 * b as f32).round() as u8
    }
}

fn watermark_position(base: &RgbaFrame, mark: &RgbaFrame, watermark: &Watermark) -> (i32, i32) {
    let base_width = base.width as i32;
    let base_height = base.height as i32;
    let mark_width = mark.width as i32;
    let mark_height = mark.height as i32;
    match watermark.anchor {
        WatermarkAnchor::TopLeft => (watermark.margin_x, watermark.margin_y),
        WatermarkAnchor::TopRight => (
            base_width - mark_width - watermark.margin_x,
            watermark.margin_y,
        ),
        WatermarkAnchor::BottomLeft => (
            watermark.margin_x,
            base_height - mark_height - watermark.margin_y,
        ),
        WatermarkAnchor::BottomRight => (
            base_width - mark_width - watermark.margin_x,
            base_height - mark_height - watermark.margin_y,
        ),
        WatermarkAnchor::Center => (
            (base_width - mark_width) / 2 + watermark.margin_x,
            (base_height - mark_height) / 2 + watermark.margin_y,
        ),
    }
}

fn finite_or(value: f32, fallback: f32) -> f32 {
    if value.is_finite() { value } else { fallback }
}

fn clamp_u8(value: f32) -> u8 {
    value.round().clamp(0.0, 255.0) as u8
}

fn alpha_blend(dst: [u8; 4], src: [u8; 4]) -> [u8; 4] {
    let src_a = src[3] as f32 / 255.0;
    let dst_a = dst[3] as f32 / 255.0;
    let out_a = src_a + dst_a * (1.0 - src_a);

    if out_a <= f32::EPSILON {
        return [0, 0, 0, 0];
    }

    let blend_channel = |src_channel: u8, dst_channel: u8| -> u8 {
        let src_value = src_channel as f32 / 255.0;
        let dst_value = dst_channel as f32 / 255.0;
        (((src_value * src_a + dst_value * dst_a * (1.0 - src_a)) / out_a) * 255.0).round() as u8
    };

    [
        blend_channel(src[0], dst[0]),
        blend_channel(src[1], dst[1]),
        blend_channel(src[2], dst[2]),
        (out_a * 255.0).round() as u8,
    ]
}

#[cfg(test)]
mod tests {
    use super::{
        ColorFilter, CropRect, FrameTransform, FrameTransformPipeline, RgbaFrame, Watermark,
        WatermarkAnchor,
    };

    #[test]
    fn crops_and_rotates() {
        let mut frame = RgbaFrame::solid(2, 2, [0, 0, 0, 255]);
        frame.set_pixel(1, 0, [255, 0, 0, 255]);

        let cropped = frame.crop(CropRect::new(1, 0, 1, 2)).unwrap();
        let rotated = cropped.rotate_90(true);

        assert_eq!(rotated.width, 2);
        assert_eq!(rotated.height, 1);
        assert_eq!(rotated.pixel(0, 0), Some([0, 0, 0, 255]));
        assert_eq!(rotated.pixel(1, 0), Some([255, 0, 0, 255]));
    }

    #[test]
    fn overlays_with_alpha() {
        let mut base = RgbaFrame::solid(1, 1, [0, 0, 255, 255]);
        let overlay = RgbaFrame::solid(1, 1, [255, 0, 0, 128]);

        base.overlay(&overlay, 0, 0);

        let pixel = base.pixel(0, 0).unwrap();
        assert!(pixel[0] > 120);
        assert!(pixel[2] > 120);
        assert_eq!(pixel[3], 255);
    }

    #[test]
    fn blurs_bright_pixels() {
        let mut frame = RgbaFrame::solid(3, 3, [0, 0, 0, 255]);
        frame.set_pixel(1, 1, [255, 255, 255, 255]);

        let blurred = frame.box_blur(1).unwrap();

        assert_eq!(blurred.pixel(1, 1), Some([28, 28, 28, 255]));
        assert_eq!(blurred.pixel(0, 0), Some([63, 63, 63, 255]));
    }

    #[test]
    fn applies_color_filters() {
        let frame = RgbaFrame::solid(1, 1, [100, 150, 200, 255]);

        let filtered = frame.apply_color_filter(ColorFilter::grayscale());

        assert_eq!(filtered.pixel(0, 0), Some([143, 143, 143, 255]));
    }

    #[test]
    fn applies_watermark_with_anchor_and_opacity() {
        let base = RgbaFrame::solid(3, 3, [10, 10, 10, 255]);
        let watermark = Watermark::new(RgbaFrame::solid(1, 1, [250, 10, 10, 255]))
            .with_anchor(WatermarkAnchor::TopLeft)
            .with_margin(1, 1)
            .with_opacity(0.5);

        let output = base.apply_watermark(&watermark);

        let marked = output.pixel(1, 1).unwrap();
        assert!((120..=140).contains(&marked[0]));
        assert_eq!(marked[1], 10);
        assert_eq!(marked[2], 10);
        assert_eq!(marked[3], 255);
        assert_eq!(output.pixel(0, 0), Some([10, 10, 10, 255]));
    }

    #[test]
    fn applies_transform_pipeline_in_order() {
        let frame = RgbaFrame::solid(2, 2, [0, 0, 255, 255]);
        let watermark = Watermark::new(RgbaFrame::solid(1, 1, [255, 0, 0, 255]))
            .with_anchor(WatermarkAnchor::TopLeft)
            .with_margin(1, 1);
        let pipeline = FrameTransformPipeline::new()
            .with(FrameTransform::Resize {
                width: 4,
                height: 4,
            })
            .with(FrameTransform::Watermark(watermark))
            .with(FrameTransform::Crop(CropRect::new(1, 1, 2, 2)));

        let output = pipeline.apply(&frame).unwrap();
        let all = pipeline.apply_all([&frame, &frame]).unwrap();

        assert_eq!(output.width, 2);
        assert_eq!(output.height, 2);
        assert_eq!(output.pixel(0, 0), Some([255, 0, 0, 255]));
        assert_eq!(all.len(), 2);
        assert_eq!(all[0], output);
    }

    #[test]
    fn detects_black_bars() {
        let mut frame = RgbaFrame::solid(4, 4, [0, 0, 0, 255]);
        for y in 1..3 {
            for x in 1..3 {
                frame.set_pixel(x, y, [200, 200, 200, 255]);
            }
        }

        let bars = frame.detect_black_bars(5, 1.0);

        assert_eq!(bars.top, 1);
        assert_eq!(bars.bottom, 1);
        assert_eq!(bars.left, 1);
        assert_eq!(bars.right, 1);
    }
}
