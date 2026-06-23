use crate::{container::ContainerFormat, error::Error, frame::FrameTransformPipeline};
use bytes::Bytes;
use object_store::path::Path;

/// Options for object-store video frame transformation.
#[derive(Clone, Debug, PartialEq)]
pub struct ObjectVideoTransformJob {
    /// Ordered decoded-frame transforms.
    pub pipeline: FrameTransformPipeline,
    /// Specific video track to transform. The first video track is used when unset.
    pub video_track_id: Option<u32>,
    /// Preserve non-video packets in the output container when supported.
    pub preserve_non_video: bool,
    /// Codec-private data produced by the output video encoder when needed.
    pub output_video_codec_config: Option<Bytes>,
}

impl ObjectVideoTransformJob {
    /// Create a transform job from a decoded-frame pipeline.
    #[must_use]
    pub fn new(pipeline: FrameTransformPipeline) -> Self {
        Self {
            pipeline,
            video_track_id: None,
            preserve_non_video: true,
            output_video_codec_config: None,
        }
    }

    /// Select a specific video track.
    #[must_use]
    pub const fn with_video_track(mut self, track_id: u32) -> Self {
        self.video_track_id = Some(track_id);
        self
    }

    /// Control whether non-video packets should be copied into the output.
    #[must_use]
    pub const fn preserve_non_video(mut self, preserve: bool) -> Self {
        self.preserve_non_video = preserve;
        self
    }

    /// Attach output video codec-private data for muxers that require it.
    #[must_use]
    pub fn with_output_video_codec_config(mut self, codec_config: impl Into<Bytes>) -> Self {
        self.output_video_codec_config = Some(codec_config.into());
        self
    }
}

/// Report returned by object-store video frame transformation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ObjectVideoTransformReport {
    /// Source object key.
    pub source: Path,
    /// Target object key.
    pub target: Path,
    /// Source container format.
    pub source_format: ContainerFormat,
    /// Target container format.
    pub target_format: ContainerFormat,
    /// Transformed video track id.
    pub video_track_id: u32,
    /// Encoded input packets read from the source object.
    pub input_packets: usize,
    /// Decoded frames that were transformed.
    pub decoded_frames: usize,
    /// Encoded video packets produced by the encoder.
    pub encoded_video_packets: usize,
    /// Non-video packets copied into the output.
    pub copied_packets: usize,
    /// Bytes written to the target object.
    pub bytes_written: u64,
}

#[cfg(feature = "containers")]
mod object_store_impl {
    use super::{ObjectVideoTransformJob, ObjectVideoTransformReport};
    use crate::{
        codec::{CodecId, MediaType, VideoDecoder, VideoEncoder},
        container::ContainerFormat,
        containers,
        error::{Error, Result},
        media::StreamInfo,
        object_store_io::{demux_object, detect_object_container_format, write_object_bytes},
        packet::EncodedPacket,
    };
    use bytes::Bytes;
    use object_store::{ObjectStore, path::Path};

    /// Transform video frames from one object-store object into another object in the same store.
    pub async fn transform_object_video_same_store(
        store: &dyn ObjectStore,
        source: &Path,
        target: &Path,
        job: &ObjectVideoTransformJob,
        decoder: &mut dyn VideoDecoder,
        encoder: &mut dyn VideoEncoder,
    ) -> Result<ObjectVideoTransformReport> {
        transform_object_video_between_stores(store, source, store, target, job, decoder, encoder)
            .await
    }

    /// Transform video frames between object-store objects using caller-supplied codecs.
    pub async fn transform_object_video_between_stores(
        source_store: &dyn ObjectStore,
        source: &Path,
        target_store: &dyn ObjectStore,
        target: &Path,
        job: &ObjectVideoTransformJob,
        decoder: &mut dyn VideoDecoder,
        encoder: &mut dyn VideoEncoder,
    ) -> Result<ObjectVideoTransformReport> {
        let source_format = detect_object_container_format(source_store, source).await?;
        let target_format = target_format_from_path(target)?;
        let demuxed = demux_object(source_store, source).await?;
        let video_track_id = select_video_track(&demuxed.media.streams, job.video_track_id)?;
        let input_stream =
            demuxed
                .media
                .stream(video_track_id)
                .ok_or(Error::IncompatibleTrack {
                    track_id: video_track_id,
                    reason: "selected video track is missing from source media",
                })?;
        if decoder.codec_id() != input_stream.codec {
            return Err(Error::CodecMismatch {
                expected: input_stream.codec.clone(),
                actual: decoder.codec_id(),
            });
        }
        if encoder.codec_id().media_type() != Some(MediaType::Video) {
            return Err(Error::Unsupported {
                operation: "video transform",
                reason: "encoder must produce a video codec",
            });
        }

        let mut output_media = demuxed.media.clone();
        let mut output_packets = Vec::<EncodedPacket>::new();
        let mut decoded_frames = 0usize;
        let mut encoded_video_packets = 0usize;
        let mut copied_packets = 0usize;
        let output_codec = encoder.codec_id();
        let mut output_dimensions = None::<(u32, u32)>;
        let mut output_time_base = None;

        for packet in &demuxed.packets {
            if packet.track_id != video_track_id {
                if job.preserve_non_video {
                    output_packets.push(packet.clone());
                    copied_packets += 1;
                }
                continue;
            }

            let frames = decoder.decode_packet(packet)?;
            for frame in frames {
                decoded_frames += 1;
                let transformed = job.pipeline.apply(&frame)?;
                output_dimensions = Some((transformed.width, transformed.height));
                let encoded = encoder.encode_frame(&transformed, packet.pts)?;
                if let Some(packet) = encoded.first() {
                    output_time_base = Some(packet.time_base);
                }
                encoded_video_packets += encoded.len();
                output_packets.extend(encoded);
            }
        }

        for frame in decoder.flush()? {
            decoded_frames += 1;
            let transformed = job.pipeline.apply(&frame)?;
            output_dimensions = Some((transformed.width, transformed.height));
            let encoded = encoder.encode_frame(&transformed, 0)?;
            if let Some(packet) = encoded.first() {
                output_time_base = Some(packet.time_base);
            }
            encoded_video_packets += encoded.len();
            output_packets.extend(encoded);
        }
        let finished = encoder.finish()?;
        if let Some(packet) = finished.first() {
            output_time_base = Some(packet.time_base);
        }
        encoded_video_packets += finished.len();
        output_packets.extend(finished);

        update_video_stream(
            output_media
                .streams
                .iter_mut()
                .find(|stream| stream.track_id == video_track_id)
                .ok_or(Error::IncompatibleTrack {
                    track_id: video_track_id,
                    reason: "selected video track is missing from output media",
                })?,
            output_codec,
            output_dimensions,
            output_time_base,
            job.output_video_codec_config.clone(),
        );

        output_packets.sort_by(|left, right| {
            let left_ts = left.time_base.ticks_to_seconds(left.decode_order_ts());
            let right_ts = right.time_base.ticks_to_seconds(right.decode_order_ts());
            left_ts
                .total_cmp(&right_ts)
                .then_with(|| left.track_id.cmp(&right.track_id))
                .then_with(|| left.pts.cmp(&right.pts))
        });

        let bytes = mux_container_bytes(target_format, &output_media, &output_packets)?;
        let bytes_written = bytes.len() as u64;
        write_object_bytes(target_store, target, bytes).await?;

        Ok(ObjectVideoTransformReport {
            source: source.clone(),
            target: target.clone(),
            source_format,
            target_format,
            video_track_id,
            input_packets: demuxed.packets.len(),
            decoded_frames,
            encoded_video_packets,
            copied_packets,
            bytes_written,
        })
    }

    fn select_video_track(streams: &[StreamInfo], requested: Option<u32>) -> Result<u32> {
        if let Some(track_id) = requested {
            let stream = streams
                .iter()
                .find(|stream| stream.track_id == track_id)
                .ok_or(Error::IncompatibleTrack {
                    track_id,
                    reason: "requested video track is missing",
                })?;
            if stream.media_type != MediaType::Video {
                return Err(Error::IncompatibleTrack {
                    track_id,
                    reason: "requested track is not video",
                });
            }
            return Ok(track_id);
        }

        streams
            .iter()
            .find(|stream| stream.media_type == MediaType::Video)
            .map(|stream| stream.track_id)
            .ok_or(Error::Unsupported {
                operation: "video transform",
                reason: "source media has no video track",
            })
    }

    fn update_video_stream(
        stream: &mut StreamInfo,
        codec: CodecId,
        dimensions: Option<(u32, u32)>,
        time_base: Option<crate::time::TimeBase>,
        codec_config: Option<Bytes>,
    ) {
        let codec_changed = stream.codec != codec;
        stream.codec = codec;
        if let Some(time_base) = time_base {
            stream.time_base = time_base;
        }
        if let Some((width, height)) = dimensions {
            stream.width = Some(width);
            stream.height = Some(height);
        }
        if let Some(codec_config) = codec_config {
            stream.codec_config = Some(codec_config);
        } else if codec_changed {
            stream.codec_config = None;
        }
    }

    fn target_format_from_path(target: &Path) -> Result<ContainerFormat> {
        ContainerFormat::from_path(target).ok_or(Error::Unsupported {
            operation: "container detection",
            reason: "target object key extension is not a recognized media container",
        })
    }

    fn mux_container_bytes(
        format: ContainerFormat,
        media: &crate::media::MediaInfo,
        packets: &[EncodedPacket],
    ) -> Result<Bytes> {
        match format {
            ContainerFormat::Mp4 | ContainerFormat::QuickTime => {
                containers::mux_iso_bmff_bytes(format, media, packets)
            }
            ContainerFormat::Matroska | ContainerFormat::WebM => {
                containers::mux_matroska_bytes(format, media, packets)
            }
            ContainerFormat::MpegTs => containers::mux_mpeg_ts_bytes(media, packets),
            ContainerFormat::RawElementary => containers::mux_elementary_bytes(media, packets),
            ContainerFormat::MpegPs
            | ContainerFormat::Avi
            | ContainerFormat::Flv
            | ContainerFormat::Ogg
            | ContainerFormat::Wav
            | ContainerFormat::Aiff => Err(Error::Unsupported {
                operation: "video transform",
                reason: "no mux adapter is wired for this target container format yet",
            }),
        }
    }
}

#[cfg(feature = "containers")]
pub use object_store_impl::{
    transform_object_video_between_stores, transform_object_video_same_store,
};

/// Return the unsupported error used when no video backend is available.
#[must_use]
pub const fn missing_video_backend_error() -> Error {
    Error::Unsupported {
        operation: "video transform",
        reason: "a video decoder and encoder backend must be supplied",
    }
}
