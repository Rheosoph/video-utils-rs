use crate::{
    codec::{CodecDescriptor, CodecId, VideoEncoder},
    error::{Error, Result},
    frame::RgbaFrame,
    packet::EncodedPacket,
    time::TimeBase,
};
use bytes::Bytes;
use rav1e::prelude::{
    ChromaSampling, Config, Context, EncoderConfig, EncoderStatus, Frame, FrameType, PixelRange,
    Plane, Rational,
};
use std::collections::BTreeMap;

/// Configuration for the Rust-native AV1 encoder backed by `rav1e`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Rav1eAv1EncoderOptions {
    /// rav1e speed preset in `0..=10`; higher is faster and lower quality.
    pub speed: u8,
    /// rav1e base quantizer in `0..=255`; lower is higher quality.
    pub quantizer: usize,
    /// Maximum keyframe interval in frames. `0` lets rav1e use its internal maximum.
    pub max_key_frame_interval: u64,
    /// Worker threads. `0` uses rav1e's default.
    pub threads: usize,
}

impl Rav1eAv1EncoderOptions {
    /// Create options suitable for quick utility transcodes.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            speed: 10,
            quantizer: 120,
            max_key_frame_interval: 120,
            threads: 0,
        }
    }

    /// Set rav1e speed preset.
    #[must_use]
    pub const fn with_speed(mut self, speed: u8) -> Self {
        self.speed = speed;
        self
    }

    /// Set rav1e base quantizer.
    #[must_use]
    pub const fn with_quantizer(mut self, quantizer: usize) -> Self {
        self.quantizer = quantizer;
        self
    }

    /// Set maximum keyframe interval.
    #[must_use]
    pub const fn with_max_key_frame_interval(mut self, frames: u64) -> Self {
        self.max_key_frame_interval = frames;
        self
    }

    /// Set worker thread count. `0` uses rav1e's default.
    #[must_use]
    pub const fn with_threads(mut self, threads: usize) -> Self {
        self.threads = threads;
        self
    }
}

impl Default for Rav1eAv1EncoderOptions {
    fn default() -> Self {
        Self::new()
    }
}

/// Rust-native AV1 encoder for RGBA frames using `rav1e`.
///
/// The encoder converts input frames to 8-bit full-range YUV 4:2:0 before
/// passing them to rav1e. Width and height are fixed for the encoder lifetime.
pub struct Rav1eAv1Encoder {
    context: Context<u8>,
    track_id: u32,
    width: u32,
    height: u32,
    time_base: TimeBase,
    frame_duration: i64,
    codec_config: Bytes,
    next_input_frameno: u64,
    pts_by_input_frameno: BTreeMap<u64, i64>,
    flushed: bool,
}

impl Rav1eAv1Encoder {
    /// Create an AV1 encoder with default rav1e options.
    pub fn new(
        track_id: u32,
        width: u32,
        height: u32,
        time_base: TimeBase,
        frame_duration: i64,
    ) -> Result<Self> {
        Self::with_options(
            track_id,
            width,
            height,
            time_base,
            frame_duration,
            Rav1eAv1EncoderOptions::default(),
        )
    }

    /// Create an AV1 encoder with explicit rav1e options.
    pub fn with_options(
        track_id: u32,
        width: u32,
        height: u32,
        time_base: TimeBase,
        frame_duration: i64,
        options: Rav1eAv1EncoderOptions,
    ) -> Result<Self> {
        if width == 0 || height == 0 {
            return Err(Error::InvalidFrameBuffer {
                expected: 1,
                actual: 0,
            });
        }
        if frame_duration < 0 {
            return Err(Error::InvalidPacketTiming {
                reason: "negative frame duration",
            });
        }

        let mut encoder_config = EncoderConfig::with_speed_preset(options.speed.min(10));
        encoder_config.width = width as usize;
        encoder_config.height = height as usize;
        encoder_config.time_base = Rational::new(time_base.num as u64, time_base.den as u64);
        encoder_config.bit_depth = 8;
        encoder_config.chroma_sampling = ChromaSampling::Cs420;
        encoder_config.pixel_range = PixelRange::Full;
        encoder_config.low_latency = true;
        encoder_config.quantizer = options.quantizer.min(255);
        encoder_config.set_key_frame_interval(0, options.max_key_frame_interval);

        let config = Config::new()
            .with_encoder_config(encoder_config)
            .with_threads(options.threads);
        let context = config
            .new_context::<u8>()
            .map_err(|err| codec_error("configure", err.to_string()))?;
        let codec_config = Bytes::from(context.container_sequence_header());

        Ok(Self {
            context,
            track_id,
            width,
            height,
            time_base,
            frame_duration,
            codec_config,
            next_input_frameno: 0,
            pts_by_input_frameno: BTreeMap::new(),
            flushed: false,
        })
    }

    /// AV1 codec-private sequence header suitable for Matroska/WebM/ISOBMFF metadata.
    #[must_use]
    pub fn codec_config(&self) -> Bytes {
        self.codec_config.clone()
    }

    /// Encoded stream width.
    #[must_use]
    pub const fn width(&self) -> u32 {
        self.width
    }

    /// Encoded stream height.
    #[must_use]
    pub const fn height(&self) -> u32 {
        self.height
    }

    fn drain_packets(&mut self) -> Result<Vec<EncodedPacket>> {
        let mut output = Vec::new();
        loop {
            match self.context.receive_packet() {
                Ok(packet) => output.push(self.packet_from_rav1e(packet)),
                Err(EncoderStatus::Encoded) => continue,
                Err(EncoderStatus::NeedMoreData | EncoderStatus::LimitReached) => break,
                Err(status) => return Err(codec_error("encode", status.to_string())),
            }
        }
        Ok(output)
    }

    fn packet_from_rav1e(&mut self, packet: rav1e::Packet<u8>) -> EncodedPacket {
        let pts = self
            .pts_by_input_frameno
            .remove(&packet.input_frameno)
            .unwrap_or_else(|| packet.input_frameno as i64 * self.frame_duration);
        EncodedPacket::new(
            self.track_id,
            CodecId::AV1,
            pts,
            self.frame_duration,
            self.time_base,
            packet.data,
        )
        .with_keyframe(matches!(
            packet.frame_type,
            FrameType::KEY | FrameType::INTRA_ONLY
        ))
    }
}

impl CodecDescriptor for Rav1eAv1Encoder {
    fn name(&self) -> &'static str {
        "rav1e-av1-encoder"
    }

    fn codec_id(&self) -> CodecId {
        CodecId::AV1
    }
}

impl VideoEncoder for Rav1eAv1Encoder {
    fn encode_frame(&mut self, frame: &RgbaFrame, pts: i64) -> Result<Vec<EncodedPacket>> {
        if self.flushed {
            return Err(codec_error(
                "encode",
                "cannot send frames after encoder finish",
            ));
        }
        if frame.width != self.width || frame.height != self.height {
            return Err(Error::InvalidFrameBuffer {
                expected: self.width as usize * self.height as usize * 4,
                actual: frame.width as usize * frame.height as usize * 4,
            });
        }

        let input_frameno = self.next_input_frameno;
        self.next_input_frameno += 1;
        self.pts_by_input_frameno.insert(input_frameno, pts);

        let rav_frame = rgba_to_rav1e_frame(&self.context, frame);
        self.context
            .send_frame(rav_frame)
            .map_err(|status| codec_error("send frame", status.to_string()))?;
        self.drain_packets()
    }

    fn finish(&mut self) -> Result<Vec<EncodedPacket>> {
        if !self.flushed {
            self.context
                .send_frame(None)
                .map_err(|status| codec_error("finish", status.to_string()))?;
            self.flushed = true;
        }
        self.drain_packets()
    }
}

fn rgba_to_rav1e_frame(context: &Context<u8>, source: &RgbaFrame) -> Frame<u8> {
    let mut frame = context.new_frame();
    fill_luma_plane(&mut frame.planes[0], source);
    let (luma_and_u, v) = frame.planes.split_at_mut(2);
    fill_chroma_planes(&mut luma_and_u[1], &mut v[0], source);
    for plane in &mut frame.planes {
        plane.pad(source.width as usize, source.height as usize);
    }
    frame
}

fn fill_luma_plane(plane: &mut Plane<u8>, source: &RgbaFrame) {
    let stride = plane.cfg.stride;
    let width = source.width as usize;
    let height = source.height as usize;
    let origin = plane.data_origin_mut();
    for y in 0..height {
        let output_row = &mut origin[y * stride..y * stride + width];
        for (x, sample) in output_row.iter_mut().enumerate() {
            let [r, g, b, _] = source.pixel(x as u32, y as u32).unwrap_or([0, 0, 0, 255]);
            *sample = rgb_to_y(r, g, b);
        }
    }
}

fn fill_chroma_planes(u_plane: &mut Plane<u8>, v_plane: &mut Plane<u8>, source: &RgbaFrame) {
    let u_stride = u_plane.cfg.stride;
    let v_stride = v_plane.cfg.stride;
    let chroma_width = source.width.div_ceil(2) as usize;
    let chroma_height = source.height.div_ceil(2) as usize;
    let u_origin = u_plane.data_origin_mut();
    let v_origin = v_plane.data_origin_mut();

    for cy in 0..chroma_height {
        let u_row = &mut u_origin[cy * u_stride..cy * u_stride + chroma_width];
        let v_row = &mut v_origin[cy * v_stride..cy * v_stride + chroma_width];
        for cx in 0..chroma_width {
            let mut r_sum = 0u32;
            let mut g_sum = 0u32;
            let mut b_sum = 0u32;
            let mut samples = 0u32;

            for dy in 0..2 {
                let y = cy as u32 * 2 + dy;
                if y >= source.height {
                    continue;
                }
                for dx in 0..2 {
                    let x = cx as u32 * 2 + dx;
                    if x >= source.width {
                        continue;
                    }
                    let [r, g, b, _] = source.pixel(x, y).unwrap_or([0, 0, 0, 255]);
                    r_sum += u32::from(r);
                    g_sum += u32::from(g);
                    b_sum += u32::from(b);
                    samples += 1;
                }
            }

            let samples = samples.max(1);
            let r = (r_sum / samples) as u8;
            let g = (g_sum / samples) as u8;
            let b = (b_sum / samples) as u8;
            u_row[cx] = rgb_to_u(r, g, b);
            v_row[cx] = rgb_to_v(r, g, b);
        }
    }
}

fn rgb_to_y(r: u8, g: u8, b: u8) -> u8 {
    clamp_u8(0.299 * f32::from(r) + 0.587 * f32::from(g) + 0.114 * f32::from(b))
}

fn rgb_to_u(r: u8, g: u8, b: u8) -> u8 {
    clamp_u8(128.0 - 0.168_736 * f32::from(r) - 0.331_264 * f32::from(g) + 0.5 * f32::from(b))
}

fn rgb_to_v(r: u8, g: u8, b: u8) -> u8 {
    clamp_u8(128.0 + 0.5 * f32::from(r) - 0.418_688 * f32::from(g) - 0.081_312 * f32::from(b))
}

fn clamp_u8(value: f32) -> u8 {
    value.round().clamp(0.0, 255.0) as u8
}

fn codec_error(operation: &'static str, message: impl Into<String>) -> Error {
    Error::CodecBackend {
        codec: CodecId::AV1,
        operation,
        message: message.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rav1e_encoder_produces_av1_packets() {
        let time_base = TimeBase::new(1, 30).unwrap();
        let mut encoder = Rav1eAv1Encoder::new(1, 16, 16, time_base, 1).unwrap();
        let frame = RgbaFrame::solid(16, 16, [32, 96, 160, 255]);

        let mut packets = encoder.encode_frame(&frame, 0).unwrap();
        packets.extend(encoder.finish().unwrap());

        assert!(!packets.is_empty());
        assert_eq!(packets[0].codec, CodecId::AV1);
        assert!(!packets[0].data.is_empty());
        assert!(!encoder.codec_config().is_empty());
    }
}
