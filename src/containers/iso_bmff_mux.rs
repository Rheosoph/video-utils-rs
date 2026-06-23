use crate::{
    bitstream::{
        aac::aac_packet_to_adts, h264::h264_packet_to_annex_b, h265::h265_packet_to_annex_b,
    },
    codec::{CodecId, MediaType},
    container::{ContainerFormat, ContainerMuxer},
    error::{Error, Result},
    media::{MediaInfo, StreamInfo},
    packet::{EncodedPacket, validate_monotonic_by_track},
};
use bytes::Bytes;
use muxide::api::{AacProfile, AudioCodec, MuxerBuilder, MuxerError, VideoCodec};
use std::cmp::Ordering;

/// MP4 muxer backed by the pure Rust `muxide` writer.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct IsoBmffMuxer {
    format: ContainerFormat,
}

impl IsoBmffMuxer {
    /// Create an MP4/MOV-family muxer.
    pub fn new(format: ContainerFormat) -> Result<Self> {
        if !matches!(format, ContainerFormat::Mp4 | ContainerFormat::QuickTime) {
            return Err(Error::Unsupported {
                operation: "iso-bmff mux",
                reason: "the current Rust-native mux adapter writes MP4/MOV-family output only",
            });
        }

        Ok(Self { format })
    }
}

impl ContainerMuxer for IsoBmffMuxer {
    fn container_format(&self) -> ContainerFormat {
        self.format
    }

    fn supports_stream(&self, stream: &StreamInfo) -> bool {
        matches!(
            (&stream.media_type, &stream.codec),
            (
                MediaType::Video,
                CodecId::H264 | CodecId::H265 | CodecId::AV1 | CodecId::VP9
            ) | (MediaType::Audio, CodecId::Aac | CodecId::Opus)
        )
    }

    fn mux_bytes(&self, media: &MediaInfo, packets: &[EncodedPacket]) -> Result<Bytes> {
        mux_iso_bmff_bytes(self.format, media, packets)
    }
}

/// Mux stream metadata and encoded packets into MP4/MOV-family bytes.
pub fn mux_iso_bmff_bytes(
    format: ContainerFormat,
    media: &MediaInfo,
    packets: &[EncodedPacket],
) -> Result<Bytes> {
    let muxer = IsoBmffMuxer::new(format)?;
    let tracks = select_mux_tracks(media)?;
    validate_mux_tracks(&muxer, tracks.video, tracks.audio)?;
    validate_packets(media, packets)?;
    validate_stream_packet_coverage(tracks, packets)?;

    let prepared_packets = prepare_packets_for_mux(media, tracks, packets)?;
    let video_packets = packets_for_track(&prepared_packets, tracks.video.track_id);
    if video_packets.is_empty() {
        return Err(Error::EmptyInput);
    }

    let width = tracks.video.width.ok_or(Error::Unsupported {
        operation: "iso-bmff mux",
        reason: "video stream width is required for MP4 muxing",
    })?;
    let height = tracks.video.height.ok_or(Error::Unsupported {
        operation: "iso-bmff mux",
        reason: "video stream height is required for MP4 muxing",
    })?;
    let framerate = infer_framerate(tracks.video, &video_packets);
    let video_codec = muxide_video_codec(&tracks.video.codec)?;

    let mut output = Vec::new();
    {
        let mut builder =
            MuxerBuilder::new(&mut output).video(video_codec, width, height, framerate);
        if let Some(audio) = tracks.audio {
            let sample_rate = audio.sample_rate.ok_or(Error::Unsupported {
                operation: "iso-bmff mux",
                reason: "audio sample rate is required for MP4 muxing",
            })?;
            let channels = audio.channels.ok_or(Error::Unsupported {
                operation: "iso-bmff mux",
                reason: "audio channel count is required for MP4 muxing",
            })?;
            builder = builder.audio(muxide_audio_codec(&audio.codec)?, sample_rate, channels);
        }

        let mut writer = builder.build().map_err(mp4_mux_error)?;
        let mut ordered_packets = prepared_packets.iter().collect::<Vec<_>>();
        ordered_packets.sort_by(|left, right| packet_write_order(left, right, media));
        move_first_video_packet_to_front(&mut ordered_packets, tracks.video.track_id);

        for packet in ordered_packets {
            let stream = media
                .stream(packet.track_id)
                .ok_or(Error::IncompatibleTrack {
                    track_id: packet.track_id,
                    reason: "packet references a stream missing from MediaInfo",
                })?;
            match stream.media_type {
                MediaType::Video => write_video_packet(&mut writer, stream, packet)?,
                MediaType::Audio => write_audio_packet(&mut writer, stream, packet)?,
                _ => {
                    return Err(Error::Unsupported {
                        operation: "iso-bmff mux",
                        reason: "only video and audio packets can be written by the MP4 muxer",
                    });
                }
            }
        }

        writer.finish().map_err(mp4_mux_error)?;
    }

    if format == ContainerFormat::QuickTime {
        brand_as_quicktime(&mut output)?;
    }

    Ok(Bytes::from(output))
}

fn prepare_packets_for_mux(
    media: &MediaInfo,
    tracks: MuxTracks<'_>,
    packets: &[EncodedPacket],
) -> Result<Vec<EncodedPacket>> {
    let first_video_pts = packets
        .iter()
        .filter(|packet| packet.track_id == tracks.video.track_id)
        .map(EncodedPacket::pts_seconds)
        .min_by(f64::total_cmp)
        .ok_or(Error::EmptyInput)?;
    let audio_track_id = tracks.audio.map(|audio| audio.track_id);

    let mut prepared = packets
        .iter()
        .filter(|packet| {
            packet.track_id == tracks.video.track_id
                || audio_track_id.is_some_and(|track_id| packet.track_id == track_id)
        })
        .filter(|packet| {
            audio_track_id.is_none_or(|track_id| packet.track_id != track_id)
                || packet.pts_seconds() + 1.0e-9 >= first_video_pts
        })
        .cloned()
        .collect::<Vec<_>>();

    validate_stream_packet_coverage(tracks, &prepared)?;
    rebase_packets_to_zero(&mut prepared)?;
    validate_packets(media, &prepared)?;
    Ok(prepared)
}

fn rebase_packets_to_zero(packets: &mut [EncodedPacket]) -> Result<()> {
    let Some(start_seconds) = packets
        .iter()
        .flat_map(|packet| {
            [
                Some(packet.pts_seconds()),
                packet.dts.map(|dts| packet.time_base.ticks_to_seconds(dts)),
            ]
        })
        .flatten()
        .min_by(f64::total_cmp)
    else {
        return Ok(());
    };

    if start_seconds <= 0.0 {
        return Ok(());
    }

    for packet in packets {
        let offset = packet.time_base.seconds_to_ticks(start_seconds);
        shift_timestamp(&mut packet.pts, offset)?;
        if let Some(dts) = &mut packet.dts {
            shift_timestamp(dts, offset)?;
        }
    }

    Ok(())
}

fn shift_timestamp(value: &mut i64, offset: i64) -> Result<()> {
    *value = value
        .checked_sub(offset)
        .ok_or(Error::InvalidPacketTiming {
            reason: "timestamp rebase overflowed",
        })?;
    if *value < -1 {
        return Err(Error::InvalidPacketTiming {
            reason: "timestamp rebase produced a negative timestamp",
        });
    }
    if *value < 0 {
        *value = 0;
    }
    Ok(())
}

fn move_first_video_packet_to_front(
    ordered_packets: &mut Vec<&EncodedPacket>,
    video_track_id: u32,
) {
    if ordered_packets
        .first()
        .is_some_and(|packet| packet.track_id == video_track_id)
    {
        return;
    }
    if let Some(index) = ordered_packets
        .iter()
        .position(|packet| packet.track_id == video_track_id)
    {
        let packet = ordered_packets.remove(index);
        ordered_packets.insert(0, packet);
    }
}

fn write_video_packet<W: std::io::Write>(
    writer: &mut muxide::api::Muxer<W>,
    stream: &StreamInfo,
    packet: &EncodedPacket,
) -> Result<()> {
    let data = match packet.codec {
        CodecId::H264 => h264_packet_to_annex_b(packet, stream.codec_config.as_ref())?,
        CodecId::H265 => h265_packet_to_annex_b(packet, stream.codec_config.as_ref())?,
        CodecId::AV1 | CodecId::VP9 => packet.data.clone(),
        _ => {
            return Err(Error::Unsupported {
                operation: "iso-bmff mux",
                reason: "video codec is not supported by the Rust-native MP4 muxer",
            });
        }
    };

    let pts = packet.pts_seconds();
    if let Some(dts) = packet.dts {
        writer
            .write_video_with_dts(
                pts,
                packet.time_base.ticks_to_seconds(dts),
                &data,
                packet.is_keyframe,
            )
            .map_err(mp4_mux_error)
    } else {
        writer
            .write_video(pts, &data, packet.is_keyframe)
            .map_err(mp4_mux_error)
    }
}

fn write_audio_packet<W: std::io::Write>(
    writer: &mut muxide::api::Muxer<W>,
    stream: &StreamInfo,
    packet: &EncodedPacket,
) -> Result<()> {
    let data = match packet.codec {
        CodecId::Aac => aac_packet_to_adts(packet, stream.codec_config.as_ref())?,
        CodecId::Opus => packet.data.clone(),
        _ => {
            return Err(Error::Unsupported {
                operation: "iso-bmff mux",
                reason: "audio codec is not supported by the Rust-native MP4 muxer",
            });
        }
    };

    writer
        .write_audio(packet.pts_seconds(), &data)
        .map_err(mp4_mux_error)
}

#[derive(Clone, Copy)]
struct MuxTracks<'a> {
    video: &'a StreamInfo,
    audio: Option<&'a StreamInfo>,
}

fn select_mux_tracks(media: &MediaInfo) -> Result<MuxTracks<'_>> {
    let video = media.video_streams().collect::<Vec<_>>();
    let audio = media.audio_streams().collect::<Vec<_>>();

    if video.len() != 1 {
        return Err(Error::Unsupported {
            operation: "iso-bmff mux",
            reason: "the current Rust-native MP4 muxer requires exactly one video stream",
        });
    }
    if audio.len() > 1 {
        return Err(Error::Unsupported {
            operation: "iso-bmff mux",
            reason: "the current Rust-native MP4 muxer supports at most one audio stream",
        });
    }
    if media
        .streams
        .iter()
        .any(|stream| !matches!(stream.media_type, MediaType::Video | MediaType::Audio))
    {
        return Err(Error::Unsupported {
            operation: "iso-bmff mux",
            reason: "the current Rust-native MP4 muxer does not write subtitle or data tracks",
        });
    }

    Ok(MuxTracks {
        video: video[0],
        audio: audio.first().copied(),
    })
}

fn validate_mux_tracks(
    muxer: &IsoBmffMuxer,
    video: &StreamInfo,
    audio: Option<&StreamInfo>,
) -> Result<()> {
    if !muxer.supports_stream(video) {
        return Err(Error::Unsupported {
            operation: "iso-bmff mux",
            reason: "video stream is not supported by the Rust-native MP4 muxer",
        });
    }
    if let Some(audio) = audio
        && !muxer.supports_stream(audio)
    {
        return Err(Error::Unsupported {
            operation: "iso-bmff mux",
            reason: "audio stream is not supported by the Rust-native MP4 muxer",
        });
    }
    Ok(())
}

fn validate_packets(media: &MediaInfo, packets: &[EncodedPacket]) -> Result<()> {
    if packets.is_empty() {
        return Err(Error::EmptyInput);
    }
    validate_monotonic_by_track(packets)?;

    for packet in packets {
        let stream = media
            .stream(packet.track_id)
            .ok_or(Error::IncompatibleTrack {
                track_id: packet.track_id,
                reason: "packet references a stream missing from MediaInfo",
            })?;
        if packet.codec != stream.codec {
            return Err(Error::CodecMismatch {
                expected: stream.codec.clone(),
                actual: packet.codec.clone(),
            });
        }
        if packet.time_base != stream.time_base {
            return Err(Error::TimeBaseMismatch {
                expected: stream.time_base,
                actual: packet.time_base,
            });
        }
        if packet.pts < 0 || packet.dts.is_some_and(|dts| dts < 0) {
            return Err(Error::InvalidPacketTiming {
                reason: "negative timestamps cannot be muxed into MP4",
            });
        }
    }

    Ok(())
}

fn validate_stream_packet_coverage(tracks: MuxTracks<'_>, packets: &[EncodedPacket]) -> Result<()> {
    if !packets
        .iter()
        .any(|packet| packet.track_id == tracks.video.track_id)
    {
        return Err(Error::IncompatibleTrack {
            track_id: tracks.video.track_id,
            reason: "video stream has no packets to mux",
        });
    }
    if let Some(audio) = tracks.audio
        && !packets
            .iter()
            .any(|packet| packet.track_id == audio.track_id)
    {
        return Err(Error::IncompatibleTrack {
            track_id: audio.track_id,
            reason: "audio stream has no packets to mux",
        });
    }
    Ok(())
}

fn packets_for_track(packets: &[EncodedPacket], track_id: u32) -> Vec<&EncodedPacket> {
    packets
        .iter()
        .filter(|packet| packet.track_id == track_id)
        .collect()
}

fn infer_framerate(video: &StreamInfo, packets: &[&EncodedPacket]) -> f64 {
    if let Some(seconds) = packets
        .iter()
        .find(|packet| packet.duration > 0)
        .map(|packet| packet.duration_seconds())
        .filter(|seconds| *seconds > 0.0 && seconds.is_finite())
    {
        return (1.0 / seconds).clamp(1.0, 240.0);
    }

    if let Some(duration_seconds) = video.duration_seconds()
        && duration_seconds > 0.0
    {
        return (packets.len() as f64 / duration_seconds).clamp(1.0, 240.0);
    }

    30.0
}

fn packet_write_order(left: &EncodedPacket, right: &EncodedPacket, media: &MediaInfo) -> Ordering {
    let left_ts = left.time_base.ticks_to_seconds(left.decode_order_ts());
    let right_ts = right.time_base.ticks_to_seconds(right.decode_order_ts());
    left_ts
        .total_cmp(&right_ts)
        .then_with(|| media_priority(left, media).cmp(&media_priority(right, media)))
        .then_with(|| left.track_id.cmp(&right.track_id))
        .then_with(|| left.pts.cmp(&right.pts))
}

fn media_priority(packet: &EncodedPacket, media: &MediaInfo) -> u8 {
    match media
        .stream(packet.track_id)
        .map(|stream| stream.media_type)
    {
        Some(MediaType::Video) => 0,
        Some(MediaType::Audio) => 1,
        _ => 2,
    }
}

fn muxide_video_codec(codec: &CodecId) -> Result<VideoCodec> {
    match codec {
        CodecId::H264 => Ok(VideoCodec::H264),
        CodecId::H265 => Ok(VideoCodec::H265),
        CodecId::AV1 => Ok(VideoCodec::Av1),
        CodecId::VP9 => Ok(VideoCodec::Vp9),
        _ => Err(Error::Unsupported {
            operation: "iso-bmff mux",
            reason: "video codec is not supported by the Rust-native MP4 muxer",
        }),
    }
}

fn muxide_audio_codec(codec: &CodecId) -> Result<AudioCodec> {
    match codec {
        CodecId::Aac => Ok(AudioCodec::Aac(AacProfile::Lc)),
        CodecId::Opus => Ok(AudioCodec::Opus),
        _ => Err(Error::Unsupported {
            operation: "iso-bmff mux",
            reason: "audio codec is not supported by the Rust-native MP4 muxer",
        }),
    }
}

fn mp4_mux_error(err: MuxerError) -> Error {
    Error::Mux {
        format: "mp4",
        message: err.to_string(),
    }
}

fn brand_as_quicktime(output: &mut [u8]) -> Result<()> {
    if output.len() < 16 || &output[4..8] != b"ftyp" {
        return Err(Error::Mux {
            format: "mov",
            message: "muxer output did not start with an ftyp box".to_owned(),
        });
    }
    output[8..12].copy_from_slice(b"qt  ");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{prepare_packets_for_mux, select_mux_tracks};
    use crate::{
        codec::{CodecId, MediaType},
        media::{MediaInfo, StreamInfo},
        packet::EncodedPacket,
        time::TimeBase,
    };

    #[test]
    fn prepare_packets_drops_audio_before_first_video_pts() {
        let media = h264_aac_media();
        let packets = vec![
            packet(2, CodecId::Aac, 1_400),
            packet(1, CodecId::H264, 1_421),
            packet(2, CodecId::Aac, 1_422),
            packet(1, CodecId::H264, 1_454),
        ];

        let prepared =
            prepare_packets_for_mux(&media, select_mux_tracks(&media).unwrap(), &packets).unwrap();

        let timeline = prepared
            .iter()
            .map(|packet| (packet.track_id, packet.pts))
            .collect::<Vec<_>>();
        assert_eq!(timeline, vec![(1, 0), (2, 1), (1, 33)]);
    }

    #[test]
    fn prepare_packets_preserves_audio_offset_after_video_start() {
        let media = h264_aac_media();
        let packets = vec![
            packet(1, CodecId::H264, 1_000),
            packet(2, CodecId::Aac, 1_021),
            packet(1, CodecId::H264, 1_033),
        ];

        let prepared =
            prepare_packets_for_mux(&media, select_mux_tracks(&media).unwrap(), &packets).unwrap();

        let timeline = prepared
            .iter()
            .map(|packet| (packet.track_id, packet.pts))
            .collect::<Vec<_>>();
        assert_eq!(timeline, vec![(1, 0), (2, 21), (1, 33)]);
    }

    fn h264_aac_media() -> MediaInfo {
        let time_base = TimeBase::milliseconds();
        let mut media = MediaInfo::default();
        media.push_stream(
            StreamInfo::new(1, MediaType::Video, CodecId::H264, time_base)
                .with_dimensions(320, 180),
        );
        media.push_stream(
            StreamInfo::new(2, MediaType::Audio, CodecId::Aac, time_base)
                .with_audio_format(48_000, 2),
        );
        media
    }

    fn packet(track_id: u32, codec: CodecId, pts: i64) -> EncodedPacket {
        EncodedPacket::new(track_id, codec, pts, 1, TimeBase::milliseconds(), vec![0])
    }
}
