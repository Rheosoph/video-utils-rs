use crate::error::{Error, Result};

/// Decoded interleaved floating-point audio frame.
#[derive(Clone, Debug, PartialEq)]
pub struct AudioFrame {
    /// Samples per second.
    pub sample_rate: u32,
    /// Number of interleaved channels.
    pub channels: u16,
    /// Presentation timestamp expressed in samples at `sample_rate`.
    pub pts: i64,
    /// Interleaved f32 samples, nominally in `[-1.0, 1.0]`.
    pub samples_f32_interleaved: Vec<f32>,
}

impl AudioFrame {
    /// Create a validated audio frame.
    pub fn new(
        sample_rate: u32,
        channels: u16,
        pts: i64,
        samples_f32_interleaved: Vec<f32>,
    ) -> Result<Self> {
        if sample_rate == 0 {
            return Err(Error::InvalidAudioBuffer {
                reason: "sample rate must be non-zero",
            });
        }
        if channels == 0 {
            return Err(Error::InvalidAudioBuffer {
                reason: "channel count must be non-zero",
            });
        }
        if !samples_f32_interleaved
            .len()
            .is_multiple_of(channels as usize)
        {
            return Err(Error::InvalidAudioBuffer {
                reason: "interleaved sample count is not divisible by channel count",
            });
        }

        Ok(Self {
            sample_rate,
            channels,
            pts,
            samples_f32_interleaved,
        })
    }

    /// Number of sample frames.
    #[must_use]
    pub fn sample_frames(&self) -> usize {
        self.samples_f32_interleaved.len() / self.channels as usize
    }

    /// Duration in seconds.
    #[must_use]
    pub fn duration_seconds(&self) -> f64 {
        self.sample_frames() as f64 / self.sample_rate as f64
    }

    /// End timestamp in sample units.
    #[must_use]
    pub fn end_pts(&self) -> i64 {
        self.pts + self.sample_frames() as i64
    }

    /// Maximum absolute sample value.
    #[must_use]
    pub fn peak_amplitude(&self) -> f32 {
        self.samples_f32_interleaved
            .iter()
            .map(|sample| sample.abs())
            .fold(0.0, f32::max)
    }

    /// Root mean square amplitude across all samples.
    #[must_use]
    pub fn rms(&self) -> f32 {
        if self.samples_f32_interleaved.is_empty() {
            return 0.0;
        }

        let sum = self
            .samples_f32_interleaved
            .iter()
            .map(|sample| sample * sample)
            .sum::<f32>();

        (sum / self.samples_f32_interleaved.len() as f32).sqrt()
    }
}

/// Fade curve shape.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FadeShape {
    /// Linear amplitude ramp.
    Linear,
    /// Sine-based equal-power ramp.
    EqualPower,
}

/// Waveform summary bucket.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WaveformBucket {
    /// Inclusive start sample frame.
    pub start_sample: usize,
    /// Exclusive end sample frame.
    pub end_sample: usize,
    /// Minimum sample value in the bucket.
    pub min: f32,
    /// Maximum sample value in the bucket.
    pub max: f32,
    /// RMS amplitude in the bucket.
    pub rms: f32,
}

/// Detected silence range.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SilenceRange {
    /// Inclusive start sample frame.
    pub start_sample: usize,
    /// Exclusive end sample frame.
    pub end_sample: usize,
}

/// Convert decibels to a linear gain factor.
#[must_use]
pub fn db_to_linear(db: f32) -> f32 {
    10.0_f32.powf(db / 20.0)
}

/// Apply a linear gain factor in place with hard clipping.
pub fn apply_gain(frame: &mut AudioFrame, gain: f32) {
    for sample in &mut frame.samples_f32_interleaved {
        *sample = (*sample * gain).clamp(-1.0, 1.0);
    }
}

/// Apply gain in decibels in place with hard clipping.
pub fn apply_gain_db(frame: &mut AudioFrame, db: f32) {
    apply_gain(frame, db_to_linear(db));
}

/// Peak-normalize audio in place.
pub fn normalize_peak(frame: &mut AudioFrame, target_peak: f32) -> Result<()> {
    if !(0.0..=1.0).contains(&target_peak) {
        return Err(Error::InvalidAudioBuffer {
            reason: "target peak must be between 0.0 and 1.0",
        });
    }

    let peak = frame.peak_amplitude();
    if peak > 0.0 {
        apply_gain(frame, target_peak / peak);
    }

    Ok(())
}

/// Apply fade in/out envelopes in place.
pub fn fade(
    frame: &mut AudioFrame,
    fade_in_samples: usize,
    fade_out_samples: usize,
    shape: FadeShape,
) {
    let frames = frame.sample_frames();
    let channels = frame.channels as usize;

    for sample_index in 0..frames {
        let fade_in = if fade_in_samples == 0 || sample_index >= fade_in_samples {
            1.0
        } else {
            envelope(sample_index as f32 / fade_in_samples as f32, shape)
        };

        let samples_from_end = frames.saturating_sub(sample_index + 1);
        let fade_out = if fade_out_samples == 0 || samples_from_end >= fade_out_samples {
            1.0
        } else {
            envelope(samples_from_end as f32 / fade_out_samples as f32, shape)
        };

        let factor = fade_in.min(fade_out);
        let base = sample_index * channels;
        for channel in 0..channels {
            frame.samples_f32_interleaved[base + channel] *= factor;
        }
    }
}

/// Mix frames with identical sample rate and channel count.
pub fn mix(frames: &[AudioFrame]) -> Result<AudioFrame> {
    if frames.is_empty() {
        return Err(Error::EmptyInput);
    }

    let sample_rate = frames[0].sample_rate;
    let channels = frames[0].channels;
    for frame in frames {
        if frame.sample_rate != sample_rate || frame.channels != channels {
            return Err(Error::InvalidAudioBuffer {
                reason: "all frames must have the same sample rate and channel count",
            });
        }
    }

    let start = frames.iter().map(|frame| frame.pts).min().unwrap();
    let end = frames.iter().map(AudioFrame::end_pts).max().unwrap();
    let output_frames = (end - start) as usize;
    let mut output = vec![0.0; output_frames * channels as usize];

    for frame in frames {
        let offset_frames = (frame.pts - start) as usize;
        let offset = offset_frames * channels as usize;
        for (index, sample) in frame.samples_f32_interleaved.iter().enumerate() {
            output[offset + index] = (output[offset + index] + *sample).clamp(-1.0, 1.0);
        }
    }

    AudioFrame::new(sample_rate, channels, start, output)
}

/// Summarize a frame into waveform buckets.
pub fn waveform_peaks(frame: &AudioFrame, buckets: usize) -> Result<Vec<WaveformBucket>> {
    if buckets == 0 {
        return Err(Error::InvalidAudioBuffer {
            reason: "bucket count must be non-zero",
        });
    }

    let sample_frames = frame.sample_frames();
    if sample_frames == 0 {
        return Ok(Vec::new());
    }

    let channels = frame.channels as usize;
    let mut output = Vec::with_capacity(buckets.min(sample_frames));

    for bucket_index in 0..buckets {
        let start = bucket_index * sample_frames / buckets;
        let end = ((bucket_index + 1) * sample_frames / buckets).min(sample_frames);
        if start == end {
            continue;
        }

        let mut min = f32::INFINITY;
        let mut max = f32::NEG_INFINITY;
        let mut sum_squares = 0.0;
        let mut count = 0usize;

        for sample in &frame.samples_f32_interleaved[start * channels..end * channels] {
            min = min.min(*sample);
            max = max.max(*sample);
            sum_squares += sample * sample;
            count += 1;
        }

        output.push(WaveformBucket {
            start_sample: start,
            end_sample: end,
            min,
            max,
            rms: (sum_squares / count as f32).sqrt(),
        });
    }

    Ok(output)
}

/// Detect silence using windowed RMS.
pub fn detect_silence(
    frame: &AudioFrame,
    threshold_db: f32,
    window_samples: usize,
    min_duration_samples: usize,
) -> Result<Vec<SilenceRange>> {
    if window_samples == 0 || min_duration_samples == 0 {
        return Err(Error::InvalidAudioBuffer {
            reason: "window and minimum duration must be non-zero",
        });
    }

    let threshold = db_to_linear(threshold_db);
    let sample_frames = frame.sample_frames();
    let channels = frame.channels as usize;
    let mut ranges = Vec::new();
    let mut current_start = None;
    let mut cursor = 0usize;

    while cursor < sample_frames {
        let end = (cursor + window_samples).min(sample_frames);
        let mut sum_squares = 0.0;
        let mut count = 0usize;

        for sample in &frame.samples_f32_interleaved[cursor * channels..end * channels] {
            sum_squares += sample * sample;
            count += 1;
        }

        let rms = if count == 0 {
            0.0
        } else {
            (sum_squares / count as f32).sqrt()
        };

        if rms <= threshold {
            current_start.get_or_insert(cursor);
        } else if let Some(start) = current_start.take()
            && cursor.saturating_sub(start) >= min_duration_samples
        {
            ranges.push(SilenceRange {
                start_sample: start,
                end_sample: cursor,
            });
        }

        cursor = end;
    }

    if let Some(start) = current_start
        && sample_frames.saturating_sub(start) >= min_duration_samples
    {
        ranges.push(SilenceRange {
            start_sample: start,
            end_sample: sample_frames,
        });
    }

    Ok(ranges)
}

fn envelope(position: f32, shape: FadeShape) -> f32 {
    let position = position.clamp(0.0, 1.0);
    match shape {
        FadeShape::Linear => position,
        FadeShape::EqualPower => (position * std::f32::consts::FRAC_PI_2).sin(),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        AudioFrame, FadeShape, apply_gain_db, detect_silence, fade, mix, normalize_peak,
        waveform_peaks,
    };

    #[test]
    fn peak_normalizes_and_applies_gain() {
        let mut frame = AudioFrame::new(48_000, 1, 0, vec![0.25, -0.5]).unwrap();

        normalize_peak(&mut frame, 1.0).unwrap();
        apply_gain_db(&mut frame, -6.0);

        assert!(frame.peak_amplitude() < 0.51);
        assert!(frame.peak_amplitude() > 0.49);
    }

    #[test]
    fn mixes_aligned_by_pts() {
        let a = AudioFrame::new(10, 1, 0, vec![0.5, 0.5]).unwrap();
        let b = AudioFrame::new(10, 1, 1, vec![0.25, 0.25]).unwrap();

        let mixed = mix(&[a, b]).unwrap();

        assert_eq!(mixed.samples_f32_interleaved, vec![0.5, 0.75, 0.25]);
    }

    #[test]
    fn summarizes_waveform_and_silence() {
        let frame = AudioFrame::new(10, 1, 0, vec![0.0, 0.0, 0.5, -0.5]).unwrap();

        let peaks = waveform_peaks(&frame, 2).unwrap();
        let silence = detect_silence(&frame, -40.0, 1, 2).unwrap();

        assert_eq!(peaks.len(), 2);
        assert_eq!(silence[0].start_sample, 0);
        assert_eq!(silence[0].end_sample, 2);
    }

    #[test]
    fn fades_in_and_out() {
        let mut frame = AudioFrame::new(10, 1, 0, vec![1.0; 5]).unwrap();

        fade(&mut frame, 2, 2, FadeShape::Linear);

        assert_eq!(frame.samples_f32_interleaved[0], 0.0);
        assert!(frame.samples_f32_interleaved[2] > 0.9);
        assert_eq!(frame.samples_f32_interleaved[4], 0.0);
    }
}
