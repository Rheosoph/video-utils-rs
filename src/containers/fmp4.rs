use crate::{
    bitstream::{
        aac::aac_packet_to_raw, h264::h264_packet_to_length_prefixed,
        h265::h265_packet_to_length_prefixed,
    },
    codec::{CodecId, MediaType},
    container::{ContainerDemuxer, ContainerFormat, DemuxedMedia},
    error::{Error, Result},
    media::{MediaInfo, StreamInfo},
    packet::{EncodedPacket, validate_monotonic_by_track},
    time::TimeBase,
};
use bytes::Bytes;
use std::collections::{BTreeMap, BTreeSet};

const VIDEO_TIMESCALE: u32 = 90_000;
const MOVIE_TIMESCALE: u32 = 1_000;

/// Fragmented MP4 initialization and media segments.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FragmentedMp4Output {
    /// Initialization segment (`ftyp` + `moov`).
    pub init_segment: Bytes,
    /// Media segments (`moof` + `mdat`).
    pub media_segments: Vec<Bytes>,
}

/// Fragmented MP4 demuxer for initialization segments plus `moof`/`mdat` fragments.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct FragmentedMp4Demuxer {
    format: ContainerFormat,
}

impl FragmentedMp4Demuxer {
    /// Create an fMP4 demuxer for MP4 or QuickTime-family byte streams.
    pub fn new(format: ContainerFormat) -> Result<Self> {
        if !matches!(format, ContainerFormat::Mp4 | ContainerFormat::QuickTime) {
            return Err(Error::Unsupported {
                operation: "fmp4 demux",
                reason: "fragmented MP4 demuxing only handles MP4/MOV-family containers",
            });
        }

        Ok(Self { format })
    }
}

impl ContainerDemuxer for FragmentedMp4Demuxer {
    fn container_format(&self) -> ContainerFormat {
        self.format
    }

    fn demux_bytes(&self, bytes: &Bytes) -> Result<DemuxedMedia> {
        demux_fragmented_mp4_bytes(self.format, bytes)
    }
}

/// Mux packet-copy streams into fMP4 initialization and media segments.
///
/// The current writer supports H.264/H.265 video tracks with MP4 codec config
/// (`avcC`/`hvcC`), AAC audio tracks with AudioSpecificConfig, and WebVTT
/// subtitle tracks as `wvtt` cue samples.
pub fn mux_fragmented_mp4_segments(
    media: &MediaInfo,
    segment_packets: &[Vec<EncodedPacket>],
    fragment_duration_ms: u32,
) -> Result<FragmentedMp4Output> {
    if media.streams.is_empty() || segment_packets.is_empty() {
        return Err(Error::EmptyInput);
    }
    if fragment_duration_ms == 0 {
        return Err(Error::InvalidRange { start: 0, end: 0 });
    }

    let tracks = build_tracks(media)?;
    validate_segment_packets(&tracks, segment_packets)?;
    let init_segment = Bytes::from(build_init_segment(&tracks)?);
    let mut media_segments = Vec::with_capacity(segment_packets.len());
    for (index, packets) in segment_packets.iter().enumerate() {
        media_segments.push(Bytes::from(build_media_segment(
            &tracks,
            packets,
            u32::try_from(index + 1).map_err(|_| Error::Unsupported {
                operation: "fmp4 mux",
                reason: "too many fMP4 media segments",
            })?,
        )?));
    }

    Ok(FragmentedMp4Output {
        init_segment,
        media_segments,
    })
}

/// Probe stream metadata from concatenated fMP4 bytes.
pub fn probe_fragmented_mp4_bytes(format: ContainerFormat, bytes: &Bytes) -> Result<MediaInfo> {
    let demuxer = FragmentedMp4Demuxer::new(format)?;
    parse_init_tracks(format, bytes).map(|tracks| {
        let mut media = media_from_fmp4_tracks(&tracks);
        media.duration_seconds = parse_fragment_duration_seconds(format, &tracks, bytes).ok();
        let _ = demuxer;
        media
    })
}

/// Demux concatenated fMP4 bytes containing `ftyp`/`moov` followed by `moof`/`mdat` fragments.
pub fn demux_fragmented_mp4_bytes(format: ContainerFormat, bytes: &Bytes) -> Result<DemuxedMedia> {
    let demuxer = FragmentedMp4Demuxer::new(format)?;
    let tracks = parse_init_tracks(format, bytes)?;
    let mut media = media_from_fmp4_tracks(&tracks);
    let mut packets = parse_media_fragments(format, &tracks, bytes)?;
    sort_packets_by_decode_time(&mut packets);
    validate_monotonic_by_track(&packets)?;
    media.duration_seconds = infer_packet_duration_seconds(&packets);
    Ok(DemuxedMedia::new(
        demuxer.container_format(),
        media,
        packets,
    ))
}

/// Demux fMP4 from an initialization segment plus separate media segments.
pub fn demux_fragmented_mp4_segments(
    format: ContainerFormat,
    init_segment: &Bytes,
    media_segments: &[Bytes],
) -> Result<DemuxedMedia> {
    let demuxer = FragmentedMp4Demuxer::new(format)?;
    let tracks = parse_init_tracks(format, init_segment)?;
    let mut media = media_from_fmp4_tracks(&tracks);
    let mut packets = Vec::new();
    for segment in media_segments {
        packets.extend(parse_media_fragments(format, &tracks, segment)?);
    }
    sort_packets_by_decode_time(&mut packets);
    validate_monotonic_by_track(&packets)?;
    media.duration_seconds = infer_packet_duration_seconds(&packets);
    Ok(DemuxedMedia::new(
        demuxer.container_format(),
        media,
        packets,
    ))
}

#[derive(Clone, Debug)]
struct Fmp4Track {
    source_track_id: u32,
    mp4_track_id: u32,
    media_type: MediaType,
    codec: CodecId,
    timescale: u32,
    width: Option<u32>,
    height: Option<u32>,
    sample_rate: Option<u32>,
    channels: Option<u16>,
    codec_config: Bytes,
}

#[derive(Clone, Debug)]
struct Fmp4Sample {
    dts: u64,
    duration: u32,
    data: Bytes,
    is_sync: bool,
    composition_time_offset: i32,
}

#[derive(Clone, Debug)]
struct TrackFragment<'a> {
    track: &'a Fmp4Track,
    samples: Vec<Fmp4Sample>,
    data_offset: u32,
}

#[derive(Clone, Debug)]
struct ParsedBox<'a> {
    typ: [u8; 4],
    start: usize,
    header_size: usize,
    content: &'a [u8],
}

#[derive(Clone, Debug)]
struct TrunSample {
    duration: u32,
    size: u32,
    flags: u32,
    composition_time_offset: i32,
}

#[derive(Clone, Debug)]
struct TrunData {
    data_offset: i64,
    first_sample_flags: Option<u32>,
    samples: Vec<TrunSample>,
}

fn build_tracks(media: &MediaInfo) -> Result<Vec<Fmp4Track>> {
    let mut tracks = Vec::with_capacity(media.streams.len());
    for (index, stream) in media.streams.iter().enumerate() {
        let mp4_track_id = u32::try_from(index + 1).map_err(|_| Error::Unsupported {
            operation: "fmp4 mux",
            reason: "too many fMP4 tracks",
        })?;
        tracks.push(track_from_stream(stream, mp4_track_id)?);
    }
    Ok(tracks)
}

fn track_from_stream(stream: &StreamInfo, mp4_track_id: u32) -> Result<Fmp4Track> {
    match (&stream.media_type, &stream.codec) {
        (MediaType::Video, CodecId::H264 | CodecId::H265) => {
            let config = stream.codec_config.clone().ok_or(Error::Unsupported {
                operation: "fmp4 mux",
                reason: "H.264/H.265 fMP4 muxing requires avcC/hvcC codec config",
            })?;
            let width = stream.width.ok_or(Error::Unsupported {
                operation: "fmp4 mux",
                reason: "fMP4 video tracks need known width and height",
            })?;
            let height = stream.height.ok_or(Error::Unsupported {
                operation: "fmp4 mux",
                reason: "fMP4 video tracks need known width and height",
            })?;
            if width > u32::from(u16::MAX) || height > u32::from(u16::MAX) {
                return Err(Error::Unsupported {
                    operation: "fmp4 mux",
                    reason: "fMP4 video dimensions exceed 16-bit sample-entry fields",
                });
            }
            if stream.codec == CodecId::H264 {
                let _ = parse_avcc_parameter_sets(&config)?;
            } else {
                parse_hvcc_parameter_sets(&config)?;
            }
            Ok(Fmp4Track {
                source_track_id: stream.track_id,
                mp4_track_id,
                media_type: stream.media_type,
                codec: stream.codec.clone(),
                timescale: VIDEO_TIMESCALE,
                width: Some(width),
                height: Some(height),
                sample_rate: None,
                channels: None,
                codec_config: config,
            })
        }
        (MediaType::Audio, CodecId::Aac) => {
            let config = stream.codec_config.clone().ok_or(Error::Unsupported {
                operation: "fmp4 mux",
                reason: "AAC fMP4 muxing requires AudioSpecificConfig",
            })?;
            let sample_rate = stream.sample_rate.ok_or(Error::Unsupported {
                operation: "fmp4 mux",
                reason: "AAC fMP4 muxing requires sample rate metadata",
            })?;
            let channels = stream.channels.ok_or(Error::Unsupported {
                operation: "fmp4 mux",
                reason: "AAC fMP4 muxing requires channel metadata",
            })?;
            if sample_rate > u32::from(u16::MAX) {
                return Err(Error::Unsupported {
                    operation: "fmp4 mux",
                    reason: "AAC sample rates above 65535 Hz need an extended MP4 audio sample entry",
                });
            }
            Ok(Fmp4Track {
                source_track_id: stream.track_id,
                mp4_track_id,
                media_type: stream.media_type,
                codec: stream.codec.clone(),
                timescale: sample_rate,
                width: None,
                height: None,
                sample_rate: Some(sample_rate),
                channels: Some(channels),
                codec_config: config,
            })
        }
        (MediaType::Subtitle, CodecId::WebVtt) => Ok(Fmp4Track {
            source_track_id: stream.track_id,
            mp4_track_id,
            media_type: stream.media_type,
            codec: stream.codec.clone(),
            timescale: stream_timescale(stream)?,
            width: None,
            height: None,
            sample_rate: None,
            channels: None,
            codec_config: stream.codec_config.clone().unwrap_or_default(),
        }),
        (MediaType::Video, _) => Err(Error::Unsupported {
            operation: "fmp4 mux",
            reason: "fMP4 video muxing currently supports H.264 and H.265",
        }),
        (MediaType::Audio, _) => Err(Error::Unsupported {
            operation: "fmp4 mux",
            reason: "fMP4 audio muxing currently supports AAC",
        }),
        (MediaType::Subtitle, _) => Err(Error::Unsupported {
            operation: "fmp4 mux",
            reason: "fMP4 subtitle muxing currently supports WebVTT",
        }),
        _ => Err(Error::Unsupported {
            operation: "fmp4 mux",
            reason: "fMP4 muxing currently supports audio, video, and WebVTT subtitle tracks only",
        }),
    }
}

fn stream_timescale(stream: &StreamInfo) -> Result<u32> {
    if stream.time_base.num == 1 {
        return u32::try_from(stream.time_base.den).map_err(|_| Error::InvalidTimeBase {
            num: stream.time_base.num,
            den: stream.time_base.den,
        });
    }
    if stream.time_base.den % stream.time_base.num == 0 {
        return u32::try_from(stream.time_base.den / stream.time_base.num).map_err(|_| {
            Error::InvalidTimeBase {
                num: stream.time_base.num,
                den: stream.time_base.den,
            }
        });
    }
    Ok(MOVIE_TIMESCALE)
}

fn validate_segment_packets(
    tracks: &[Fmp4Track],
    segment_packets: &[Vec<EncodedPacket>],
) -> Result<()> {
    let by_source_track = tracks
        .iter()
        .map(|track| (track.source_track_id, track))
        .collect::<BTreeMap<_, _>>();
    let mut covered_tracks = BTreeSet::new();

    for packets in segment_packets {
        if packets.is_empty() {
            return Err(Error::EmptyInput);
        }
        validate_monotonic_by_track(packets)?;
        for packet in packets {
            let track = by_source_track
                .get(&packet.track_id)
                .ok_or(Error::IncompatibleTrack {
                    track_id: packet.track_id,
                    reason: "packet references a stream missing from MediaInfo",
                })?;
            if packet.codec != track.codec {
                return Err(Error::CodecMismatch {
                    expected: track.codec.clone(),
                    actual: packet.codec.clone(),
                });
            }
            covered_tracks.insert(packet.track_id);
        }
    }

    for track in tracks {
        if !covered_tracks.contains(&track.source_track_id) {
            return Err(Error::IncompatibleTrack {
                track_id: track.source_track_id,
                reason: "declared fMP4 track has no packets",
            });
        }
    }

    Ok(())
}

fn build_init_segment(tracks: &[Fmp4Track]) -> Result<Vec<u8>> {
    let mut out = build_ftyp()?;
    out.extend_from_slice(&build_moov(tracks)?);
    Ok(out)
}

fn build_media_segment(
    tracks: &[Fmp4Track],
    packets: &[EncodedPacket],
    sequence_number: u32,
) -> Result<Vec<u8>> {
    let mut fragments = Vec::new();
    for track in tracks {
        let mut track_packets = packets
            .iter()
            .filter(|packet| packet.track_id == track.source_track_id)
            .collect::<Vec<_>>();
        track_packets.sort_by_key(|packet| packet.decode_order_ts());
        if track_packets.is_empty() {
            continue;
        }

        let samples = track_packets
            .into_iter()
            .map(|packet| sample_from_packet(track, packet))
            .collect::<Result<Vec<_>>>()?;
        fragments.push(TrackFragment {
            track,
            samples,
            data_offset: 0,
        });
    }

    if fragments.is_empty() {
        return Err(Error::EmptyInput);
    }

    let placeholder_moof = build_moof(sequence_number, &fragments)?;
    let mut next_data_offset = checked_u32(placeholder_moof.len() + 8, "fMP4 moof size")?;
    for fragment in &mut fragments {
        fragment.data_offset = next_data_offset;
        next_data_offset = next_data_offset
            .checked_add(fragment_data_len(fragment)?)
            .ok_or(Error::Unsupported {
                operation: "fmp4 mux",
                reason: "fMP4 media segment is too large",
            })?;
    }

    let moof = build_moof(sequence_number, &fragments)?;
    let mut mdat_payload = Vec::new();
    for fragment in &fragments {
        for sample in &fragment.samples {
            mdat_payload.extend_from_slice(&sample.data);
        }
    }
    let mdat = build_box(b"mdat", &mdat_payload)?;

    let mut out = Vec::with_capacity(moof.len() + mdat.len());
    out.extend_from_slice(&moof);
    out.extend_from_slice(&mdat);
    Ok(out)
}

fn fragment_data_len(fragment: &TrackFragment<'_>) -> Result<u32> {
    let len = fragment
        .samples
        .iter()
        .try_fold(0usize, |total, sample| total.checked_add(sample.data.len()))
        .ok_or(Error::Unsupported {
            operation: "fmp4 mux",
            reason: "fMP4 media segment is too large",
        })?;
    checked_u32(len, "fMP4 sample data")
}

fn sample_from_packet(track: &Fmp4Track, packet: &EncodedPacket) -> Result<Fmp4Sample> {
    let target_time_base = TimeBase::new(
        1,
        i32::try_from(track.timescale).map_err(|_| Error::InvalidTimeBase {
            num: 1,
            den: i32::MAX,
        })?,
    )?;
    let pts = timestamp_to_track_ticks(packet.time_base, packet.pts, target_time_base)?;
    let dts = timestamp_to_track_ticks(
        packet.time_base,
        packet.dts.unwrap_or(packet.pts),
        target_time_base,
    )?;
    let composition_time_offset =
        i32::try_from(i128::from(pts) - i128::from(dts)).map_err(|_| {
            Error::InvalidPacketTiming {
                reason: "fMP4 composition time offset exceeds signed 32-bit trun field",
            }
        })?;

    Ok(Fmp4Sample {
        dts,
        duration: duration_to_track_ticks(packet.time_base, packet.duration, target_time_base)?,
        data: sample_payload(track, packet)?,
        is_sync: track.media_type != MediaType::Video || packet.is_keyframe,
        composition_time_offset,
    })
}

fn sample_payload(track: &Fmp4Track, packet: &EncodedPacket) -> Result<Bytes> {
    match track.codec {
        CodecId::H264 => h264_packet_to_length_prefixed(packet, &track.codec_config),
        CodecId::H265 => h265_packet_to_length_prefixed(packet, &track.codec_config),
        CodecId::Aac => aac_packet_to_raw(packet),
        CodecId::WebVtt => build_webvtt_sample(&packet.data),
        _ => Err(Error::Unsupported {
            operation: "fmp4 mux",
            reason: "track codec is not supported by the fMP4 writer",
        }),
    }
}

fn build_webvtt_sample(payload: &[u8]) -> Result<Bytes> {
    let payl = build_box(b"payl", payload)?;
    let vttc = build_box(b"vttc", &payl)?;
    Ok(Bytes::from(vttc))
}

fn timestamp_to_track_ticks(source: TimeBase, ticks: i64, target: TimeBase) -> Result<u64> {
    let scaled = source.rescale(ticks, target);
    u64::try_from(scaled).map_err(|_| Error::InvalidPacketTiming {
        reason: "fMP4 timestamps cannot be negative",
    })
}

fn duration_to_track_ticks(source: TimeBase, ticks: i64, target: TimeBase) -> Result<u32> {
    let scaled = source.rescale(ticks, target);
    if scaled <= 0 {
        return Err(Error::InvalidPacketTiming {
            reason: "fMP4 sample durations must be positive",
        });
    }
    u32::try_from(scaled).map_err(|_| Error::InvalidPacketTiming {
        reason: "fMP4 sample duration exceeds 32-bit trun field",
    })
}

fn build_ftyp() -> Result<Vec<u8>> {
    let mut payload = Vec::new();
    payload.extend_from_slice(b"iso6");
    push_u32(&mut payload, 1);
    payload.extend_from_slice(b"iso6");
    payload.extend_from_slice(b"mp41");
    payload.extend_from_slice(b"cmfc");
    payload.extend_from_slice(b"dash");
    build_box(b"ftyp", &payload)
}

fn build_moov(tracks: &[Fmp4Track]) -> Result<Vec<u8>> {
    let mut payload = Vec::new();
    payload.extend_from_slice(&build_mvhd(tracks)?);
    for track in tracks {
        payload.extend_from_slice(&build_trak(track)?);
    }
    payload.extend_from_slice(&build_mvex(tracks)?);
    build_box(b"moov", &payload)
}

fn build_mvhd(tracks: &[Fmp4Track]) -> Result<Vec<u8>> {
    let mut payload = Vec::new();
    push_u32(&mut payload, 0);
    push_u32(&mut payload, 0);
    push_u32(&mut payload, 0);
    push_u32(&mut payload, MOVIE_TIMESCALE);
    push_u32(&mut payload, 0);
    push_u32(&mut payload, 0x0001_0000);
    push_u16(&mut payload, 0x0100);
    push_u16(&mut payload, 0);
    payload.extend_from_slice(&[0; 8]);
    push_matrix(&mut payload);
    payload.extend_from_slice(&[0; 24]);
    push_u32(
        &mut payload,
        u32::try_from(tracks.len() + 1).map_err(|_| Error::Unsupported {
            operation: "fmp4 mux",
            reason: "too many fMP4 tracks",
        })?,
    );
    build_full_box(b"mvhd", 0, 0, &payload)
}

fn build_trak(track: &Fmp4Track) -> Result<Vec<u8>> {
    let mut payload = Vec::new();
    payload.extend_from_slice(&build_tkhd(track)?);
    payload.extend_from_slice(&build_mdia(track)?);
    build_box(b"trak", &payload)
}

fn build_tkhd(track: &Fmp4Track) -> Result<Vec<u8>> {
    let mut payload = Vec::new();
    push_u32(&mut payload, 0);
    push_u32(&mut payload, 0);
    push_u32(&mut payload, track.mp4_track_id);
    push_u32(&mut payload, 0);
    push_u32(&mut payload, 0);
    payload.extend_from_slice(&[0; 8]);
    push_u16(&mut payload, 0);
    push_u16(&mut payload, 0);
    push_u16(
        &mut payload,
        if track.media_type == MediaType::Audio {
            0x0100
        } else {
            0
        },
    );
    push_u16(&mut payload, 0);
    push_matrix(&mut payload);
    push_u32(&mut payload, track.width.unwrap_or(0) << 16);
    push_u32(&mut payload, track.height.unwrap_or(0) << 16);
    build_full_box(b"tkhd", 0, 0x000003, &payload)
}

fn build_mdia(track: &Fmp4Track) -> Result<Vec<u8>> {
    let mut payload = Vec::new();
    payload.extend_from_slice(&build_mdhd(track)?);
    payload.extend_from_slice(&build_hdlr(track)?);
    payload.extend_from_slice(&build_minf(track)?);
    build_box(b"mdia", &payload)
}

fn build_mdhd(track: &Fmp4Track) -> Result<Vec<u8>> {
    let mut payload = Vec::new();
    push_u32(&mut payload, 0);
    push_u32(&mut payload, 0);
    push_u32(&mut payload, track.timescale);
    push_u32(&mut payload, 0);
    push_u16(&mut payload, language_code("und"));
    push_u16(&mut payload, 0);
    build_full_box(b"mdhd", 0, 0, &payload)
}

fn build_hdlr(track: &Fmp4Track) -> Result<Vec<u8>> {
    let mut payload = Vec::new();
    push_u32(&mut payload, 0);
    match track.media_type {
        MediaType::Video => payload.extend_from_slice(b"vide"),
        MediaType::Audio => payload.extend_from_slice(b"soun"),
        MediaType::Subtitle => payload.extend_from_slice(b"text"),
        _ => unreachable!("track validation rejects unsupported tracks"),
    }
    payload.extend_from_slice(&[0; 12]);
    payload.extend_from_slice(match track.media_type {
        MediaType::Video => b"VideoHandler\0".as_slice(),
        MediaType::Audio => b"SoundHandler\0".as_slice(),
        MediaType::Subtitle => b"TextHandler\0".as_slice(),
        _ => unreachable!("track validation rejects unsupported tracks"),
    });
    build_full_box(b"hdlr", 0, 0, &payload)
}

fn build_minf(track: &Fmp4Track) -> Result<Vec<u8>> {
    let mut payload = Vec::new();
    match track.media_type {
        MediaType::Video => payload.extend_from_slice(&build_vmhd()?),
        MediaType::Audio => payload.extend_from_slice(&build_smhd()?),
        MediaType::Subtitle => payload.extend_from_slice(&build_nmhd()?),
        _ => unreachable!("track validation rejects unsupported tracks"),
    }
    payload.extend_from_slice(&build_dinf()?);
    payload.extend_from_slice(&build_stbl(track)?);
    build_box(b"minf", &payload)
}

fn build_vmhd() -> Result<Vec<u8>> {
    let mut payload = Vec::new();
    push_u16(&mut payload, 0);
    push_u16(&mut payload, 0);
    push_u16(&mut payload, 0);
    push_u16(&mut payload, 0);
    build_full_box(b"vmhd", 0, 1, &payload)
}

fn build_smhd() -> Result<Vec<u8>> {
    let mut payload = Vec::new();
    push_u16(&mut payload, 0);
    push_u16(&mut payload, 0);
    build_full_box(b"smhd", 0, 0, &payload)
}

fn build_nmhd() -> Result<Vec<u8>> {
    build_full_box(b"nmhd", 0, 0, &[])
}

fn build_dinf() -> Result<Vec<u8>> {
    let url = build_full_box(b"url ", 0, 1, &[])?;
    let mut dref_payload = Vec::new();
    push_u32(&mut dref_payload, 1);
    dref_payload.extend_from_slice(&url);
    let dref = build_full_box(b"dref", 0, 0, &dref_payload)?;
    build_box(b"dinf", &dref)
}

fn build_stbl(track: &Fmp4Track) -> Result<Vec<u8>> {
    let mut payload = Vec::new();
    payload.extend_from_slice(&build_stsd(track)?);
    payload.extend_from_slice(&build_empty_sample_table_boxes()?);
    build_box(b"stbl", &payload)
}

fn build_stsd(track: &Fmp4Track) -> Result<Vec<u8>> {
    let sample_entry = match track.media_type {
        MediaType::Video => build_visual_sample_entry(track)?,
        MediaType::Audio => build_audio_sample_entry(track)?,
        MediaType::Subtitle => build_webvtt_sample_entry(track)?,
        _ => unreachable!("track validation rejects unsupported tracks"),
    };
    let mut payload = Vec::new();
    push_u32(&mut payload, 1);
    payload.extend_from_slice(&sample_entry);
    build_full_box(b"stsd", 0, 0, &payload)
}

fn build_empty_sample_table_boxes() -> Result<Vec<u8>> {
    let mut payload = Vec::new();
    let mut stts = Vec::new();
    push_u32(&mut stts, 0);
    payload.extend_from_slice(&build_full_box(b"stts", 0, 0, &stts)?);

    let mut stsc = Vec::new();
    push_u32(&mut stsc, 0);
    payload.extend_from_slice(&build_full_box(b"stsc", 0, 0, &stsc)?);

    let mut stsz = Vec::new();
    push_u32(&mut stsz, 0);
    push_u32(&mut stsz, 0);
    payload.extend_from_slice(&build_full_box(b"stsz", 0, 0, &stsz)?);

    let mut stco = Vec::new();
    push_u32(&mut stco, 0);
    payload.extend_from_slice(&build_full_box(b"stco", 0, 0, &stco)?);
    Ok(payload)
}

fn build_visual_sample_entry(track: &Fmp4Track) -> Result<Vec<u8>> {
    let mut payload = Vec::new();
    payload.extend_from_slice(&[0; 6]);
    push_u16(&mut payload, 1);
    push_u16(&mut payload, 0);
    push_u16(&mut payload, 0);
    push_u32(&mut payload, 0);
    push_u32(&mut payload, 0);
    push_u32(&mut payload, 0);
    push_u16(&mut payload, track.width.unwrap_or(0) as u16);
    push_u16(&mut payload, track.height.unwrap_or(0) as u16);
    push_u32(&mut payload, 0x0048_0000);
    push_u32(&mut payload, 0x0048_0000);
    push_u32(&mut payload, 0);
    push_u16(&mut payload, 1);
    payload.extend_from_slice(&[0; 32]);
    push_u16(&mut payload, 0x0018);
    push_u16(&mut payload, 0xffff);
    match track.codec {
        CodecId::H264 => payload.extend_from_slice(&build_box(b"avcC", &track.codec_config)?),
        CodecId::H265 => payload.extend_from_slice(&build_box(b"hvcC", &track.codec_config)?),
        _ => unreachable!("track validation rejects unsupported video codecs"),
    }
    build_box(
        match track.codec {
            CodecId::H264 => b"avc1",
            CodecId::H265 => b"hvc1",
            _ => unreachable!("track validation rejects unsupported video codecs"),
        },
        &payload,
    )
}

fn build_audio_sample_entry(track: &Fmp4Track) -> Result<Vec<u8>> {
    let mut payload = Vec::new();
    payload.extend_from_slice(&[0; 6]);
    push_u16(&mut payload, 1);
    push_u32(&mut payload, 0);
    push_u32(&mut payload, 0);
    push_u16(&mut payload, track.channels.unwrap_or(2));
    push_u16(&mut payload, 16);
    push_u16(&mut payload, 0);
    push_u16(&mut payload, 0);
    push_u32(&mut payload, track.sample_rate.unwrap_or(48_000) << 16);
    payload.extend_from_slice(&build_esds(&track.codec_config)?);
    build_box(b"mp4a", &payload)
}

fn build_webvtt_sample_entry(track: &Fmp4Track) -> Result<Vec<u8>> {
    let mut payload = Vec::new();
    payload.extend_from_slice(&[0; 6]);
    push_u16(&mut payload, 1);
    let config = if track.codec_config.is_empty() {
        Bytes::from_static(b"WEBVTT")
    } else {
        track.codec_config.clone()
    };
    payload.extend_from_slice(&build_box(b"vttC", &config)?);
    build_box(b"wvtt", &payload)
}

fn build_esds(audio_specific_config: &[u8]) -> Result<Vec<u8>> {
    let decoder_specific = descriptor(0x05, audio_specific_config)?;
    let mut decoder_config = Vec::new();
    decoder_config.push(0x40);
    decoder_config.push(0x15);
    push_u24(&mut decoder_config, 0);
    push_u32(&mut decoder_config, 0);
    push_u32(&mut decoder_config, 0);
    decoder_config.extend_from_slice(&decoder_specific);
    let decoder_config = descriptor(0x04, &decoder_config)?;
    let sl_config = descriptor(0x06, &[0x02])?;

    let mut es = Vec::new();
    push_u16(&mut es, 1);
    es.push(0);
    es.extend_from_slice(&decoder_config);
    es.extend_from_slice(&sl_config);
    let es = descriptor(0x03, &es)?;
    build_full_box(b"esds", 0, 0, &es)
}

fn descriptor(tag: u8, payload: &[u8]) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    out.push(tag);
    push_descriptor_size(&mut out, payload.len())?;
    out.extend_from_slice(payload);
    Ok(out)
}

fn push_descriptor_size(out: &mut Vec<u8>, len: usize) -> Result<()> {
    if len > 0x0fff_ffff {
        return Err(Error::Unsupported {
            operation: "fmp4 mux",
            reason: "MPEG-4 descriptor is too large",
        });
    }
    let mut bytes = [
        ((len >> 21) & 0x7f) as u8,
        ((len >> 14) & 0x7f) as u8,
        ((len >> 7) & 0x7f) as u8,
        (len & 0x7f) as u8,
    ];
    let first = bytes.iter().position(|byte| *byte != 0).unwrap_or(3);
    for byte in &mut bytes[first..3] {
        *byte |= 0x80;
    }
    out.extend_from_slice(&bytes[first..]);
    Ok(())
}

fn build_mvex(tracks: &[Fmp4Track]) -> Result<Vec<u8>> {
    let mut payload = Vec::new();
    for track in tracks {
        payload.extend_from_slice(&build_trex(track)?);
    }
    build_box(b"mvex", &payload)
}

fn build_trex(track: &Fmp4Track) -> Result<Vec<u8>> {
    let mut payload = Vec::new();
    push_u32(&mut payload, track.mp4_track_id);
    push_u32(&mut payload, 1);
    push_u32(&mut payload, 0);
    push_u32(&mut payload, 0);
    push_u32(&mut payload, 0);
    build_full_box(b"trex", 0, 0, &payload)
}

fn build_moof(sequence_number: u32, fragments: &[TrackFragment<'_>]) -> Result<Vec<u8>> {
    let mut payload = Vec::new();
    payload.extend_from_slice(&build_mfhd(sequence_number)?);
    for fragment in fragments {
        payload.extend_from_slice(&build_traf(fragment)?);
    }
    build_box(b"moof", &payload)
}

fn build_mfhd(sequence_number: u32) -> Result<Vec<u8>> {
    let mut payload = Vec::new();
    push_u32(&mut payload, sequence_number);
    build_full_box(b"mfhd", 0, 0, &payload)
}

fn build_traf(fragment: &TrackFragment<'_>) -> Result<Vec<u8>> {
    let mut payload = Vec::new();
    payload.extend_from_slice(&build_tfhd(fragment.track)?);
    payload.extend_from_slice(&build_tfdt(fragment)?);
    payload.extend_from_slice(&build_trun(fragment)?);
    build_box(b"traf", &payload)
}

fn build_tfhd(track: &Fmp4Track) -> Result<Vec<u8>> {
    let mut payload = Vec::new();
    push_u32(&mut payload, track.mp4_track_id);
    build_full_box(b"tfhd", 0, 0x020000, &payload)
}

fn build_tfdt(fragment: &TrackFragment<'_>) -> Result<Vec<u8>> {
    let mut payload = Vec::new();
    push_u64(&mut payload, fragment.samples[0].dts);
    build_full_box(b"tfdt", 1, 0, &payload)
}

fn build_trun(fragment: &TrackFragment<'_>) -> Result<Vec<u8>> {
    let mut payload = Vec::new();
    push_u32(
        &mut payload,
        u32::try_from(fragment.samples.len()).map_err(|_| Error::Unsupported {
            operation: "fmp4 mux",
            reason: "too many samples in fMP4 track fragment",
        })?,
    );
    push_u32(&mut payload, fragment.data_offset);
    for sample in &fragment.samples {
        push_u32(&mut payload, sample.duration);
        push_u32(
            &mut payload,
            checked_u32(sample.data.len(), "fMP4 sample size")?,
        );
        push_u32(&mut payload, sample_flags(fragment.track, sample));
        payload.extend_from_slice(&sample.composition_time_offset.to_be_bytes());
    }
    build_full_box(b"trun", 1, 0x000f01, &payload)
}

fn sample_flags(track: &Fmp4Track, sample: &Fmp4Sample) -> u32 {
    if track.media_type != MediaType::Video || sample.is_sync {
        0x0200_0000
    } else {
        0x0101_0000
    }
}

fn build_box(typ: &[u8; 4], payload: &[u8]) -> Result<Vec<u8>> {
    let size = checked_u32(payload.len() + 8, "MP4 box size")?;
    let mut out = Vec::with_capacity(size as usize);
    push_u32(&mut out, size);
    out.extend_from_slice(typ);
    out.extend_from_slice(payload);
    Ok(out)
}

fn build_full_box(typ: &[u8; 4], version: u8, flags: u32, payload: &[u8]) -> Result<Vec<u8>> {
    let mut full_payload = Vec::with_capacity(payload.len() + 4);
    full_payload.push(version);
    full_payload.push(((flags >> 16) & 0xff) as u8);
    full_payload.push(((flags >> 8) & 0xff) as u8);
    full_payload.push((flags & 0xff) as u8);
    full_payload.extend_from_slice(payload);
    build_box(typ, &full_payload)
}

fn checked_u32(value: usize, what: &'static str) -> Result<u32> {
    u32::try_from(value).map_err(|_| Error::Unsupported {
        operation: "fmp4 mux",
        reason: what,
    })
}

fn push_u16(out: &mut Vec<u8>, value: u16) {
    out.extend_from_slice(&value.to_be_bytes());
}

fn push_u24(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&[
        ((value >> 16) & 0xff) as u8,
        ((value >> 8) & 0xff) as u8,
        (value & 0xff) as u8,
    ]);
}

fn push_u32(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_be_bytes());
}

fn push_u64(out: &mut Vec<u8>, value: u64) {
    out.extend_from_slice(&value.to_be_bytes());
}

fn push_matrix(out: &mut Vec<u8>) {
    push_u32(out, 0x0001_0000);
    push_u32(out, 0);
    push_u32(out, 0);
    push_u32(out, 0);
    push_u32(out, 0x0001_0000);
    push_u32(out, 0);
    push_u32(out, 0);
    push_u32(out, 0);
    push_u32(out, 0x4000_0000);
}

fn language_code(language: &str) -> u16 {
    let bytes = language.as_bytes();
    if bytes.len() != 3 || !bytes.iter().all(|byte| byte.is_ascii_lowercase()) {
        return 0x55c4;
    }
    (u16::from(bytes[0] - 0x60) << 10)
        | (u16::from(bytes[1] - 0x60) << 5)
        | u16::from(bytes[2] - 0x60)
}

fn parse_avcc_parameter_sets(config: &[u8]) -> Result<(Bytes, Bytes)> {
    if config.len() < 7 {
        return Err(Error::Parse {
            format: "h264",
            message: "AVC decoder config is too short for SPS/PPS data".to_owned(),
        });
    }

    let mut offset = 5usize;
    let sps_count = config[offset] & 0x1f;
    offset += 1;
    let mut sps = None;
    for _ in 0..sps_count {
        let nal = read_config_nal(config, &mut offset, "h264")?;
        sps.get_or_insert(nal);
    }

    if offset >= config.len() {
        return Err(Error::Parse {
            format: "h264",
            message: "AVC decoder config is missing PPS count".to_owned(),
        });
    }
    let pps_count = config[offset];
    offset += 1;
    let mut pps = None;
    for _ in 0..pps_count {
        let nal = read_config_nal(config, &mut offset, "h264")?;
        pps.get_or_insert(nal);
    }

    let sps = sps.ok_or(Error::Parse {
        format: "h264",
        message: "AVC decoder config has no SPS".to_owned(),
    })?;
    let pps = pps.ok_or(Error::Parse {
        format: "h264",
        message: "AVC decoder config has no PPS".to_owned(),
    })?;
    Ok((sps, pps))
}

fn parse_hvcc_parameter_sets(config: &[u8]) -> Result<()> {
    if config.len() < 23 {
        return Err(Error::Parse {
            format: "h265",
            message: "HEVC decoder config is too short for parameter sets".to_owned(),
        });
    }

    let mut offset = 22usize;
    let array_count = config[offset];
    offset += 1;
    let mut vps = None;
    let mut sps = None;
    let mut pps = None;

    for _ in 0..array_count {
        if config.len().saturating_sub(offset) < 3 {
            return Err(Error::Parse {
                format: "h265",
                message: "HEVC decoder config ended inside a parameter-set array".to_owned(),
            });
        }
        let nal_type = config[offset] & 0x3f;
        let nal_count = u16::from_be_bytes([config[offset + 1], config[offset + 2]]);
        offset += 3;

        for _ in 0..nal_count {
            let nal = read_config_nal(config, &mut offset, "h265")?;
            match nal_type {
                32 => {
                    vps.get_or_insert(nal);
                }
                33 => {
                    sps.get_or_insert(nal);
                }
                34 => {
                    pps.get_or_insert(nal);
                }
                _ => {}
            }
        }
    }

    if vps.is_none() {
        return Err(Error::Parse {
            format: "h265",
            message: "HEVC decoder config has no VPS".to_owned(),
        });
    }
    if sps.is_none() {
        return Err(Error::Parse {
            format: "h265",
            message: "HEVC decoder config has no SPS".to_owned(),
        });
    }
    if pps.is_none() {
        return Err(Error::Parse {
            format: "h265",
            message: "HEVC decoder config has no PPS".to_owned(),
        });
    }
    Ok(())
}

fn read_config_nal(config: &[u8], offset: &mut usize, format: &'static str) -> Result<Bytes> {
    if config.len().saturating_sub(*offset) < 2 {
        return Err(Error::Parse {
            format,
            message: "codec config ended before NAL length".to_owned(),
        });
    }
    let len = u16::from_be_bytes([config[*offset], config[*offset + 1]]) as usize;
    *offset += 2;
    if config.len().saturating_sub(*offset) < len {
        return Err(Error::Parse {
            format,
            message: "codec config NAL length exceeds config bytes".to_owned(),
        });
    }
    let nal = Bytes::copy_from_slice(&config[*offset..*offset + len]);
    *offset += len;
    Ok(nal)
}

fn parse_init_tracks(format: ContainerFormat, bytes: &Bytes) -> Result<Vec<Fmp4Track>> {
    let top_level = read_boxes(bytes, 0, format)?;
    let moov = find_box(&top_level, b"moov").ok_or(Error::Parse {
        format: format.as_str(),
        message: "fragmented MP4 init segment is missing moov".to_owned(),
    })?;

    let mut tracks = Vec::new();
    for trak in read_boxes(moov.content, moov.content_start(), format)?
        .into_iter()
        .filter(|box_| box_.typ == *b"trak")
    {
        tracks.push(parse_trak(format, &trak)?);
    }

    if tracks.is_empty() {
        return Err(Error::Parse {
            format: format.as_str(),
            message: "fragmented MP4 init segment has no tracks".to_owned(),
        });
    }

    Ok(tracks)
}

fn parse_trak(format: ContainerFormat, trak: &ParsedBox<'_>) -> Result<Fmp4Track> {
    let trak_children = read_boxes(trak.content, trak.content_start(), format)?;
    let tkhd = find_box(&trak_children, b"tkhd").ok_or(Error::Parse {
        format: format.as_str(),
        message: "trak is missing tkhd".to_owned(),
    })?;
    let (track_id, tkhd_width, tkhd_height) = parse_tkhd(format, tkhd.content)?;

    let mdia = find_box(&trak_children, b"mdia").ok_or(Error::Parse {
        format: format.as_str(),
        message: "trak is missing mdia".to_owned(),
    })?;
    let mdia_children = read_boxes(mdia.content, mdia.content_start(), format)?;
    let mdhd = find_box(&mdia_children, b"mdhd").ok_or(Error::Parse {
        format: format.as_str(),
        message: "mdia is missing mdhd".to_owned(),
    })?;
    let (timescale, _duration) = parse_mdhd(format, mdhd.content)?;

    let hdlr = find_box(&mdia_children, b"hdlr").ok_or(Error::Parse {
        format: format.as_str(),
        message: "mdia is missing hdlr".to_owned(),
    })?;
    let handler = parse_hdlr(format, hdlr.content)?;

    let minf = find_box(&mdia_children, b"minf").ok_or(Error::Parse {
        format: format.as_str(),
        message: "mdia is missing minf".to_owned(),
    })?;
    let minf_children = read_boxes(minf.content, minf.content_start(), format)?;
    let stbl = find_box(&minf_children, b"stbl").ok_or(Error::Parse {
        format: format.as_str(),
        message: "minf is missing stbl".to_owned(),
    })?;
    let stbl_children = read_boxes(stbl.content, stbl.content_start(), format)?;
    let stsd = find_box(&stbl_children, b"stsd").ok_or(Error::Parse {
        format: format.as_str(),
        message: "stbl is missing stsd".to_owned(),
    })?;
    let mut track = parse_stsd(format, stsd.content, track_id, timescale, handler)?;
    if track.media_type == MediaType::Video {
        if track.width.is_none() && tkhd_width > 0 {
            track.width = Some(tkhd_width);
        }
        if track.height.is_none() && tkhd_height > 0 {
            track.height = Some(tkhd_height);
        }
    }
    Ok(track)
}

fn parse_tkhd(format: ContainerFormat, content: &[u8]) -> Result<(u32, u32, u32)> {
    let (version, _, payload) = parse_full_box(format, "tkhd", content)?;
    let (track_offset, width_offset) = match version {
        0 => (8, 72),
        1 => (16, 84),
        _ => {
            return Err(Error::Parse {
                format: format.as_str(),
                message: "unsupported tkhd version".to_owned(),
            });
        }
    };
    if payload.len().saturating_sub(track_offset) < 4
        || payload.len().saturating_sub(width_offset) < 8
    {
        return Err(Error::Parse {
            format: format.as_str(),
            message: "tkhd box is truncated".to_owned(),
        });
    }

    let track_id = read_u32_at(payload, track_offset);
    let width = read_u32_at(payload, width_offset) >> 16;
    let height = read_u32_at(payload, width_offset + 4) >> 16;
    Ok((track_id, width, height))
}

fn parse_mdhd(format: ContainerFormat, content: &[u8]) -> Result<(u32, u64)> {
    let (version, _, payload) = parse_full_box(format, "mdhd", content)?;
    match version {
        0 => {
            if payload.len() < 16 {
                return Err(Error::Parse {
                    format: format.as_str(),
                    message: "mdhd box is truncated".to_owned(),
                });
            }
            Ok((read_u32_at(payload, 8), u64::from(read_u32_at(payload, 12))))
        }
        1 => {
            if payload.len() < 28 {
                return Err(Error::Parse {
                    format: format.as_str(),
                    message: "mdhd version 1 box is truncated".to_owned(),
                });
            }
            Ok((read_u32_at(payload, 16), read_u64_at(payload, 20)))
        }
        _ => Err(Error::Parse {
            format: format.as_str(),
            message: "unsupported mdhd version".to_owned(),
        }),
    }
}

fn parse_hdlr(format: ContainerFormat, content: &[u8]) -> Result<[u8; 4]> {
    let (_, _, payload) = parse_full_box(format, "hdlr", content)?;
    if payload.len() < 8 {
        return Err(Error::Parse {
            format: format.as_str(),
            message: "hdlr box is truncated".to_owned(),
        });
    }
    Ok(payload[4..8].try_into().unwrap())
}

fn parse_stsd(
    format: ContainerFormat,
    content: &[u8],
    track_id: u32,
    timescale: u32,
    handler: [u8; 4],
) -> Result<Fmp4Track> {
    let (_, _, payload) = parse_full_box(format, "stsd", content)?;
    if payload.len() < 8 {
        return Err(Error::Parse {
            format: format.as_str(),
            message: "stsd box is truncated".to_owned(),
        });
    }
    let entry_count = read_u32_at(payload, 0);
    if entry_count == 0 {
        return Err(Error::Parse {
            format: format.as_str(),
            message: "stsd has no sample entries".to_owned(),
        });
    }

    let entries = read_boxes(&payload[4..], 4, format)?;
    let entry = entries.first().ok_or(Error::Parse {
        format: format.as_str(),
        message: "stsd sample entry is missing".to_owned(),
    })?;

    match &entry.typ {
        b"avc1" | b"avc3" => {
            parse_visual_entry(format, entry, track_id, CodecId::H264, timescale, b"avcC")
        }
        b"hvc1" | b"hev1" => {
            parse_visual_entry(format, entry, track_id, CodecId::H265, timescale, b"hvcC")
        }
        b"mp4a" => parse_mp4a_entry(format, entry, track_id, timescale),
        b"wvtt" => Ok(Fmp4Track {
            source_track_id: track_id,
            mp4_track_id: track_id,
            media_type: MediaType::Subtitle,
            codec: CodecId::WebVtt,
            timescale,
            width: None,
            height: None,
            sample_rate: None,
            channels: None,
            codec_config: Bytes::new(),
        }),
        _ => Err(Error::Unsupported {
            operation: "fmp4 demux",
            reason: if handler == *b"vide" {
                "unsupported fMP4 video sample entry"
            } else if handler == *b"soun" {
                "unsupported fMP4 audio sample entry"
            } else if handler == *b"text" || handler == *b"sbtl" || handler == *b"subt" {
                "unsupported fMP4 subtitle sample entry"
            } else {
                "unsupported fMP4 sample entry"
            },
        }),
    }
}

fn parse_visual_entry(
    format: ContainerFormat,
    entry: &ParsedBox<'_>,
    track_id: u32,
    codec: CodecId,
    timescale: u32,
    config_box: &[u8; 4],
) -> Result<Fmp4Track> {
    if entry.content.len() < 78 {
        return Err(Error::Parse {
            format: format.as_str(),
            message: "visual sample entry is truncated".to_owned(),
        });
    }
    let width = u32::from(read_u16_at(entry.content, 24));
    let height = u32::from(read_u16_at(entry.content, 26));
    let children = read_boxes(&entry.content[78..], entry.content_start() + 78, format)?;
    let config = find_box(&children, config_box).ok_or(Error::Parse {
        format: format.as_str(),
        message: "visual sample entry is missing codec config".to_owned(),
    })?;
    Ok(Fmp4Track {
        source_track_id: track_id,
        mp4_track_id: track_id,
        media_type: MediaType::Video,
        codec,
        timescale,
        width: Some(width),
        height: Some(height),
        sample_rate: None,
        channels: None,
        codec_config: Bytes::copy_from_slice(config.content),
    })
}

fn parse_mp4a_entry(
    format: ContainerFormat,
    entry: &ParsedBox<'_>,
    track_id: u32,
    timescale: u32,
) -> Result<Fmp4Track> {
    if entry.content.len() < 28 {
        return Err(Error::Parse {
            format: format.as_str(),
            message: "mp4a sample entry is truncated".to_owned(),
        });
    }
    let channels = read_u16_at(entry.content, 16);
    let sample_rate = read_u32_at(entry.content, 24) >> 16;
    let children = read_boxes(&entry.content[28..], entry.content_start() + 28, format)?;
    let esds = find_box(&children, b"esds").ok_or(Error::Parse {
        format: format.as_str(),
        message: "mp4a sample entry is missing esds".to_owned(),
    })?;
    let config = extract_esds_decoder_specific(format, esds.content)?;
    Ok(Fmp4Track {
        source_track_id: track_id,
        mp4_track_id: track_id,
        media_type: MediaType::Audio,
        codec: CodecId::Aac,
        timescale,
        width: None,
        height: None,
        sample_rate: Some(sample_rate),
        channels: Some(channels),
        codec_config: config,
    })
}

fn media_from_fmp4_tracks(tracks: &[Fmp4Track]) -> MediaInfo {
    let mut media = MediaInfo::default();
    for track in tracks {
        media.push_stream(stream_from_fmp4_track(track));
    }
    media
}

fn stream_from_fmp4_track(track: &Fmp4Track) -> StreamInfo {
    let time_base = TimeBase::new(1, track.timescale as i32).unwrap_or_else(|_| {
        TimeBase::new(1, i32::MAX).expect("hard-coded positive denominator is valid")
    });
    let mut stream = StreamInfo::new(
        track.source_track_id,
        track.media_type,
        track.codec.clone(),
        time_base,
    );
    if let (Some(width), Some(height)) = (track.width, track.height) {
        stream = stream.with_dimensions(width, height);
    }
    if let (Some(sample_rate), Some(channels)) = (track.sample_rate, track.channels) {
        stream = stream.with_audio_format(sample_rate, channels);
    }
    if !track.codec_config.is_empty() {
        stream.codec_config = Some(track.codec_config.clone());
    }
    stream
}

fn parse_media_fragments(
    format: ContainerFormat,
    tracks: &[Fmp4Track],
    bytes: &Bytes,
) -> Result<Vec<EncodedPacket>> {
    let by_track_id = tracks
        .iter()
        .map(|track| (track.mp4_track_id, track))
        .collect::<BTreeMap<_, _>>();
    let mut packets = Vec::new();
    for box_ in read_boxes(bytes, 0, format)?
        .into_iter()
        .filter(|box_| box_.typ == *b"moof")
    {
        packets.extend(parse_moof(format, bytes, &box_, &by_track_id)?);
    }
    Ok(packets)
}

fn parse_moof(
    format: ContainerFormat,
    bytes: &Bytes,
    moof: &ParsedBox<'_>,
    tracks: &BTreeMap<u32, &Fmp4Track>,
) -> Result<Vec<EncodedPacket>> {
    let mut packets = Vec::new();
    for traf in read_boxes(moof.content, moof.content_start(), format)?
        .into_iter()
        .filter(|box_| box_.typ == *b"traf")
    {
        packets.extend(parse_traf_fragment(
            format, bytes, moof.start, &traf, tracks,
        )?);
    }
    Ok(packets)
}

fn parse_traf_fragment(
    format: ContainerFormat,
    bytes: &Bytes,
    moof_start: usize,
    traf: &ParsedBox<'_>,
    tracks: &BTreeMap<u32, &Fmp4Track>,
) -> Result<Vec<EncodedPacket>> {
    let children = read_boxes(traf.content, traf.content_start(), format)?;
    let tfhd = find_box(&children, b"tfhd").ok_or(Error::Parse {
        format: format.as_str(),
        message: "traf is missing tfhd".to_owned(),
    })?;
    let (track_id, default_duration, default_size, default_flags) =
        parse_tfhd(format, tfhd.content)?;
    let track = tracks.get(&track_id).ok_or(Error::IncompatibleTrack {
        track_id,
        reason: "fragment references a track missing from fMP4 init metadata",
    })?;

    let tfdt = find_box(&children, b"tfdt").ok_or(Error::Parse {
        format: format.as_str(),
        message: "traf is missing tfdt".to_owned(),
    })?;
    let mut dts = parse_tfdt(format, tfdt.content)?;
    let trun = find_box(&children, b"trun").ok_or(Error::Parse {
        format: format.as_str(),
        message: "traf is missing trun".to_owned(),
    })?;
    let trun = parse_trun(
        format,
        trun.content,
        default_duration,
        default_size,
        default_flags,
    )?;

    let base = i64::try_from(moof_start).map_err(|_| Error::Parse {
        format: format.as_str(),
        message: "moof offset exceeds signed range".to_owned(),
    })?;
    let mut sample_offset = usize::try_from(base + trun.data_offset).map_err(|_| Error::Parse {
        format: format.as_str(),
        message: "trun data offset points before the segment".to_owned(),
    })?;
    let time_base = TimeBase::new(1, track.timescale as i32)?;
    let mut out = Vec::with_capacity(trun.samples.len());
    for (index, sample) in trun.samples.iter().enumerate() {
        let sample_size = sample.size as usize;
        let sample_end = sample_offset.checked_add(sample_size).ok_or(Error::Parse {
            format: format.as_str(),
            message: "fragment sample range overflows usize".to_owned(),
        })?;
        let Some(data) = bytes.get(sample_offset..sample_end) else {
            return Err(Error::Parse {
                format: format.as_str(),
                message: "fragment sample range is outside media segment bytes".to_owned(),
            });
        };
        let flags = if index == 0 {
            trun.first_sample_flags.unwrap_or(sample.flags)
        } else {
            sample.flags
        };
        let cto = i64::from(sample.composition_time_offset);
        let pts = u64_to_i64(dts, format, "fragment decode timestamp")?
            .checked_add(cto)
            .ok_or(Error::InvalidPacketTiming {
                reason: "fMP4 presentation timestamp overflows i64",
            })?;
        let packet_data = demux_sample_payload(track, data, format)?;
        let packet = EncodedPacket::new(
            track.source_track_id,
            track.codec.clone(),
            pts,
            i64::from(sample.duration),
            time_base,
            packet_data,
        )
        .with_dts(u64_to_i64(dts, format, "fragment decode timestamp")?)
        .with_keyframe(track.media_type == MediaType::Audio || sample_is_sync(flags));
        out.push(packet);
        sample_offset = sample_end;
        dts = dts
            .checked_add(u64::from(sample.duration))
            .ok_or(Error::InvalidPacketTiming {
                reason: "fMP4 decode timestamp overflows u64",
            })?;
    }

    Ok(out)
}

fn demux_sample_payload(track: &Fmp4Track, data: &[u8], format: ContainerFormat) -> Result<Bytes> {
    if track.codec != CodecId::WebVtt {
        return Ok(Bytes::copy_from_slice(data));
    }

    let top = read_boxes(data, 0, format)?;
    let Some(vttc) = find_box(&top, b"vttc") else {
        return Ok(Bytes::new());
    };
    let cue_boxes = read_boxes(vttc.content, vttc.content_start(), format)?;
    let Some(payl) = find_box(&cue_boxes, b"payl") else {
        return Ok(Bytes::new());
    };
    Ok(Bytes::copy_from_slice(payl.content))
}

fn parse_tfhd(
    format: ContainerFormat,
    content: &[u8],
) -> Result<(u32, Option<u32>, Option<u32>, u32)> {
    let (_, flags, payload) = parse_full_box(format, "tfhd", content)?;
    if payload.len() < 4 {
        return Err(Error::Parse {
            format: format.as_str(),
            message: "tfhd box is truncated".to_owned(),
        });
    }
    let track_id = read_u32_at(payload, 0);
    let mut offset = 4usize;
    if flags & 0x000001 != 0 {
        offset = offset.checked_add(8).ok_or(Error::Parse {
            format: format.as_str(),
            message: "tfhd offset overflow".to_owned(),
        })?;
    }
    if flags & 0x000002 != 0 {
        offset = offset.checked_add(4).ok_or(Error::Parse {
            format: format.as_str(),
            message: "tfhd offset overflow".to_owned(),
        })?;
    }
    let default_duration = if flags & 0x000008 != 0 {
        if payload.len().saturating_sub(offset) < 4 {
            return Err(Error::Parse {
                format: format.as_str(),
                message: "tfhd default sample duration is truncated".to_owned(),
            });
        }
        let value = read_u32_at(payload, offset);
        offset += 4;
        Some(value)
    } else {
        None
    };
    let default_size = if flags & 0x000010 != 0 {
        if payload.len().saturating_sub(offset) < 4 {
            return Err(Error::Parse {
                format: format.as_str(),
                message: "tfhd default sample size is truncated".to_owned(),
            });
        }
        let value = read_u32_at(payload, offset);
        offset += 4;
        Some(value)
    } else {
        None
    };
    let default_flags = if flags & 0x000020 != 0 {
        if payload.len().saturating_sub(offset) < 4 {
            return Err(Error::Parse {
                format: format.as_str(),
                message: "tfhd default sample flags are truncated".to_owned(),
            });
        }
        read_u32_at(payload, offset)
    } else {
        0
    };
    Ok((track_id, default_duration, default_size, default_flags))
}

fn parse_tfdt(format: ContainerFormat, content: &[u8]) -> Result<u64> {
    let (version, _, payload) = parse_full_box(format, "tfdt", content)?;
    match version {
        0 => {
            if payload.len() < 4 {
                return Err(Error::Parse {
                    format: format.as_str(),
                    message: "tfdt box is truncated".to_owned(),
                });
            }
            Ok(u64::from(read_u32_at(payload, 0)))
        }
        1 => {
            if payload.len() < 8 {
                return Err(Error::Parse {
                    format: format.as_str(),
                    message: "tfdt version 1 box is truncated".to_owned(),
                });
            }
            Ok(read_u64_at(payload, 0))
        }
        _ => Err(Error::Parse {
            format: format.as_str(),
            message: "unsupported tfdt version".to_owned(),
        }),
    }
}

fn parse_trun(
    format: ContainerFormat,
    content: &[u8],
    default_duration: Option<u32>,
    default_size: Option<u32>,
    default_flags: u32,
) -> Result<TrunData> {
    let (version, flags, payload) = parse_full_box(format, "trun", content)?;
    if payload.len() < 4 {
        return Err(Error::Parse {
            format: format.as_str(),
            message: "trun box is truncated".to_owned(),
        });
    }
    let sample_count = read_u32_at(payload, 0) as usize;
    let mut offset = 4usize;
    let data_offset = if flags & 0x000001 != 0 {
        if payload.len().saturating_sub(offset) < 4 {
            return Err(Error::Parse {
                format: format.as_str(),
                message: "trun data offset is truncated".to_owned(),
            });
        }
        let value = i32::from_be_bytes(payload[offset..offset + 4].try_into().unwrap());
        offset += 4;
        i64::from(value)
    } else {
        0
    };
    let first_sample_flags = if flags & 0x000004 != 0 {
        if payload.len().saturating_sub(offset) < 4 {
            return Err(Error::Parse {
                format: format.as_str(),
                message: "trun first-sample flags are truncated".to_owned(),
            });
        }
        let value = read_u32_at(payload, offset);
        offset += 4;
        Some(value)
    } else {
        None
    };

    let mut samples = Vec::with_capacity(sample_count);
    for _ in 0..sample_count {
        let duration = if flags & 0x000100 != 0 {
            let value = read_required_u32(format, "trun sample duration", payload, offset)?;
            offset += 4;
            value
        } else {
            default_duration.ok_or(Error::Parse {
                format: format.as_str(),
                message: "trun omits sample duration and tfhd has no default".to_owned(),
            })?
        };
        let size = if flags & 0x000200 != 0 {
            let value = read_required_u32(format, "trun sample size", payload, offset)?;
            offset += 4;
            value
        } else {
            default_size.ok_or(Error::Parse {
                format: format.as_str(),
                message: "trun omits sample size and tfhd has no default".to_owned(),
            })?
        };
        let sample_flags = if flags & 0x000400 != 0 {
            let value = read_required_u32(format, "trun sample flags", payload, offset)?;
            offset += 4;
            value
        } else {
            default_flags
        };
        let composition_time_offset = if flags & 0x000800 != 0 {
            let raw = read_required_u32(format, "trun sample composition offset", payload, offset)?;
            offset += 4;
            if version == 0 {
                i32::try_from(raw).map_err(|_| Error::InvalidPacketTiming {
                    reason: "unsigned fMP4 composition offset exceeds i32",
                })?
            } else {
                i32::from_be_bytes(raw.to_be_bytes())
            }
        } else {
            0
        };
        samples.push(TrunSample {
            duration,
            size,
            flags: sample_flags,
            composition_time_offset,
        });
    }

    Ok(TrunData {
        data_offset,
        first_sample_flags,
        samples,
    })
}

fn parse_fragment_duration_seconds(
    format: ContainerFormat,
    tracks: &[Fmp4Track],
    bytes: &Bytes,
) -> Result<f64> {
    let packets = parse_media_fragments(format, tracks, bytes)?;
    infer_packet_duration_seconds(&packets).ok_or(Error::EmptyInput)
}

fn infer_packet_duration_seconds(packets: &[EncodedPacket]) -> Option<f64> {
    packets
        .iter()
        .map(|packet| {
            packet
                .time_base
                .ticks_to_seconds(packet.pts + packet.duration)
        })
        .filter(|seconds| seconds.is_finite())
        .max_by(f64::total_cmp)
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

fn sample_is_sync(flags: u32) -> bool {
    flags & 0x0001_0000 == 0
}

fn extract_esds_decoder_specific(format: ContainerFormat, content: &[u8]) -> Result<Bytes> {
    let (_, _, payload) = parse_full_box(format, "esds", content)?;
    find_descriptor_payload(payload, 0x05)
        .map(Bytes::copy_from_slice)
        .ok_or(Error::Parse {
            format: format.as_str(),
            message: "esds is missing DecoderSpecificInfo".to_owned(),
        })
}

fn find_descriptor_payload(payload: &[u8], target_tag: u8) -> Option<&[u8]> {
    for start in 0..payload.len() {
        let tag = payload[start];
        let size_start = start + 1;
        let (len, size_len) = match read_descriptor_size(payload.get(size_start..)?) {
            Some(size) => size,
            None => continue,
        };
        let offset = size_start + size_len;
        let end = match offset.checked_add(len) {
            Some(end) if end <= payload.len() => end,
            _ => continue,
        };
        let descriptor_payload = &payload[offset..end];
        if tag == target_tag {
            return Some(descriptor_payload);
        }
        if let Some(found) = find_descriptor_payload(descriptor_payload, target_tag) {
            return Some(found);
        }
    }
    None
}

fn read_descriptor_size(bytes: &[u8]) -> Option<(usize, usize)> {
    let mut size = 0usize;
    for (index, byte) in bytes.iter().copied().take(4).enumerate() {
        size = (size << 7) | usize::from(byte & 0x7f);
        if byte & 0x80 == 0 {
            return Some((size, index + 1));
        }
    }
    None
}

impl<'a> ParsedBox<'a> {
    fn content_start(&self) -> usize {
        self.start + self.header_size
    }
}

fn read_boxes<'a>(
    data: &'a [u8],
    base_offset: usize,
    format: ContainerFormat,
) -> Result<Vec<ParsedBox<'a>>> {
    let mut offset = 0usize;
    let mut boxes = Vec::new();
    while offset < data.len() {
        if data.len().saturating_sub(offset) < 8 {
            return Err(Error::Parse {
                format: format.as_str(),
                message: "MP4 box header is truncated".to_owned(),
            });
        }
        let size32 = read_u32_at(data, offset);
        let typ: [u8; 4] = data[offset + 4..offset + 8].try_into().unwrap();
        let (header_size, size) = if size32 == 1 {
            if data.len().saturating_sub(offset) < 16 {
                return Err(Error::Parse {
                    format: format.as_str(),
                    message: "extended MP4 box header is truncated".to_owned(),
                });
            }
            (16usize, read_u64_at(data, offset + 8) as usize)
        } else if size32 == 0 {
            (8usize, data.len() - offset)
        } else {
            (8usize, size32 as usize)
        };
        if size < header_size || data.len().saturating_sub(offset) < size {
            return Err(Error::Parse {
                format: format.as_str(),
                message: "MP4 box size exceeds available bytes".to_owned(),
            });
        }
        boxes.push(ParsedBox {
            typ,
            start: base_offset + offset,
            header_size,
            content: &data[offset + header_size..offset + size],
        });
        offset += size;
    }
    Ok(boxes)
}

fn find_box<'a>(boxes: &'a [ParsedBox<'a>], typ: &[u8; 4]) -> Option<&'a ParsedBox<'a>> {
    boxes.iter().find(|box_| box_.typ == *typ)
}

fn parse_full_box<'a>(
    format: ContainerFormat,
    name: &'static str,
    content: &'a [u8],
) -> Result<(u8, u32, &'a [u8])> {
    if content.len() < 4 {
        return Err(Error::Parse {
            format: format.as_str(),
            message: format!("{name} full-box header is truncated"),
        });
    }
    let version = content[0];
    let flags =
        (u32::from(content[1]) << 16) | (u32::from(content[2]) << 8) | u32::from(content[3]);
    Ok((version, flags, &content[4..]))
}

fn read_required_u32(
    format: ContainerFormat,
    field: &'static str,
    bytes: &[u8],
    offset: usize,
) -> Result<u32> {
    if bytes.len().saturating_sub(offset) < 4 {
        return Err(Error::Parse {
            format: format.as_str(),
            message: format!("{field} is truncated"),
        });
    }
    Ok(read_u32_at(bytes, offset))
}

fn read_u16_at(bytes: &[u8], offset: usize) -> u16 {
    u16::from_be_bytes(bytes[offset..offset + 2].try_into().unwrap())
}

fn read_u32_at(bytes: &[u8], offset: usize) -> u32 {
    u32::from_be_bytes(bytes[offset..offset + 4].try_into().unwrap())
}

fn read_u64_at(bytes: &[u8], offset: usize) -> u64 {
    u64::from_be_bytes(bytes[offset..offset + 8].try_into().unwrap())
}

fn u64_to_i64(value: u64, format: ContainerFormat, field: &'static str) -> Result<i64> {
    value.try_into().map_err(|_| Error::Parse {
        format: format.as_str(),
        message: format!("{field} is too large for packet timing"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_h264_init_and_media_segments() {
        let time_base = TimeBase::milliseconds();
        let avcc = Bytes::from_static(b"\x01\x42\x00\x1e\xff\xe1\0\x02\x67\x42\x01\0\x02\x68\xce");
        let mut media = MediaInfo::default();
        let mut stream =
            StreamInfo::new(1, MediaType::Video, CodecId::H264, time_base).with_dimensions(16, 16);
        stream.codec_config = Some(avcc);
        media.push_stream(stream);

        let segments = vec![
            vec![
                EncodedPacket::new(
                    1,
                    CodecId::H264,
                    0,
                    1_000,
                    time_base,
                    Bytes::from_static(b"\0\0\0\x01\x67\x42\0\0\0\x01\x68\xce\0\0\0\x01\x65\x88"),
                )
                .with_keyframe(true),
            ],
            vec![EncodedPacket::new(
                1,
                CodecId::H264,
                1_000,
                1_000,
                time_base,
                Bytes::from_static(b"\0\0\0\x01\x41\x99"),
            )],
        ];

        let output = mux_fragmented_mp4_segments(&media, &segments, 2_000).unwrap();

        assert!(contains_box(&output.init_segment, b"ftyp"));
        assert!(contains_box(&output.init_segment, b"moov"));
        assert!(contains_box(&output.init_segment, b"avc1"));
        assert_eq!(output.media_segments.len(), 2);
        assert!(
            output
                .media_segments
                .iter()
                .all(|segment| contains_box(segment, b"moof"))
        );
        assert!(
            output
                .media_segments
                .iter()
                .all(|segment| contains_box(segment, b"mdat"))
        );
    }

    #[test]
    fn writes_h264_and_aac_multitrack_fragments() {
        let video_time_base = TimeBase::milliseconds();
        let audio_time_base = TimeBase::new(1, 48_000).unwrap();
        let mut media = MediaInfo::default();
        let mut video = StreamInfo::new(1, MediaType::Video, CodecId::H264, video_time_base)
            .with_dimensions(16, 16);
        video.codec_config = Some(Bytes::from_static(
            b"\x01\x42\x00\x1e\xff\xe1\0\x02\x67\x42\x01\0\x02\x68\xce",
        ));
        media.push_stream(video);
        let mut audio = StreamInfo::new(2, MediaType::Audio, CodecId::Aac, audio_time_base)
            .with_audio_format(48_000, 2);
        audio.codec_config = Some(Bytes::from_static(&[0x11, 0x90]));
        media.push_stream(audio);

        let segments = vec![vec![
            EncodedPacket::new(
                1,
                CodecId::H264,
                0,
                1_000,
                video_time_base,
                Bytes::from_static(b"\0\0\0\x01\x65\x88"),
            )
            .with_keyframe(true),
            EncodedPacket::new(
                2,
                CodecId::Aac,
                0,
                1024,
                audio_time_base,
                Bytes::from_static(b"\x11\x22"),
            )
            .with_keyframe(true),
        ]];

        let output = mux_fragmented_mp4_segments(&media, &segments, 2_000).unwrap();

        assert!(contains_box(&output.init_segment, b"avc1"));
        assert!(contains_box(&output.init_segment, b"mp4a"));
        assert!(contains_box(&output.init_segment, b"esds"));
        assert_eq!(count_box_name(&output.media_segments[0], b"traf"), 2);
    }

    #[test]
    fn demuxes_muxed_h264_and_aac_segments() {
        let video_time_base = TimeBase::milliseconds();
        let audio_time_base = TimeBase::new(1, 48_000).unwrap();
        let mut media = MediaInfo::default();
        let mut video = StreamInfo::new(1, MediaType::Video, CodecId::H264, video_time_base)
            .with_dimensions(16, 16);
        video.codec_config = Some(Bytes::from_static(
            b"\x01\x42\x00\x1e\xff\xe1\0\x02\x67\x42\x01\0\x02\x68\xce",
        ));
        media.push_stream(video);
        let mut audio = StreamInfo::new(2, MediaType::Audio, CodecId::Aac, audio_time_base)
            .with_audio_format(48_000, 2);
        audio.codec_config = Some(Bytes::from_static(&[0x11, 0x90]));
        media.push_stream(audio);

        let segments = vec![vec![
            EncodedPacket::new(
                1,
                CodecId::H264,
                0,
                40,
                video_time_base,
                Bytes::from_static(b"\0\0\0\x01\x65\x88"),
            )
            .with_keyframe(true),
            EncodedPacket::new(
                2,
                CodecId::Aac,
                0,
                1024,
                audio_time_base,
                Bytes::from_static(b"\x11\x22"),
            )
            .with_keyframe(true),
        ]];
        let output = mux_fragmented_mp4_segments(&media, &segments, 1_000).unwrap();

        let demuxed = demux_fragmented_mp4_segments(
            ContainerFormat::Mp4,
            &output.init_segment,
            &output.media_segments,
        )
        .unwrap();

        assert_eq!(demuxed.media.streams.len(), 2);
        assert_eq!(demuxed.packets.len(), 2);
        assert_eq!(demuxed.packets[0].track_id, 1);
        assert_eq!(demuxed.packets[0].codec, CodecId::H264);
        assert_eq!(&demuxed.packets[0].data[..], b"\0\0\0\x02\x65\x88");
        assert_eq!(demuxed.packets[1].track_id, 2);
        assert_eq!(demuxed.packets[1].codec, CodecId::Aac);
        assert_eq!(&demuxed.packets[1].data[..], b"\x11\x22");
    }

    #[test]
    fn demuxes_concatenated_fragmented_mp4_bytes() {
        let time_base = TimeBase::milliseconds();
        let avcc = Bytes::from_static(b"\x01\x42\x00\x1e\xff\xe1\0\x02\x67\x42\x01\0\x02\x68\xce");
        let mut media = MediaInfo::default();
        let mut stream =
            StreamInfo::new(7, MediaType::Video, CodecId::H264, time_base).with_dimensions(16, 16);
        stream.codec_config = Some(avcc);
        media.push_stream(stream);
        let segments = vec![vec![
            EncodedPacket::new(
                7,
                CodecId::H264,
                0,
                40,
                time_base,
                Bytes::from_static(b"\0\0\0\x01\x65\x88"),
            )
            .with_keyframe(true),
        ]];
        let output = mux_fragmented_mp4_segments(&media, &segments, 1_000).unwrap();
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&output.init_segment);
        bytes.extend_from_slice(&output.media_segments[0]);

        let demuxed =
            demux_fragmented_mp4_bytes(ContainerFormat::Mp4, &Bytes::from(bytes)).unwrap();

        assert_eq!(demuxed.media.streams[0].track_id, 1);
        assert_eq!(
            demuxed.media.streams[0].codec_config,
            media.streams[0].codec_config
        );
        assert_eq!(demuxed.packets.len(), 1);
        assert!(demuxed.packets[0].is_keyframe);
    }

    #[test]
    fn muxes_and_demuxes_webvtt_subtitle_track() {
        let video_time_base = TimeBase::milliseconds();
        let subtitle_time_base = TimeBase::milliseconds();
        let mut media = MediaInfo::default();
        let mut video = StreamInfo::new(1, MediaType::Video, CodecId::H264, video_time_base)
            .with_dimensions(16, 16);
        video.codec_config = Some(Bytes::from_static(
            b"\x01\x42\x00\x1e\xff\xe1\0\x02\x67\x42\x01\0\x02\x68\xce",
        ));
        media.push_stream(video);
        media.push_stream(StreamInfo::new(
            3,
            MediaType::Subtitle,
            CodecId::WebVtt,
            subtitle_time_base,
        ));

        let segments = vec![vec![
            EncodedPacket::new(
                1,
                CodecId::H264,
                0,
                1_000,
                video_time_base,
                Bytes::from_static(b"\0\0\0\x01\x65\x88"),
            )
            .with_keyframe(true),
            EncodedPacket::new(
                3,
                CodecId::WebVtt,
                250,
                1_500,
                subtitle_time_base,
                Bytes::from_static(b"caption text"),
            )
            .with_keyframe(true),
        ]];

        let output = mux_fragmented_mp4_segments(&media, &segments, 1_000).unwrap();
        let demuxed = demux_fragmented_mp4_segments(
            ContainerFormat::Mp4,
            &output.init_segment,
            &output.media_segments,
        )
        .unwrap();

        assert!(contains_box(&output.init_segment, b"wvtt"));
        assert!(contains_box(&output.init_segment, b"vttC"));
        let subtitle = demuxed
            .packets
            .iter()
            .find(|packet| packet.codec == CodecId::WebVtt)
            .unwrap();
        assert_eq!(subtitle.pts, 250);
        assert_eq!(subtitle.duration, 1_500);
        assert_eq!(&subtitle.data[..], b"caption text");
    }

    fn contains_box(bytes: &[u8], name: &[u8; 4]) -> bool {
        bytes.windows(4).any(|window| window == name)
    }

    fn count_box_name(bytes: &[u8], name: &[u8; 4]) -> usize {
        bytes.windows(4).filter(|window| *window == name).count()
    }
}
