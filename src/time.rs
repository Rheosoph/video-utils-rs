use crate::error::{Error, Result};
use std::fmt;

/// Rational time base used by encoded packets and streams.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct TimeBase {
    /// Numerator in seconds.
    pub num: i32,
    /// Denominator in seconds.
    pub den: i32,
}

impl TimeBase {
    /// Create a validated time base.
    pub fn new(num: i32, den: i32) -> Result<Self> {
        if num <= 0 || den <= 0 {
            return Err(Error::InvalidTimeBase { num, den });
        }

        Ok(Self { num, den })
    }

    /// Common millisecond time base.
    #[must_use]
    pub const fn milliseconds() -> Self {
        Self { num: 1, den: 1_000 }
    }

    /// Common microsecond time base.
    #[must_use]
    pub const fn microseconds() -> Self {
        Self {
            num: 1,
            den: 1_000_000,
        }
    }

    /// Convert ticks in this time base to seconds.
    #[must_use]
    pub fn ticks_to_seconds(self, ticks: i64) -> f64 {
        ticks as f64 * self.num as f64 / self.den as f64
    }

    /// Convert seconds to ticks in this time base, rounded to the nearest tick.
    #[must_use]
    pub fn seconds_to_ticks(self, seconds: f64) -> i64 {
        (seconds * self.den as f64 / self.num as f64).round() as i64
    }

    /// Rescale a timestamp from this time base into another time base.
    #[must_use]
    pub fn rescale(self, ticks: i64, target: Self) -> i64 {
        let numerator = ticks as i128 * self.num as i128 * target.den as i128;
        let denominator = self.den as i128 * target.num as i128;
        div_round_i128(numerator, denominator) as i64
    }
}

impl fmt::Display for TimeBase {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.num, self.den)
    }
}

fn div_round_i128(value: i128, divisor: i128) -> i128 {
    debug_assert!(divisor > 0);
    if value >= 0 {
        (value + divisor / 2) / divisor
    } else {
        (value - divisor / 2) / divisor
    }
}

#[cfg(test)]
mod tests {
    use super::TimeBase;

    #[test]
    fn rescales_between_time_bases() {
        let ms = TimeBase::milliseconds();
        let video = TimeBase::new(1, 90_000).unwrap();

        assert_eq!(ms.rescale(1_500, video), 135_000);
        assert_eq!(video.rescale(135_000, ms), 1_500);
    }
}
