use crate::{
    codec::{CodecDescriptor, CodecId, Decoder, Encoder},
    error::{Error, Result},
    subtitle::{SubtitleEvent, SubtitleFormat, parse_subtitles, write_srt, write_webvtt},
};
use std::str;

/// Concrete SRT/WebVTT sidecar text codec.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SubtitleTextCodec {
    format: SubtitleFormat,
}

impl SubtitleTextCodec {
    /// Create a subtitle text codec for the given format.
    #[must_use]
    pub const fn new(format: SubtitleFormat) -> Self {
        Self { format }
    }

    /// Create an SRT text codec.
    #[must_use]
    pub const fn srt() -> Self {
        Self::new(SubtitleFormat::Srt)
    }

    /// Create a WebVTT text codec.
    #[must_use]
    pub const fn webvtt() -> Self {
        Self::new(SubtitleFormat::WebVtt)
    }

    /// Subtitle format handled by this codec.
    #[must_use]
    pub const fn format(self) -> SubtitleFormat {
        self.format
    }
}

impl CodecDescriptor for SubtitleTextCodec {
    fn name(&self) -> &'static str {
        match self.format {
            SubtitleFormat::Srt => "subtitle-text/srt",
            SubtitleFormat::WebVtt => "subtitle-text/webvtt",
        }
    }

    fn codec_id(&self) -> CodecId {
        match self.format {
            SubtitleFormat::Srt => CodecId::Srt,
            SubtitleFormat::WebVtt => CodecId::WebVtt,
        }
    }
}

impl Decoder for SubtitleTextCodec {
    type Input = [u8];
    type Output = Vec<SubtitleEvent>;

    fn decode(&mut self, input: &Self::Input) -> Result<Self::Output> {
        let input = str::from_utf8(input).map_err(|err| Error::Parse {
            format: "subtitle",
            message: format!("input is not valid UTF-8: {err}"),
        })?;
        parse_subtitles(self.format, input)
    }
}

impl Encoder for SubtitleTextCodec {
    type Input = [SubtitleEvent];
    type Output = Vec<u8>;

    fn encode(&mut self, input: &Self::Input) -> Result<Self::Output> {
        let output = match self.format {
            SubtitleFormat::Srt => write_srt(input),
            SubtitleFormat::WebVtt => write_webvtt(input),
        };
        Ok(output.into_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::SubtitleTextCodec;
    use crate::{Decoder, Encoder, SubtitleEvent};

    #[test]
    fn decodes_and_encodes_srt() {
        let mut codec = SubtitleTextCodec::srt();
        let events = codec
            .decode(b"1\n00:00:01,000 --> 00:00:02,000\nhello\n\n")
            .unwrap();
        let output = codec.encode(&events).unwrap();

        assert_eq!(events[0].start_ms, 1_000);
        assert!(
            String::from_utf8(output)
                .unwrap()
                .contains("00:00:01,000 --> 00:00:02,000")
        );
    }

    #[test]
    fn decodes_and_encodes_webvtt() {
        let mut codec = SubtitleTextCodec::webvtt();
        let events = codec
            .decode(b"WEBVTT\n\n00:00:01.000 --> 00:00:02.000\nhello\n\n")
            .unwrap();
        let output = codec.encode(&events).unwrap();

        assert_eq!(events[0].end_ms, 2_000);
        assert!(String::from_utf8(output).unwrap().starts_with("WEBVTT"));
    }

    #[test]
    fn rejects_non_utf8() {
        let mut codec = SubtitleTextCodec::srt();

        assert!(codec.decode(&[0xff, 0xfe]).is_err());
    }

    #[test]
    fn encoder_accepts_slices() {
        let events = vec![SubtitleEvent::new(0, 1_000, "hello").unwrap()];
        let mut codec = SubtitleTextCodec::srt();

        let output = codec.encode(events.as_slice()).unwrap();

        assert!(String::from_utf8(output).unwrap().contains("hello"));
    }
}
