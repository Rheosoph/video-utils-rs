use crate::{codec::CodecId, time::TimeBase};
use thiserror::Error;

/// Crate-wide result type.
pub type Result<T> = std::result::Result<T, Error>;

/// Errors returned by portable media operations.
#[derive(Debug, Error)]
pub enum Error {
    /// A caller supplied an empty input where at least one item was required.
    #[error("input is empty")]
    EmptyInput,

    /// A time base had a non-positive numerator or denominator.
    #[error("invalid time base {num}/{den}")]
    InvalidTimeBase {
        /// Time base numerator.
        num: i32,
        /// Time base denominator.
        den: i32,
    },

    /// A range had inverted or otherwise invalid bounds.
    #[error("invalid range: start={start}, end={end}")]
    InvalidRange {
        /// Inclusive start value.
        start: i64,
        /// Exclusive end value.
        end: i64,
    },

    /// Packet timestamps or durations were internally inconsistent.
    #[error("invalid packet timing: {reason}")]
    InvalidPacketTiming {
        /// Human-readable timing problem.
        reason: &'static str,
    },

    /// Packets from two streams cannot be packet-copy concatenated.
    #[error("track {track_id} is incompatible: {reason}")]
    IncompatibleTrack {
        /// Track identifier.
        track_id: u32,
        /// Compatibility failure.
        reason: &'static str,
    },

    /// A codec mismatch prevented a packet-copy operation.
    #[error("codec mismatch: expected {expected:?}, actual {actual:?}")]
    CodecMismatch {
        /// Expected codec.
        expected: CodecId,
        /// Actual codec.
        actual: CodecId,
    },

    /// A time base mismatch prevented a packet-copy operation.
    #[error("time base mismatch: expected {expected}, actual {actual}")]
    TimeBaseMismatch {
        /// Expected time base.
        expected: TimeBase,
        /// Actual time base.
        actual: TimeBase,
    },

    /// Raw frame data did not match the declared dimensions or stride.
    #[error("invalid frame buffer: expected at least {expected} bytes, got {actual}")]
    InvalidFrameBuffer {
        /// Expected minimum byte length.
        expected: usize,
        /// Actual byte length.
        actual: usize,
    },

    /// Audio data did not match the declared channel layout.
    #[error("invalid audio buffer: {reason}")]
    InvalidAudioBuffer {
        /// Human-readable audio buffer problem.
        reason: &'static str,
    },

    /// Parsing failed.
    #[error("{format} parse error: {message}")]
    Parse {
        /// Format being parsed.
        format: &'static str,
        /// Human-readable parse problem.
        message: String,
    },

    /// Muxing failed.
    #[error("{format} mux error: {message}")]
    Mux {
        /// Format being written.
        format: &'static str,
        /// Human-readable muxing problem.
        message: String,
    },

    /// A concrete codec backend failed while decoding or encoding.
    #[error("{codec} {operation} codec backend error: {message}")]
    CodecBackend {
        /// Codec handled by the backend.
        codec: CodecId,
        /// Operation being performed.
        operation: &'static str,
        /// Backend error message.
        message: String,
    },

    /// An object store operation failed.
    #[error("object store {operation} failed: {message}")]
    ObjectStore {
        /// Operation name.
        operation: &'static str,
        /// Human-readable object store error.
        message: String,
    },

    /// The operation is intentionally outside the portable core.
    #[error("{operation} is unsupported by the portable core: {reason}")]
    Unsupported {
        /// Operation name.
        operation: &'static str,
        /// Reason it is not implemented in this feature set.
        reason: &'static str,
    },
}
