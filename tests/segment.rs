//! Tests for `playback_segments` (Phase 1 gate).

use quran_tui::domain::catalog::{Catalog, Surah};
use quran_tui::domain::segment::playback_segments;

/// Build a minimal test surah with the given number and ayah count.
fn surah(number: u16, ayah_count: u16) -> Surah {
    Surah {
        number,
        name_transliterated: format!("Surah {number}"),
        name_arabic: String::new(),
        ayah_count,
        revelation_place: "Meccan".into(),
    }
}

#[test]
fn single_surah_subrange() {
    let surahs = vec![surah(18, 110)];
    let segs = playback_segments(&surahs, 18, 18, 5, 20);
    assert_eq!(segs.len(), 1);
    assert_eq!(segs[0].surah.number, 18);
    assert_eq!(segs[0].range, 5..=20);
}

#[test]
fn single_surah_clamps_bounds_to_ayah_count() {
    let surahs = vec![surah(112, 4)];
    let segs = playback_segments(&surahs, 112, 112, 1, 99);
    assert_eq!(segs[0].range, 1..=4);
}

#[test]
fn three_surah_span_first_partial_middle_full_last_partial() {
    let surahs = vec![surah(1, 7), surah(2, 286), surah(3, 200)];
    let segs = playback_segments(&surahs, 1, 3, 4, 10);
    assert_eq!(segs.len(), 3);
    assert_eq!((segs[0].surah.number, segs[0].range.clone()), (1, 4..=7));
    assert_eq!((segs[1].surah.number, segs[1].range.clone()), (2, 1..=286));
    assert_eq!((segs[2].surah.number, segs[2].range.clone()), (3, 1..=10));
}

#[test]
fn reversed_surah_input_normalizes() {
    let surahs = vec![surah(1, 7), surah(2, 286), surah(3, 200)];
    let forward = playback_segments(&surahs, 1, 3, 4, 10);
    let reversed = playback_segments(&surahs, 3, 1, 4, 10);
    assert_eq!(forward, reversed);
}

#[test]
fn empty_when_no_surah_falls_in_range() {
    let surahs = vec![surah(1, 7)];
    assert!(playback_segments(&surahs, 50, 60, 1, 1).is_empty());
}

#[test]
fn resolves_against_the_real_catalog() {
    let catalog = Catalog::load();
    let segs = playback_segments(&catalog.surahs, 1, 1, 1, 7);
    assert_eq!(segs.len(), 1);
    assert_eq!(segs[0].surah.name_transliterated, "Al-Fatihah");
    assert_eq!(segs[0].range, 1..=7);
}
