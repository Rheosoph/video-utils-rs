use crate::{
    codec::{CodecId, MediaType},
    container::{ContainerDemuxer, ContainerFormat, DemuxedMedia},
    error::{Error, Result},
    media::{MediaInfo, StreamInfo},
    packet::{EncodedPacket, validate_monotonic_by_track},
    time::TimeBase,
};
use bytes::Bytes;
use matroska_demuxer::{Frame, MatroskaFile, TrackEntry, TrackType};
use std::{collections::BTreeMap, io::Cursor};

/// Matroska/WebM demuxer backed by `matroska-demuxer`.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct MatroskaDemuxer {
    format: ContainerFormat,
}

impl MatroskaDemuxer {
    /// Create a Matroska/WebM demuxer for the given EBML container format.
    pub fn new(format: ContainerFormat) -> Result<Self> {
        if !matches!(format, ContainerFormat::Matroska | ContainerFormat::WebM) {
            return Err(Error::Unsupported {
                operation: "matroska demux",
                reason: "only Matroska and WebM containers are handled by MatroskaDemuxer",
            });
        }

        Ok(Self { format })
    }
}

impl ContainerDemuxer for MatroskaDemuxer {
    fn container_format(&self) -> ContainerFormat {
        self.format
    }

    fn demux_bytes(&self, bytes: &Bytes) -> Result<DemuxedMedia> {
        demux_matroska_bytes(self.format, bytes)
    }
}

/// Probe stream metadata from Matroska/WebM bytes.
pub fn probe_matroska_bytes(format: ContainerFormat, bytes: &Bytes) -> Result<MediaInfo> {
    demux_matroska_bytes(format, bytes).map(|demuxed| demuxed.media)
}

/// Demux Matroska/WebM bytes into stream metadata and encoded packets.
pub fn demux_matroska_bytes(format: ContainerFormat, bytes: &Bytes) -> Result<DemuxedMedia> {
    let demuxer = MatroskaDemuxer::new(format)?;
    let mut mkv = MatroskaFile::open(Cursor::new(bytes.as_ref()))
        .map_err(|err| matroska_parse_error(format, err))?;

    let detected_format = format_from_doc_type(mkv.ebml_header().doc_type()).unwrap_or(format);
    let time_base = time_base_from_timestamp_scale(mkv.info().timestamp_scale().get())?;
    let mut media = MediaInfo {
        duration_seconds: mkv
            .info()
            .duration()
            .map(|duration| duration * time_base.num as f64 / time_base.den as f64),
        ..Default::default()
    };
    if let Some(title) = mkv.info().title() {
        media.tags.insert("title".to_owned(), title.to_owned());
    }

    let mut track_context = BTreeMap::new();
    for track in mkv.tracks() {
        let stream = stream_info_for_track(track, time_base, detected_format)?;
        track_context.insert(
            u32::try_from(track.track_number().get()).map_err(|_| Error::Parse {
                format: detected_format.as_str(),
                message: "track number is too large for u32".to_owned(),
            })?,
            TrackPacketContext {
                codec: stream.codec.clone(),
                media_type: stream.media_type,
                time_base: stream.time_base,
                default_duration: default_duration_ticks(track, time_base, detected_format)?,
            },
        );
        media.push_stream(stream);
    }

    let mut packets = Vec::new();
    let mut frame = Frame::default();
    while mkv
        .next_frame(&mut frame)
        .map_err(|err| matroska_parse_error(detected_format, err))?
    {
        let track_id = u32::try_from(frame.track).map_err(|_| Error::Parse {
            format: detected_format.as_str(),
            message: "frame track number is too large for u32".to_owned(),
        })?;
        let context = track_context.get(&track_id).ok_or(Error::Parse {
            format: detected_format.as_str(),
            message: format!("frame references unknown track {track_id}"),
        })?;
        let duration = frame
            .duration
            .and_then(|duration| i64::try_from(duration).ok())
            .or(context.default_duration)
            .unwrap_or(0);
        let pts = u64_to_i64(frame.timestamp, detected_format, "frame timestamp")?;
        let is_keyframe = frame
            .is_keyframe
            .unwrap_or(context.media_type != MediaType::Video);
        packets.push(
            EncodedPacket::new(
                track_id,
                context.codec.clone(),
                pts,
                duration,
                context.time_base,
                Bytes::copy_from_slice(&frame.data),
            )
            .with_keyframe(is_keyframe),
        );
    }

    fill_missing_packet_durations(&mut packets);
    sort_packets_by_decode_time(&mut packets);
    validate_monotonic_by_track(&packets)?;

    Ok(DemuxedMedia::new(
        demuxer.container_format(),
        media,
        packets,
    ))
}

#[derive(Clone, Debug)]
struct TrackPacketContext {
    codec: CodecId,
    media_type: MediaType,
    time_base: TimeBase,
    default_duration: Option<i64>,
}

fn stream_info_for_track(
    track: &TrackEntry,
    time_base: TimeBase,
    format: ContainerFormat,
) -> Result<StreamInfo> {
    let track_id = u32::try_from(track.track_number().get()).map_err(|_| Error::Parse {
        format: format.as_str(),
        message: "track number is too large for u32".to_owned(),
    })?;
    let media_type = media_type_from_track(track.track_type());
    let codec = codec_from_matroska_id(track.codec_id());
    let mut stream = StreamInfo::new(track_id, media_type, codec, time_base);

    if let Some(duration) = default_duration_ticks(track, time_base, format)? {
        stream.duration = Some(duration);
    }
    if let Some(language) = track.language_bcp47().or_else(|| track.language()) {
        stream.language = Some(language.to_owned());
    }
    if let Some(codec_private) = track.codec_private() {
        stream.codec_config = Some(Bytes::copy_from_slice(codec_private));
    }
    if let Some(name) = track.name() {
        stream.tags.insert("name".to_owned(), name.to_owned());
    }
    if let Some(codec_name) = track.codec_name() {
        stream
            .tags
            .insert("codec_name".to_owned(), codec_name.to_owned());
    }
    stream
        .tags
        .insert("matroska_codec_id".to_owned(), track.codec_id().to_owned());

    if let Some(video) = track.video() {
        stream = stream.with_dimensions(
            u32::try_from(video.pixel_width().get()).map_err(|_| Error::Parse {
                format: format.as_str(),
                message: "video width is too large for u32".to_owned(),
            })?,
            u32::try_from(video.pixel_height().get()).map_err(|_| Error::Parse {
                format: format.as_str(),
                message: "video height is too large for u32".to_owned(),
            })?,
        );
    }
    if let Some(audio) = track.audio() {
        stream = stream.with_audio_format(
            round_f64_to_u32(audio.sampling_frequency(), format, "sampling frequency")?,
            u16::try_from(audio.channels().get()).map_err(|_| Error::Parse {
                format: format.as_str(),
                message: "audio channel count is too large for u16".to_owned(),
            })?,
        );
    }

    Ok(stream)
}

fn media_type_from_track(track_type: TrackType) -> MediaType {
    match track_type {
        TrackType::Video => MediaType::Video,
        TrackType::Audio => MediaType::Audio,
        TrackType::Subtitle => MediaType::Subtitle,
        TrackType::Metadata => MediaType::Data,
        TrackType::Unknown
        | TrackType::Complex
        | TrackType::Logo
        | TrackType::Buttons
        | TrackType::Control => MediaType::Data,
    }
}

fn codec_from_matroska_id(codec_id: &str) -> CodecId {
    match codec_id {
        "V_MPEG4/ISO/AVC" => CodecId::H264,
        "V_MPEGH/ISO/HEVC" => CodecId::H265,
        "V_AV1" => CodecId::AV1,
        "V_VP8" => CodecId::VP8,
        "V_VP9" => CodecId::VP9,
        "V_MPEG1" => CodecId::Mpeg1Video,
        "V_MPEG2" => CodecId::Mpeg2Video,
        "V_MPEG4/ISO/ASP" | "V_MPEG4/ISO/SP" => CodecId::Mpeg4Part2,
        "V_PRORES" => CodecId::ProRes,
        "V_THEORA" => CodecId::Theora,
        "V_UNCOMPRESSED" => CodecId::RawVideo,
        "A_AAC" => CodecId::Aac,
        "A_AC3" => CodecId::Ac3,
        "A_EAC3" => CodecId::Eac3,
        "A_ALAC" => CodecId::Alac,
        "A_OPUS" => CodecId::Opus,
        "A_FLAC" => CodecId::Flac,
        "A_MPEG/L1" => CodecId::Mp1,
        "A_MPEG/L2" => CodecId::Mp2,
        "A_MPEG/L3" => CodecId::Mp3,
        "A_PCM/INT/BIG" | "A_PCM/INT/LIT" | "A_PCM/FLOAT/IEEE" => CodecId::Pcm,
        "A_VORBIS" => CodecId::Vorbis,
        "A_SPEEX" => CodecId::Speex,
        "A_DTS" | "A_DTS/EXPRESS" | "A_DTS/LOSSLESS" => CodecId::Dts,
        "A_WAVPACK4" => CodecId::WavPack,
        "S_TEXT/UTF8" | "S_TEXT/ASCII" => CodecId::Srt,
        "S_TEXT/WEBVTT" => CodecId::WebVtt,
        other => CodecId::Unknown(other.to_owned()),
    }
}

fn default_duration_ticks(
    track: &TrackEntry,
    time_base: TimeBase,
    format: ContainerFormat,
) -> Result<Option<i64>> {
    let Some(default_duration_ns) = track.default_duration() else {
        return Ok(None);
    };

    let seconds = default_duration_ns.get() as f64 / 1_000_000_000.0;
    let ticks = (seconds * time_base.den as f64 / time_base.num as f64).round();
    if ticks > i64::MAX as f64 {
        return Err(Error::Parse {
            format: format.as_str(),
            message: "default duration is too large for packet timing".to_owned(),
        });
    }
    Ok(Some(ticks as i64))
}

fn time_base_from_timestamp_scale(timestamp_scale_ns: u64) -> Result<TimeBase> {
    let gcd = gcd_u64(timestamp_scale_ns, 1_000_000_000);
    let num = timestamp_scale_ns / gcd;
    let den = 1_000_000_000 / gcd;
    TimeBase::new(
        num.try_into().map_err(|_| Error::Parse {
            format: "matroska",
            message: "timestamp scale numerator is too large for TimeBase".to_owned(),
        })?,
        den.try_into().map_err(|_| Error::Parse {
            format: "matroska",
            message: "timestamp scale denominator is too large for TimeBase".to_owned(),
        })?,
    )
}

fn format_from_doc_type(doc_type: &str) -> Option<ContainerFormat> {
    match doc_type.trim_end_matches('\0') {
        "webm" => Some(ContainerFormat::WebM),
        "matroska" => Some(ContainerFormat::Matroska),
        _ => None,
    }
}

fn sort_packets_by_decode_time(packets: &mut [EncodedPacket]) {
    packets.sort_by(|left, right| {
        let left_ts = left.time_base.ticks_to_seconds(left.decode_order_ts());
        let right_ts = right.time_base.ticks_to_seconds(right.decode_order_ts());
        left_ts
            .total_cmp(&right_ts)
            .then_with(|| left.track_id.cmp(&right.track_id))
    });
}

fn fill_missing_packet_durations(packets: &mut [EncodedPacket]) {
    let mut by_track = BTreeMap::<u32, Vec<usize>>::new();
    for (index, packet) in packets.iter().enumerate() {
        by_track.entry(packet.track_id).or_default().push(index);
    }

    for indices in by_track.values() {
        for pair in indices.windows(2) {
            let current = pair[0];
            let next = pair[1];
            if packets[current].duration == 0 && packets[next].pts > packets[current].pts {
                packets[current].duration = packets[next].pts - packets[current].pts;
            }
        }
    }
}

fn round_f64_to_u32(value: f64, format: ContainerFormat, field: &'static str) -> Result<u32> {
    if !value.is_finite() || value < 0.0 || value > u32::MAX as f64 {
        return Err(Error::Parse {
            format: format.as_str(),
            message: format!("{field} is outside u32 range"),
        });
    }
    Ok(value.round() as u32)
}

fn u64_to_i64(value: u64, format: ContainerFormat, field: &'static str) -> Result<i64> {
    value.try_into().map_err(|_| Error::Parse {
        format: format.as_str(),
        message: format!("{field} is too large for packet timing"),
    })
}

fn gcd_u64(mut a: u64, mut b: u64) -> u64 {
    while b != 0 {
        let remainder = a % b;
        a = b;
        b = remainder;
    }
    a
}

fn matroska_parse_error(format: ContainerFormat, err: matroska_demuxer::DemuxError) -> Error {
    Error::Parse {
        format: format.as_str(),
        message: err.to_string(),
    }
}
