use crate::{
    codec::CodecId,
    error::{Error, Result},
    time::TimeBase,
};
use bytes::Bytes;
use std::collections::BTreeMap;

/// Encoded packet that has not been decoded into audio/video frames.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EncodedPacket {
    /// Container track identifier.
    pub track_id: u32,
    /// Encoded codec.
    pub codec: CodecId,
    /// Presentation timestamp in `time_base` ticks.
    pub pts: i64,
    /// Decode timestamp in `time_base` ticks when known.
    pub dts: Option<i64>,
    /// Packet duration in `time_base` ticks.
    pub duration: i64,
    /// Timestamp time base.
    pub time_base: TimeBase,
    /// Whether this packet begins a random-access point.
    pub is_keyframe: bool,
    /// Encoded packet payload.
    pub data: Bytes,
}

impl EncodedPacket {
    /// Build a packet with no decode timestamp.
    #[must_use]
    pub fn new(
        track_id: u32,
        codec: CodecId,
        pts: i64,
        duration: i64,
        time_base: TimeBase,
        data: impl Into<Bytes>,
    ) -> Self {
        Self {
            track_id,
            codec,
            pts,
            dts: None,
            duration,
            time_base,
            is_keyframe: false,
            data: data.into(),
        }
    }

    /// Attach a decode timestamp.
    #[must_use]
    pub fn with_dts(mut self, dts: i64) -> Self {
        self.dts = Some(dts);
        self
    }

    /// Mark the packet as a keyframe or non-keyframe.
    #[must_use]
    pub fn with_keyframe(mut self, is_keyframe: bool) -> Self {
        self.is_keyframe = is_keyframe;
        self
    }

    /// End presentation timestamp.
    #[must_use]
    pub fn end_pts(&self) -> i64 {
        self.pts + self.duration
    }

    /// Timestamp used for decode order validation.
    #[must_use]
    pub fn decode_order_ts(&self) -> i64 {
        self.dts.unwrap_or(self.pts)
    }

    /// Presentation timestamp in seconds.
    #[must_use]
    pub fn pts_seconds(&self) -> f64 {
        self.time_base.ticks_to_seconds(self.pts)
    }

    /// Duration in seconds.
    #[must_use]
    pub fn duration_seconds(&self) -> f64 {
        self.time_base.ticks_to_seconds(self.duration)
    }
}

/// Half-open packet range into a packet slice.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PacketSlice {
    /// Inclusive start index.
    pub start: usize,
    /// Exclusive end index.
    pub end: usize,
}

impl PacketSlice {
    /// Length in packets.
    #[must_use]
    pub fn len(self) -> usize {
        self.end.saturating_sub(self.start)
    }

    /// True when the range is empty.
    #[must_use]
    pub fn is_empty(self) -> bool {
        self.start >= self.end
    }
}

/// Validate packet durations and monotonic decode timestamps per track.
pub fn validate_monotonic_by_track(packets: &[EncodedPacket]) -> Result<()> {
    let mut last_by_track = BTreeMap::<u32, i64>::new();

    for packet in packets {
        if packet.duration < 0 {
            return Err(Error::InvalidPacketTiming {
                reason: "negative duration",
            });
        }

        let ts = packet.decode_order_ts();
        if let Some(last) = last_by_track.insert(packet.track_id, ts)
            && ts < last
        {
            return Err(Error::InvalidPacketTiming {
                reason: "decode timestamps are not monotonic within a track",
            });
        }
    }

    Ok(())
}

/// Rebase timestamps so each track starts at zero or later.
pub fn normalize_timestamps(packets: &mut [EncodedPacket]) -> Result<()> {
    validate_monotonic_by_track(packets)?;

    let mut min_by_track = BTreeMap::<u32, i64>::new();
    for packet in packets.iter() {
        let candidate = packet.dts.unwrap_or(packet.pts).min(packet.pts);
        min_by_track
            .entry(packet.track_id)
            .and_modify(|min_ts| *min_ts = (*min_ts).min(candidate))
            .or_insert(candidate);
    }

    for packet in packets {
        let offset = min_by_track.get(&packet.track_id).copied().unwrap_or(0);
        packet.pts -= offset;
        if let Some(dts) = &mut packet.dts {
            *dts -= offset;
        }
    }

    Ok(())
}

/// Select a keyframe-aligned range for one track.
pub fn select_keyframe_range(
    packets: &[EncodedPacket],
    track_id: u32,
    start_seconds: f64,
    end_seconds: f64,
) -> Result<PacketSlice> {
    if packets.is_empty() {
        return Err(Error::EmptyInput);
    }
    if !start_seconds.is_finite() || !end_seconds.is_finite() || end_seconds <= start_seconds {
        return Err(Error::InvalidRange {
            start: start_seconds.round() as i64,
            end: end_seconds.round() as i64,
        });
    }

    let track_packets: Vec<(usize, &EncodedPacket)> = packets
        .iter()
        .enumerate()
        .filter(|(_, packet)| packet.track_id == track_id)
        .collect();

    if track_packets.is_empty() {
        return Err(Error::IncompatibleTrack {
            track_id,
            reason: "track is not present",
        });
    }

    let time_base = track_packets[0].1.time_base;
    let start_tick = time_base.seconds_to_ticks(start_seconds);
    let end_tick = time_base.seconds_to_ticks(end_seconds);

    let start = track_packets
        .iter()
        .rev()
        .find(|(_, packet)| packet.is_keyframe && packet.pts <= start_tick)
        .or_else(|| track_packets.iter().find(|(_, packet)| packet.is_keyframe))
        .map(|(index, _)| *index)
        .unwrap_or(track_packets[0].0);

    let end = track_packets
        .iter()
        .find(|(_, packet)| packet.pts >= end_tick)
        .map(|(index, _)| *index)
        .unwrap_or_else(|| {
            track_packets
                .last()
                .map(|(index, _)| index + 1)
                .unwrap_or(start)
        });

    if end <= start {
        return Err(Error::InvalidRange {
            start: start as i64,
            end: end as i64,
        });
    }

    Ok(PacketSlice { start, end })
}

/// Return cloned packets for a single track.
#[must_use]
pub fn filter_track(packets: &[EncodedPacket], track_id: u32) -> Vec<EncodedPacket> {
    packets
        .iter()
        .filter(|packet| packet.track_id == track_id)
        .cloned()
        .collect()
}

/// Validate that all groups can be packet-copy concatenated.
pub fn validate_concat_compatible(groups: &[&[EncodedPacket]]) -> Result<()> {
    if groups.is_empty() || groups.iter().all(|group| group.is_empty()) {
        return Err(Error::EmptyInput);
    }

    let mut signature = BTreeMap::<u32, (CodecId, TimeBase)>::new();

    for group in groups.iter().copied().filter(|group| !group.is_empty()) {
        validate_monotonic_by_track(group)?;

        for packet in group {
            let current = (packet.codec.clone(), packet.time_base);
            match signature.get(&packet.track_id) {
                Some((codec, _)) if *codec != packet.codec => {
                    return Err(Error::CodecMismatch {
                        expected: codec.clone(),
                        actual: packet.codec.clone(),
                    });
                }
                Some((_, time_base)) if *time_base != packet.time_base => {
                    return Err(Error::TimeBaseMismatch {
                        expected: *time_base,
                        actual: packet.time_base,
                    });
                }
                Some(_) => {}
                None => {
                    signature.insert(packet.track_id, current);
                }
            }
        }
    }

    Ok(())
}

/// Concatenate packet groups with per-track timestamp rebasing.
pub fn concat_copy(groups: &[&[EncodedPacket]]) -> Result<Vec<EncodedPacket>> {
    validate_concat_compatible(groups)?;

    let mut output = Vec::new();
    let mut offsets = BTreeMap::<u32, i64>::new();

    for group in groups.iter().copied().filter(|group| !group.is_empty()) {
        let mut packets = group.to_vec();
        normalize_timestamps(&mut packets)?;

        let mut group_ends = BTreeMap::<u32, i64>::new();
        for packet in &mut packets {
            let offset = offsets.get(&packet.track_id).copied().unwrap_or(0);
            packet.pts += offset;
            if let Some(dts) = &mut packet.dts {
                *dts += offset;
            }
            group_ends
                .entry(packet.track_id)
                .and_modify(|end| *end = (*end).max(packet.end_pts()))
                .or_insert_with(|| packet.end_pts());
        }

        for (track_id, end) in group_ends {
            offsets.insert(track_id, end);
        }
        output.extend(packets);
    }

    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::{EncodedPacket, concat_copy, normalize_timestamps, select_keyframe_range};
    use crate::{codec::CodecId, time::TimeBase};

    fn packet(pts: i64, keyframe: bool) -> EncodedPacket {
        EncodedPacket::new(
            1,
            CodecId::H264,
            pts,
            1_000,
            TimeBase::milliseconds(),
            vec![0],
        )
        .with_keyframe(keyframe)
    }

    #[test]
    fn normalizes_negative_start() {
        let mut packets = vec![packet(-1_000, true), packet(0, false)];
        normalize_timestamps(&mut packets).unwrap();

        assert_eq!(packets[0].pts, 0);
        assert_eq!(packets[1].pts, 1_000);
    }

    #[test]
    fn selects_previous_keyframe_for_trim() {
        let packets = vec![
            packet(0, true),
            packet(1_000, false),
            packet(2_000, true),
            packet(3_000, false),
        ];

        let range = select_keyframe_range(&packets, 1, 1.5, 3.1).unwrap();

        assert_eq!(range.start, 0);
        assert_eq!(range.end, 4);
    }

    #[test]
    fn concat_rebases_each_group() {
        let first = vec![packet(10, true), packet(1_010, false)];
        let second = vec![packet(500, true)];

        let merged = concat_copy(&[&first, &second]).unwrap();

        assert_eq!(merged[0].pts, 0);
        assert_eq!(merged[1].pts, 1_000);
        assert_eq!(merged[2].pts, 2_000);
    }
}
