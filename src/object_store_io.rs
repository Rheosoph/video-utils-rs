#[cfg(feature = "containers")]
use crate::bitstream::{
    aac::{audio_specific_config_from_adts, audio_specific_config_from_format},
    h264::{avcc_from_annex_b_sample, dimensions_from_annex_b_sample, dimensions_from_avcc},
};
#[cfg(feature = "containers")]
use crate::packet::EncodedPacket;
#[cfg(feature = "containers")]
use crate::{container::DemuxedMedia, containers};
use crate::{
    container::{ContainerFormat, RemuxPlan, plan_container_remux},
    error::{Error, Result},
    media::MediaInfo,
};
use bytes::Bytes;
use object_store::{ObjectStore, PutPayload, PutResult, path::Path};
use std::ops::Range;

const HEADER_PROBE_BYTES: u64 = 64;

/// Report returned by object-store byte copy helpers.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ObjectTransferReport {
    /// Source object key.
    pub source: Path,
    /// Target object key.
    pub target: Path,
    /// Bytes written to the target.
    pub bytes_written: u64,
    /// Source format when known from the key or header.
    pub source_format: Option<ContainerFormat>,
    /// Target format when known from the key.
    pub target_format: Option<ContainerFormat>,
}

/// Options for range-based object reads.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ObjectChunkReadOptions {
    /// Requested chunk size in bytes.
    pub chunk_size: u64,
}

impl ObjectChunkReadOptions {
    /// Create chunk read options.
    #[must_use]
    pub const fn new(chunk_size: u64) -> Self {
        Self { chunk_size }
    }
}

impl Default for ObjectChunkReadOptions {
    fn default() -> Self {
        Self {
            chunk_size: 8 * 1024 * 1024,
        }
    }
}

/// One object byte-range chunk.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ObjectReadChunk {
    /// Inclusive start byte offset.
    pub offset: u64,
    /// Chunk bytes.
    pub bytes: Bytes,
}

/// Operation used by a remux helper.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ObjectRemuxOperation {
    /// Same-store object copy via [`ObjectStore::copy`].
    SameStoreCopy,
    /// Cross-store byte transfer via [`ObjectStore::get`] and [`ObjectStore::put`].
    CrossStoreByteCopy,
    /// Same-store demux plus packet-copy mux into a different container.
    SameStorePacketCopyMux,
    /// Cross-store demux plus packet-copy mux into a different container.
    CrossStorePacketCopyMux,
}

/// Report returned after an object-store remux operation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ObjectRemuxReport {
    /// Source object key.
    pub source: Path,
    /// Target object key.
    pub target: Path,
    /// Source container format.
    pub source_format: ContainerFormat,
    /// Target container format.
    pub target_format: ContainerFormat,
    /// Bytes written to the target.
    pub bytes_written: u64,
    /// Object-store operation used.
    pub operation: ObjectRemuxOperation,
    /// Optional stream plan used for caller diagnostics.
    pub plan: Option<RemuxPlan>,
}

/// Report returned after muxing packet data into an object-store object.
#[cfg(feature = "containers")]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ObjectMuxReport {
    /// Target object key.
    pub target: Path,
    /// Target container format.
    pub target_format: ContainerFormat,
    /// Bytes written to the target.
    pub bytes_written: u64,
    /// Number of encoded packets submitted to the muxer.
    pub packet_count: usize,
}

/// Read an entire object into memory as bytes.
pub async fn read_object_bytes(store: &dyn ObjectStore, location: &Path) -> Result<Bytes> {
    store
        .get(location)
        .await
        .map_err(|err| object_store_error("get", err))?
        .bytes()
        .await
        .map_err(|err| object_store_error("read bytes", err))
}

/// Read one byte range from an object.
pub async fn read_object_range(
    store: &dyn ObjectStore,
    location: &Path,
    range: Range<u64>,
) -> Result<Bytes> {
    if range.start >= range.end {
        return Err(Error::InvalidRange {
            start: range.start as i64,
            end: range.end as i64,
        });
    }

    store
        .get_range(location, range)
        .await
        .map_err(|err| object_store_error("read range", err))
}

/// Read an object through bounded byte ranges.
pub async fn read_object_chunks(
    store: &dyn ObjectStore,
    location: &Path,
    options: ObjectChunkReadOptions,
) -> Result<Vec<ObjectReadChunk>> {
    if options.chunk_size == 0 {
        return Err(Error::InvalidRange { start: 0, end: 0 });
    }

    let meta = store
        .head(location)
        .await
        .map_err(|err| object_store_error("head", err))?;
    let mut chunks = Vec::new();
    let mut offset = 0u64;
    while offset < meta.size {
        let end = (offset + options.chunk_size).min(meta.size);
        let bytes = read_object_range(store, location, offset..end).await?;
        chunks.push(ObjectReadChunk { offset, bytes });
        offset = end;
    }

    Ok(chunks)
}

/// Write bytes to an object key.
pub async fn write_object_bytes(
    store: &dyn ObjectStore,
    location: &Path,
    bytes: impl Into<Bytes>,
) -> Result<PutResult> {
    store
        .put(location, PutPayload::from_bytes(bytes.into()))
        .await
        .map_err(|err| object_store_error("put", err))
}

/// Detect a media container for an object.
///
/// The object key extension is used first. If it is not recognized, the helper
/// probes a small object-store byte range and checks container magic bytes.
pub async fn detect_object_container_format(
    store: &dyn ObjectStore,
    location: &Path,
) -> Result<ContainerFormat> {
    if let Some(format) = ContainerFormat::from_path(location) {
        return Ok(format);
    }

    let meta = store
        .head(location)
        .await
        .map_err(|err| object_store_error("head", err))?;
    if meta.size == 0 {
        return Err(Error::EmptyInput);
    }

    let probe_len = meta.size.min(HEADER_PROBE_BYTES);
    let header = store
        .get_range(location, 0..probe_len)
        .await
        .map_err(|err| object_store_error("read header", err))?;
    ContainerFormat::from_magic(&header).ok_or(Error::Unsupported {
        operation: "container detection",
        reason: "object key extension and header are not a recognized media container",
    })
}

/// Copy an object inside one object store.
///
/// This uses [`ObjectStore::copy`] and does not read through local files.
pub async fn copy_object_same_store(
    store: &dyn ObjectStore,
    source: &Path,
    target: &Path,
) -> Result<ObjectTransferReport> {
    let meta = store
        .head(source)
        .await
        .map_err(|err| object_store_error("head", err))?;
    store
        .copy(source, target)
        .await
        .map_err(|err| object_store_error("copy", err))?;

    Ok(ObjectTransferReport {
        source: source.clone(),
        target: target.clone(),
        bytes_written: meta.size,
        source_format: ContainerFormat::from_path(source),
        target_format: ContainerFormat::from_path(target),
    })
}

/// Copy an object between object stores by fetching and writing bytes through
/// the object-store APIs.
pub async fn copy_object_between_stores(
    source_store: &dyn ObjectStore,
    source: &Path,
    target_store: &dyn ObjectStore,
    target: &Path,
) -> Result<ObjectTransferReport> {
    let source_format = ContainerFormat::from_path(source);
    let target_format = ContainerFormat::from_path(target);
    let bytes = read_object_bytes(source_store, source).await?;
    let bytes_written = bytes.len() as u64;
    write_object_bytes(target_store, target, bytes).await?;

    Ok(ObjectTransferReport {
        source: source.clone(),
        target: target.clone(),
        bytes_written,
        source_format,
        target_format,
    })
}

/// Build a stream-level remux plan using object-store source format detection.
pub async fn plan_object_remux(
    source_store: &dyn ObjectStore,
    source: &Path,
    target: &Path,
    media: &MediaInfo,
) -> Result<RemuxPlan> {
    let source_format = detect_object_container_format(source_store, source).await?;
    let target_format = target_format_from_path(target)?;
    plan_container_remux(source_format, target_format, media)
}

/// Probe media metadata by reading and parsing an object-store object.
#[cfg(feature = "containers")]
pub async fn probe_object_media_info(
    source_store: &dyn ObjectStore,
    source: &Path,
) -> Result<MediaInfo> {
    demux_object(source_store, source)
        .await
        .map(|demuxed| demuxed.media)
}

/// Demux an object-store media object into metadata plus encoded packets.
#[cfg(feature = "containers")]
pub async fn demux_object(source_store: &dyn ObjectStore, source: &Path) -> Result<DemuxedMedia> {
    let source_format = detect_object_container_format(source_store, source).await?;
    let bytes = read_object_bytes(source_store, source).await?;
    if source_format == ContainerFormat::RawElementary {
        return containers::demux_elementary_bytes_from_path(source, &bytes);
    }
    demux_container_bytes(source_format, &bytes)
}

/// Build a stream-level remux plan by probing source media from the object.
#[cfg(feature = "containers")]
pub async fn plan_object_remux_from_probe(
    source_store: &dyn ObjectStore,
    source: &Path,
    target: &Path,
) -> Result<RemuxPlan> {
    let demuxed = demux_object(source_store, source).await?;
    let target_format = target_format_from_path(target)?;
    plan_container_remux(demuxed.format, target_format, &demuxed.media)
}

/// Mux stream metadata and encoded packets into an object-store object.
#[cfg(feature = "containers")]
pub async fn mux_object(
    target_store: &dyn ObjectStore,
    target: &Path,
    media: &MediaInfo,
    packets: &[EncodedPacket],
) -> Result<ObjectMuxReport> {
    let target_format = target_format_from_path(target)?;
    let bytes = mux_container_bytes(target_format, media, packets)?;
    let bytes_written = bytes.len() as u64;
    write_object_bytes(target_store, target, bytes).await?;

    Ok(ObjectMuxReport {
        target: target.clone(),
        target_format,
        bytes_written,
        packet_count: packets.len(),
    })
}

/// Remux/copy an object inside a single object store.
///
/// Same-format remuxes are exact object copies. Cross-container remuxes demux
/// object bytes and mux compatible packet-copy streams when a target mux adapter
/// is available.
pub async fn remux_object_same_store(
    store: &dyn ObjectStore,
    source: &Path,
    target: &Path,
    media: Option<&MediaInfo>,
) -> Result<ObjectRemuxReport> {
    let source_format = detect_object_container_format(store, source).await?;
    let target_format = target_format_from_path(target)?;
    let plan = match media {
        Some(media) => Some(plan_container_remux(source_format, target_format, media)?),
        None => None,
    };

    if source_format != target_format {
        #[cfg(feature = "containers")]
        {
            let (bytes, plan) =
                remux_cross_container_bytes(store, source, source_format, target_format, media)
                    .await?;
            let bytes_written = bytes.len() as u64;
            write_object_bytes(store, target, bytes).await?;
            return Ok(ObjectRemuxReport {
                source: source.clone(),
                target: target.clone(),
                source_format,
                target_format,
                bytes_written,
                operation: ObjectRemuxOperation::SameStorePacketCopyMux,
                plan: Some(plan),
            });
        }

        #[cfg(not(feature = "containers"))]
        ensure_same_container_for_byte_copy(source_format, target_format)?;
    }
    ensure_same_container_for_byte_copy(source_format, target_format)?;

    let meta = store
        .head(source)
        .await
        .map_err(|err| object_store_error("head", err))?;
    store
        .copy(source, target)
        .await
        .map_err(|err| object_store_error("copy", err))?;

    Ok(ObjectRemuxReport {
        source: source.clone(),
        target: target.clone(),
        source_format,
        target_format,
        bytes_written: meta.size,
        operation: ObjectRemuxOperation::SameStoreCopy,
        plan,
    })
}

/// Remux/copy an object between object stores.
///
/// This is object-store-only IO. Same-format remuxes are exact byte transfers.
/// Cross-container remuxes demux object bytes and mux compatible packet-copy
/// streams when a target mux adapter is available.
pub async fn remux_object_between_stores(
    source_store: &dyn ObjectStore,
    source: &Path,
    target_store: &dyn ObjectStore,
    target: &Path,
    media: Option<&MediaInfo>,
) -> Result<ObjectRemuxReport> {
    let source_format = detect_object_container_format(source_store, source).await?;
    let target_format = target_format_from_path(target)?;
    let plan = match media {
        Some(media) => Some(plan_container_remux(source_format, target_format, media)?),
        None => None,
    };

    if source_format != target_format {
        #[cfg(feature = "containers")]
        {
            let (bytes, plan) = remux_cross_container_bytes(
                source_store,
                source,
                source_format,
                target_format,
                media,
            )
            .await?;
            let bytes_written = bytes.len() as u64;
            write_object_bytes(target_store, target, bytes).await?;
            return Ok(ObjectRemuxReport {
                source: source.clone(),
                target: target.clone(),
                source_format,
                target_format,
                bytes_written,
                operation: ObjectRemuxOperation::CrossStorePacketCopyMux,
                plan: Some(plan),
            });
        }

        #[cfg(not(feature = "containers"))]
        ensure_same_container_for_byte_copy(source_format, target_format)?;
    }
    ensure_same_container_for_byte_copy(source_format, target_format)?;

    let bytes = read_object_bytes(source_store, source).await?;
    let bytes_written = bytes.len() as u64;
    write_object_bytes(target_store, target, bytes).await?;

    Ok(ObjectRemuxReport {
        source: source.clone(),
        target: target.clone(),
        source_format,
        target_format,
        bytes_written,
        operation: ObjectRemuxOperation::CrossStoreByteCopy,
        plan,
    })
}

fn target_format_from_path(target: &Path) -> Result<ContainerFormat> {
    ContainerFormat::from_path(target).ok_or(Error::Unsupported {
        operation: "container detection",
        reason: "target object key extension is not a recognized media container",
    })
}

fn ensure_same_container_for_byte_copy(
    source: ContainerFormat,
    target: ContainerFormat,
) -> Result<()> {
    if source == target {
        return Ok(());
    }

    Err(Error::Unsupported {
        operation: "object remux",
        reason: "cross-container packet-copy rewrapping needs a demux/mux adapter; the object-store layer will not byte-copy different container formats",
    })
}

fn object_store_error(operation: &'static str, err: object_store::Error) -> Error {
    Error::ObjectStore {
        operation,
        message: err.to_string(),
    }
}

#[cfg(feature = "containers")]
fn demux_container_bytes(format: ContainerFormat, bytes: &Bytes) -> Result<DemuxedMedia> {
    match format {
        ContainerFormat::Mp4 | ContainerFormat::QuickTime => demux_mp4_family_bytes(format, bytes),
        ContainerFormat::Matroska | ContainerFormat::WebM => {
            containers::demux_matroska_bytes(format, bytes)
        }
        ContainerFormat::Ogg => containers::demux_ogg_bytes(bytes),
        ContainerFormat::Wav => containers::demux_wav_bytes(bytes),
        ContainerFormat::MpegTs => containers::demux_mpeg_ts_bytes(bytes),
        ContainerFormat::Aiff => containers::demux_aiff_bytes(bytes),
        ContainerFormat::Flv => containers::demux_flv_bytes(bytes),
        ContainerFormat::MpegPs | ContainerFormat::Avi | ContainerFormat::RawElementary => {
            Err(Error::Unsupported {
                operation: "object demux",
                reason: "no demux adapter is wired for this container format yet",
            })
        }
    }
}

#[cfg(feature = "containers")]
fn demux_mp4_family_bytes(format: ContainerFormat, bytes: &Bytes) -> Result<DemuxedMedia> {
    if bytes.windows(4).any(|window| window == b"moof") {
        return containers::demux_fragmented_mp4_bytes(format, bytes);
    }
    containers::demux_iso_bmff_bytes(format, bytes)
}

#[cfg(feature = "containers")]
fn mux_container_bytes(
    format: ContainerFormat,
    media: &MediaInfo,
    packets: &[EncodedPacket],
) -> Result<Bytes> {
    let media = enrich_media_for_packet_mux(media, packets);
    match format {
        ContainerFormat::Mp4 | ContainerFormat::QuickTime => {
            containers::mux_iso_bmff_bytes(format, &media, packets)
        }
        ContainerFormat::Matroska | ContainerFormat::WebM => {
            containers::mux_matroska_bytes(format, &media, packets)
        }
        ContainerFormat::Ogg => containers::mux_ogg_bytes(&media, packets),
        ContainerFormat::Wav => containers::mux_wav_bytes(&media, packets),
        ContainerFormat::MpegTs => containers::mux_mpeg_ts_bytes(&media, packets),
        ContainerFormat::RawElementary => containers::mux_elementary_bytes(&media, packets),
        ContainerFormat::Aiff => containers::mux_aiff_bytes(&media, packets),
        ContainerFormat::Flv => containers::mux_flv_bytes(&media, packets),
        ContainerFormat::MpegPs | ContainerFormat::Avi => Err(Error::Unsupported {
            operation: "object mux",
            reason: "no mux adapter is wired for this target container format yet",
        }),
    }
}

#[cfg(feature = "containers")]
fn enrich_media_for_packet_mux(media: &MediaInfo, packets: &[EncodedPacket]) -> MediaInfo {
    let mut enriched = media.clone();
    for stream in &mut enriched.streams {
        match stream.codec {
            crate::codec::CodecId::H264 => {
                if (stream.width.is_none() || stream.height.is_none())
                    && let Some(config) = &stream.codec_config
                    && let Ok((width, height)) = dimensions_from_avcc(config)
                {
                    stream.width = Some(width);
                    stream.height = Some(height);
                }
                let sample = packets
                    .iter()
                    .find(|packet| packet.track_id == stream.track_id && packet.is_keyframe)
                    .or_else(|| {
                        packets
                            .iter()
                            .find(|packet| packet.track_id == stream.track_id)
                    });
                if let Some(sample) = sample {
                    if stream.codec_config.is_none()
                        && let Ok(config) = avcc_from_annex_b_sample(&sample.data)
                    {
                        stream.codec_config = Some(config);
                    }
                    if (stream.width.is_none() || stream.height.is_none())
                        && let Ok((width, height)) = dimensions_from_annex_b_sample(&sample.data)
                    {
                        stream.width = Some(width);
                        stream.height = Some(height);
                    }
                }
            }
            crate::codec::CodecId::Aac => {
                if stream.codec_config.is_none()
                    && let (Some(sample_rate), Some(channels)) =
                        (stream.sample_rate, stream.channels)
                    && let Ok(config) = audio_specific_config_from_format(sample_rate, channels)
                {
                    stream.codec_config = Some(config);
                }
                let sample = packets
                    .iter()
                    .find(|packet| packet.track_id == stream.track_id);
                if let Some(sample) = sample
                    && let Ok((config, sample_rate, channels)) =
                        audio_specific_config_from_adts(&sample.data)
                {
                    if stream.codec_config.is_none() {
                        stream.codec_config = Some(config);
                    }
                    if stream.sample_rate.is_none() {
                        stream.sample_rate = Some(sample_rate);
                    }
                    if stream.channels.is_none() {
                        stream.channels = Some(channels);
                    }
                }
            }
            _ => {}
        }
    }
    enriched
}

#[cfg(feature = "containers")]
async fn remux_cross_container_bytes(
    source_store: &dyn ObjectStore,
    source: &Path,
    source_format: ContainerFormat,
    target_format: ContainerFormat,
    supplied_media: Option<&MediaInfo>,
) -> Result<(Bytes, RemuxPlan)> {
    if let Some(media) = supplied_media {
        let supplied_plan = plan_container_remux(source_format, target_format, media)?;
        ensure_packet_copy_plan(&supplied_plan)?;
    }

    let demuxed = demux_object(source_store, source).await?;
    let plan = plan_container_remux(demuxed.format, target_format, &demuxed.media)?;
    ensure_packet_copy_plan(&plan)?;
    let bytes = mux_container_bytes(target_format, &demuxed.media, &demuxed.packets)?;
    Ok((bytes, plan))
}

#[cfg(feature = "containers")]
fn ensure_packet_copy_plan(plan: &RemuxPlan) -> Result<()> {
    if plan.is_packet_copy_only() {
        return Ok(());
    }

    Err(Error::Unsupported {
        operation: "object remux",
        reason: "target remux plan requires transcoding or unsupported streams; only packet-copy muxing is available",
    })
}
