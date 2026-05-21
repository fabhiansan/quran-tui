//! Playback segmentation — a 1:1 port of `QuranContentResolver.playbackSegments`.
//!
//! A request spanning multiple surahs is broken into one segment per surah: the
//! first surah clamps the lower ayah bound, the last clamps the upper bound, and
//! every middle surah plays in full.

use std::ops::RangeInclusive;

use crate::domain::catalog::Surah;

/// One contiguous span of ayahs within a single surah.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlaybackSegment {
    pub surah: Surah,
    pub range: RangeInclusive<u16>,
}

/// Break a (possibly multi-surah) request into per-surah playback segments.
///
/// `from_surah`/`to_surah` may be given in either order — they are normalized.
/// Ayah bounds are clamped to each surah's `ayah_count`.
pub fn playback_segments(
    surahs: &[Surah],
    from_surah: u16,
    to_surah: u16,
    from_ayah: u16,
    to_ayah: u16,
) -> Vec<PlaybackSegment> {
    let lower_surah = from_surah.min(to_surah);
    let upper_surah = from_surah.max(to_surah);

    let mut selected: Vec<&Surah> = surahs
        .iter()
        .filter(|s| (lower_surah..=upper_surah).contains(&s.number))
        .collect();
    selected.sort_by_key(|s| s.number);

    let (Some(first), Some(last)) = (selected.first(), selected.last()) else {
        return Vec::new();
    };
    let first_number = first.number;
    let last_number = last.number;

    selected
        .iter()
        .filter_map(|surah| {
            let count = surah.ayah_count;
            let range: RangeInclusive<u16> = if first_number == last_number {
                // Single surah: clamp both bounds.
                let lower = from_ayah.clamp(1, count);
                let upper = to_ayah.clamp(lower, count);
                lower..=upper
            } else if surah.number == first_number {
                // First of several: clamp the lower bound, play to the end.
                let lower = from_ayah.clamp(1, count);
                lower..=count
            } else if surah.number == last_number {
                // Last of several: start at ayah 1, clamp the upper bound.
                let upper = to_ayah.clamp(1, count);
                1..=upper
            } else {
                // A middle surah: play in full.
                1..=count
            };

            if range.start() <= range.end() {
                Some(PlaybackSegment {
                    surah: (*surah).clone(),
                    range,
                })
            } else {
                None
            }
        })
        .collect()
}
