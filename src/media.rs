use crate::{codec::CodecId, codec::MediaType, time::TimeBase};
use bytes::Bytes;
use std::collections::BTreeMap;

/// Metadata for one stream/track in a media container.
#[derive(Clone, Debug, PartialEq)]
pub struct StreamInfo {
    /// Container track identifier.
    pub track_id: u32,
    /// Broad media type.
    pub media_type: MediaType,
    /// Encoded codec.
    pub codec: CodecId,
    /// Stream time base.
    pub time_base: TimeBase,
    /// Duration in stream time-base ticks.
    pub duration: Option<i64>,
    /// Width for video streams.
    pub width: Option<u32>,
    /// Height for video streams.
    pub height: Option<u32>,
    /// Sample rate for audio streams.
    pub sample_rate: Option<u32>,
    /// Channel count for audio streams.
    pub channels: Option<u16>,
    /// BCP-47-ish language tag when known.
    pub language: Option<String>,
    /// Optional codec-private config bytes.
    pub codec_config: Option<Bytes>,
    /// Container-level tags attached to this stream.
    pub tags: BTreeMap<String, String>,
}

impl StreamInfo {
    /// Create a new stream info with only required fields.
    #[must_use]
    pub fn new(track_id: u32, media_type: MediaType, codec: CodecId, time_base: TimeBase) -> Self {
        Self {
            track_id,
            media_type,
            codec,
            time_base,
            duration: None,
            width: None,
            height: None,
            sample_rate: None,
            channels: None,
            language: None,
            codec_config: None,
            tags: BTreeMap::new(),
        }
    }

    /// Set video dimensions.
    #[must_use]
    pub fn with_dimensions(mut self, width: u32, height: u32) -> Self {
        self.width = Some(width);
        self.height = Some(height);
        self
    }

    /// Set audio format fields.
    #[must_use]
    pub fn with_audio_format(mut self, sample_rate: u32, channels: u16) -> Self {
        self.sample_rate = Some(sample_rate);
        self.channels = Some(channels);
        self
    }

    /// Duration in seconds when duration is known.
    #[must_use]
    pub fn duration_seconds(&self) -> Option<f64> {
        self.duration
            .map(|duration| self.time_base.ticks_to_seconds(duration))
    }
}

/// Container-level probe result.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MediaInfo {
    /// Overall duration in seconds when known.
    pub duration_seconds: Option<f64>,
    /// Streams found in the container.
    pub streams: Vec<StreamInfo>,
    /// Container-level tags.
    pub tags: BTreeMap<String, String>,
}

impl MediaInfo {
    /// Add a stream.
    pub fn push_stream(&mut self, stream: StreamInfo) {
        self.streams.push(stream);
    }

    /// Find a stream by track id.
    #[must_use]
    pub fn stream(&self, track_id: u32) -> Option<&StreamInfo> {
        self.streams
            .iter()
            .find(|stream| stream.track_id == track_id)
    }

    /// Iterate video streams.
    pub fn video_streams(&self) -> impl Iterator<Item = &StreamInfo> {
        self.streams
            .iter()
            .filter(|stream| stream.media_type == MediaType::Video)
    }

    /// Iterate audio streams.
    pub fn audio_streams(&self) -> impl Iterator<Item = &StreamInfo> {
        self.streams
            .iter()
            .filter(|stream| stream.media_type == MediaType::Audio)
    }
}
