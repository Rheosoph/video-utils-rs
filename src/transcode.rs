use crate::{container::ContainerFormat, error::Error, transform::ObjectVideoTransformJob};
use object_store::path::Path;

/// High-level object-store media job.
///
/// The job first uses container policy to decide whether packet-copy remuxing
/// is enough. If a video stage is configured, the selected video stream is
/// decoded, transformed, encoded, and then muxed with any compatible preserved
/// packet-copy streams.
#[derive(Clone, Debug, PartialEq)]
pub struct ObjectTranscodeJob {
    /// Optional decoded-frame video stage.
    pub video: Option<ObjectVideoTransformJob>,
    /// Permit exact object copies or packet-copy remuxes when no decode stage is needed.
    pub allow_packet_copy: bool,
}

impl ObjectTranscodeJob {
    /// Create a packet-copy-first job with no configured decode stage.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            video: None,
            allow_packet_copy: true,
        }
    }

    /// Attach a decoded-frame video stage.
    #[must_use]
    pub fn with_video(mut self, video: ObjectVideoTransformJob) -> Self {
        self.video = Some(video);
        self
    }

    /// Control whether exact copies and packet-copy remuxes may be used.
    #[must_use]
    pub const fn allow_packet_copy(mut self, allow: bool) -> Self {
        self.allow_packet_copy = allow;
        self
    }
}

impl Default for ObjectTranscodeJob {
    fn default() -> Self {
        Self::new()
    }
}

/// Operation selected by a high-level transcode/remux job.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ObjectTranscodeOperation {
    /// Same-store object copy via [`object_store::ObjectStore::copy`].
    SameStoreCopy,
    /// Cross-store byte transfer via object-store get/put.
    CrossStoreByteCopy,
    /// Same-store demux plus packet-copy mux into a different container.
    SameStorePacketCopyMux,
    /// Cross-store demux plus packet-copy mux into a different container.
    CrossStorePacketCopyMux,
    /// Decoded video transform plus encode plus mux.
    VideoTranscodeMux,
}

/// Report returned by object-store transcode/remux orchestration.
#[derive(Clone, Debug, PartialEq)]
pub struct ObjectTranscodeReport {
    /// Source object key.
    pub source: Path,
    /// Target object key.
    pub target: Path,
    /// Source container format.
    pub source_format: ContainerFormat,
    /// Target container format.
    pub target_format: ContainerFormat,
    /// Operation selected by the job.
    pub operation: ObjectTranscodeOperation,
    /// Initial source-to-target packet-copy plan when media was inspected.
    pub plan: Option<crate::container::RemuxPlan>,
    /// Encoded input packets read from the source object.
    pub input_packets: usize,
    /// Output packets submitted to the muxer.
    pub output_packets: usize,
    /// Selected video track when a video transcode stage ran.
    pub video_track_id: Option<u32>,
    /// Decoded video frames transformed by the job.
    pub decoded_video_frames: usize,
    /// Encoded video packets produced by the video encoder.
    pub encoded_video_packets: usize,
    /// Packets copied without decode into the output mux.
    pub copied_packets: usize,
    /// Packets dropped because their streams were not preserved.
    pub dropped_packets: usize,
    /// Bytes written to the target object.
    pub bytes_written: u64,
}

/// Return the unsupported error used when a transcode stage is required but absent.
#[must_use]
pub const fn missing_transcode_stage_error() -> Error {
    Error::Unsupported {
        operation: "object transcode",
        reason: "target requires transcoding, but no matching decode/encode stage is configured",
    }
}

#[cfg(feature = "containers")]
mod object_store_impl {
    use super::{ObjectTranscodeJob, ObjectTranscodeOperation, ObjectTranscodeReport};
    use crate::{
        codec::{CodecId, MediaType, VideoDecoder, VideoEncoder},
        container::{ContainerFormat, RemuxPlan, plan_container_remux},
        error::{Error, Result},
        media::{MediaInfo, StreamInfo},
        object_store_io::{
            ObjectMuxReport, ObjectRemuxOperation, ObjectRemuxReport, demux_object, mux_object,
            remux_object_between_stores, remux_object_same_store,
        },
        packet::EncodedPacket,
    };
    use bytes::Bytes;
    use object_store::{ObjectStore, path::Path};

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum StorePair {
        Same,
        Cross,
    }

    /// Run a packet-copy-first transcode/remux job inside one object store.
    pub async fn transcode_object_same_store(
        store: &dyn ObjectStore,
        source: &Path,
        target: &Path,
        job: &ObjectTranscodeJob,
        video_decoder: Option<&mut dyn VideoDecoder>,
        video_encoder: Option<&mut dyn VideoEncoder>,
    ) -> Result<ObjectTranscodeReport> {
        transcode_object_inner(
            store,
            source,
            store,
            target,
            job,
            video_decoder,
            video_encoder,
            StorePair::Same,
        )
        .await
    }

    /// Run a packet-copy-first transcode/remux job between object stores.
    pub async fn transcode_object_between_stores(
        source_store: &dyn ObjectStore,
        source: &Path,
        target_store: &dyn ObjectStore,
        target: &Path,
        job: &ObjectTranscodeJob,
        video_decoder: Option<&mut dyn VideoDecoder>,
        video_encoder: Option<&mut dyn VideoEncoder>,
    ) -> Result<ObjectTranscodeReport> {
        transcode_object_inner(
            source_store,
            source,
            target_store,
            target,
            job,
            video_decoder,
            video_encoder,
            StorePair::Cross,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn transcode_object_inner(
        source_store: &dyn ObjectStore,
        source: &Path,
        target_store: &dyn ObjectStore,
        target: &Path,
        job: &ObjectTranscodeJob,
        video_decoder: Option<&mut dyn VideoDecoder>,
        video_encoder: Option<&mut dyn VideoEncoder>,
        store_pair: StorePair,
    ) -> Result<ObjectTranscodeReport> {
        if job.video.is_none() {
            if !job.allow_packet_copy {
                return Err(super::missing_transcode_stage_error());
            }
            return remux_without_decode(source_store, source, target_store, target, store_pair)
                .await;
        }

        let video_job = job
            .video
            .as_ref()
            .expect("checked above that video stage exists");
        let decoder = video_decoder.ok_or(Error::Unsupported {
            operation: "object transcode",
            reason: "video transcode requires a video decoder backend",
        })?;
        let encoder = video_encoder.ok_or(Error::Unsupported {
            operation: "object transcode",
            reason: "video transcode requires a video encoder backend",
        })?;

        let demuxed = demux_object(source_store, source).await?;
        let target_format = target_format_from_path(target)?;
        let plan = plan_container_remux(demuxed.format, target_format, &demuxed.media)?;
        let video_track_id = select_video_track(&demuxed.media.streams, video_job.video_track_id)?;
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
                operation: "object transcode",
                reason: "encoder must produce a video codec",
            });
        }

        let mut output_media = demuxed.media.clone();
        let mut output_packets = Vec::new();
        let mut decoded_video_frames = 0usize;
        let mut encoded_video_packets = 0usize;
        let mut copied_packets = 0usize;
        let mut dropped_packets = 0usize;
        let mut output_dimensions = None::<(u32, u32)>;
        let mut output_time_base = None;
        let output_codec = encoder.codec_id();

        for packet in &demuxed.packets {
            if packet.track_id != video_track_id {
                if should_copy_non_selected_stream(
                    &plan,
                    &demuxed.media,
                    target_format,
                    packet.track_id,
                    video_job.preserve_non_video,
                )? {
                    output_packets.push(packet.clone());
                    copied_packets += 1;
                } else {
                    dropped_packets += 1;
                }
                continue;
            }

            let frames = decoder.decode_packet(packet)?;
            for frame in frames {
                decoded_video_frames += 1;
                let transformed = video_job.pipeline.apply(&frame)?;
                output_dimensions = Some((transformed.width, transformed.height));
                let mut encoded = encoder.encode_frame(&transformed, packet.pts)?;
                validate_encoder_packets(video_track_id, &encoded)?;
                if let Some(packet) = encoded.first() {
                    output_time_base = Some(packet.time_base);
                }
                encoded_video_packets += encoded.len();
                output_packets.append(&mut encoded);
            }
        }

        for frame in decoder.flush()? {
            decoded_video_frames += 1;
            let transformed = video_job.pipeline.apply(&frame)?;
            output_dimensions = Some((transformed.width, transformed.height));
            let mut encoded = encoder.encode_frame(&transformed, 0)?;
            validate_encoder_packets(video_track_id, &encoded)?;
            if let Some(packet) = encoded.first() {
                output_time_base = Some(packet.time_base);
            }
            encoded_video_packets += encoded.len();
            output_packets.append(&mut encoded);
        }

        let finished = encoder.finish()?;
        validate_encoder_packets(video_track_id, &finished)?;
        if let Some(packet) = finished.first() {
            output_time_base = Some(packet.time_base);
        }
        encoded_video_packets += finished.len();
        output_packets.extend(finished);

        if encoded_video_packets == 0 {
            return Err(Error::IncompatibleTrack {
                track_id: video_track_id,
                reason: "selected video track produced no encoded packets",
            });
        }

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
            video_job.output_video_codec_config.clone(),
        );
        retain_output_streams(
            &mut output_media,
            target_format,
            video_track_id,
            video_job.preserve_non_video,
        )?;

        if output_packets.is_empty() || output_media.streams.is_empty() {
            return Err(Error::EmptyInput);
        }
        sort_packets_by_decode_time(&mut output_packets);

        let muxed = mux_object(target_store, target, &output_media, &output_packets).await?;
        Ok(report_from_video_transcode(
            source,
            target,
            demuxed.format,
            muxed,
            plan,
            demuxed.packets.len(),
            video_track_id,
            decoded_video_frames,
            encoded_video_packets,
            copied_packets,
            dropped_packets,
        ))
    }

    async fn remux_without_decode(
        source_store: &dyn ObjectStore,
        source: &Path,
        target_store: &dyn ObjectStore,
        target: &Path,
        store_pair: StorePair,
    ) -> Result<ObjectTranscodeReport> {
        let report = match store_pair {
            StorePair::Same => remux_object_same_store(source_store, source, target, None).await?,
            StorePair::Cross => {
                remux_object_between_stores(source_store, source, target_store, target, None)
                    .await?
            }
        };
        Ok(report_from_remux(report))
    }

    fn report_from_remux(report: ObjectRemuxReport) -> ObjectTranscodeReport {
        ObjectTranscodeReport {
            source: report.source,
            target: report.target,
            source_format: report.source_format,
            target_format: report.target_format,
            operation: operation_from_remux(report.operation),
            plan: report.plan,
            input_packets: 0,
            output_packets: 0,
            video_track_id: None,
            decoded_video_frames: 0,
            encoded_video_packets: 0,
            copied_packets: 0,
            dropped_packets: 0,
            bytes_written: report.bytes_written,
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn report_from_video_transcode(
        source: &Path,
        target: &Path,
        source_format: ContainerFormat,
        muxed: ObjectMuxReport,
        plan: RemuxPlan,
        input_packets: usize,
        video_track_id: u32,
        decoded_video_frames: usize,
        encoded_video_packets: usize,
        copied_packets: usize,
        dropped_packets: usize,
    ) -> ObjectTranscodeReport {
        ObjectTranscodeReport {
            source: source.clone(),
            target: target.clone(),
            source_format,
            target_format: muxed.target_format,
            operation: ObjectTranscodeOperation::VideoTranscodeMux,
            plan: Some(plan),
            input_packets,
            output_packets: muxed.packet_count,
            video_track_id: Some(video_track_id),
            decoded_video_frames,
            encoded_video_packets,
            copied_packets,
            dropped_packets,
            bytes_written: muxed.bytes_written,
        }
    }

    fn operation_from_remux(operation: ObjectRemuxOperation) -> ObjectTranscodeOperation {
        match operation {
            ObjectRemuxOperation::SameStoreCopy => ObjectTranscodeOperation::SameStoreCopy,
            ObjectRemuxOperation::CrossStoreByteCopy => {
                ObjectTranscodeOperation::CrossStoreByteCopy
            }
            ObjectRemuxOperation::SameStorePacketCopyMux => {
                ObjectTranscodeOperation::SameStorePacketCopyMux
            }
            ObjectRemuxOperation::CrossStorePacketCopyMux => {
                ObjectTranscodeOperation::CrossStorePacketCopyMux
            }
        }
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
                operation: "object transcode",
                reason: "source media has no video track",
            })
    }

    fn should_copy_non_selected_stream(
        plan: &RemuxPlan,
        media: &MediaInfo,
        target_format: ContainerFormat,
        track_id: u32,
        preserve: bool,
    ) -> Result<bool> {
        if !preserve {
            return Ok(false);
        }

        let stream = media.stream(track_id).ok_or(Error::IncompatibleTrack {
            track_id,
            reason: "packet references a missing stream",
        })?;
        let stream_plan = plan.stream(track_id).ok_or(Error::IncompatibleTrack {
            track_id,
            reason: "stream is missing from the remux plan",
        })?;
        if stream_plan.action.is_packet_copy() && target_format.supports_stream(stream) {
            return Ok(true);
        }

        Err(Error::IncompatibleTrack {
            track_id,
            reason: "non-selected stream cannot be packet-copied into target container",
        })
    }

    fn retain_output_streams(
        media: &mut MediaInfo,
        target_format: ContainerFormat,
        video_track_id: u32,
        preserve_non_video: bool,
    ) -> Result<()> {
        let mut unsupported_track = None;
        media.streams.retain(|stream| {
            if stream.track_id == video_track_id {
                if !target_format.supports_stream(stream) {
                    unsupported_track = Some(stream.track_id);
                    return false;
                }
                return true;
            }
            preserve_non_video && target_format.supports_stream(stream)
        });

        if let Some(track_id) = unsupported_track {
            return Err(Error::IncompatibleTrack {
                track_id,
                reason: "encoded video stream cannot be carried by target container",
            });
        }
        Ok(())
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

    fn validate_encoder_packets(track_id: u32, packets: &[EncodedPacket]) -> Result<()> {
        if packets.iter().all(|packet| packet.track_id == track_id) {
            return Ok(());
        }

        Err(Error::IncompatibleTrack {
            track_id,
            reason: "encoder produced packets for a different track",
        })
    }

    fn sort_packets_by_decode_time(packets: &mut [EncodedPacket]) {
        packets.sort_by(|left, right| {
            let left_ts = left.time_base.ticks_to_seconds(left.decode_order_ts());
            let right_ts = right.time_base.ticks_to_seconds(right.decode_order_ts());
            left_ts
                .total_cmp(&right_ts)
                .then_with(|| left.track_id.cmp(&right.track_id))
                .then_with(|| left.pts.cmp(&right.pts))
        });
    }

    fn target_format_from_path(target: &Path) -> Result<ContainerFormat> {
        ContainerFormat::from_path(target).ok_or(Error::Unsupported {
            operation: "container detection",
            reason: "target object key extension is not a recognized media container",
        })
    }
}

#[cfg(feature = "containers")]
pub use object_store_impl::{transcode_object_between_stores, transcode_object_same_store};
