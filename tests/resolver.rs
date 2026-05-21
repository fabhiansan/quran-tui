//! Tests for content resolution against the `TestAssets/` fixtures (Phase 3 gate).

use std::path::PathBuf;

use quran_tui::content::resolver::{self, Resolution};
use quran_tui::domain::catalog::Catalog;
use quran_tui::domain::segment::playback_segments;

fn assets_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../TestAssets")
}

#[test]
fn surah_18_full_range_resolves_to_local_per_ayah() {
    let catalog = Catalog::load();
    let reciter = catalog.reciter("alafasy").unwrap();
    let count = catalog.surah(18).unwrap().ayah_count;
    let segments = playback_segments(&catalog.surahs, 18, 18, 1, count);

    match resolver::resolve(&segments, reciter, &assets_root()) {
        // 110 ayah files + 1 prepended bismillah
        Resolution::Local(paths) => assert_eq!(paths.len(), 111),
        other => panic!("expected Local, got {other:?}"),
    }
}

#[test]
fn surah_18_subrange_resolves_without_bismillah() {
    let catalog = Catalog::load();
    let reciter = catalog.reciter("alafasy").unwrap();
    let segments = playback_segments(&catalog.surahs, 18, 18, 5, 10);

    match resolver::resolve(&segments, reciter, &assets_root()) {
        Resolution::Local(paths) => assert_eq!(paths.len(), 6),
        other => panic!("expected Local, got {other:?}"),
    }
}

#[test]
fn surah_1_falls_back_to_the_whole_surah_file() {
    let catalog = Catalog::load();
    let reciter = catalog.reciter("alafasy").unwrap();
    let count = catalog.surah(1).unwrap().ayah_count;
    let segments = playback_segments(&catalog.surahs, 1, 1, 1, count);

    match resolver::resolve(&segments, reciter, &assets_root()) {
        Resolution::WholeSurahFallback(path) => {
            assert!(path.ends_with("001-al-fatihah-mishary-alafasy.mp3"));
        }
        other => panic!("expected WholeSurahFallback, got {other:?}"),
    }
}

#[test]
fn absent_surah_reports_missing_count() {
    let catalog = Catalog::load();
    let reciter = catalog.reciter("alafasy").unwrap();
    // Surah 50, ayahs 5..=10 — no local files, no bismillah (range start != 1).
    let segments = playback_segments(&catalog.surahs, 50, 50, 5, 10);

    match resolver::resolve(&segments, reciter, &assets_root()) {
        Resolution::Missing { count } => assert_eq!(count, 6),
        other => panic!("expected Missing, got {other:?}"),
    }
}

#[test]
fn bismillah_prepend_rule() {
    let catalog = Catalog::load();

    let s18 = playback_segments(&catalog.surahs, 18, 18, 1, 10);
    assert!(resolver::should_prepend_bismillah(&s18[0]));

    // Al-Fatihah already contains the basmala.
    let s1 = playback_segments(&catalog.surahs, 1, 1, 1, 7);
    assert!(!resolver::should_prepend_bismillah(&s1[0]));

    // At-Tawbah has no basmala.
    let s9 = playback_segments(&catalog.surahs, 9, 9, 1, 10);
    assert!(!resolver::should_prepend_bismillah(&s9[0]));

    // Range that does not start at ayah 1.
    let s18b = playback_segments(&catalog.surahs, 18, 18, 5, 10);
    assert!(!resolver::should_prepend_bismillah(&s18b[0]));
}

#[test]
fn path_builders_match_conventions() {
    let catalog = Catalog::load();
    let reciter = catalog.reciter("alafasy").unwrap();
    let surah18 = catalog.surah(18).unwrap();
    let root = PathBuf::from("/audio");

    assert!(resolver::per_ayah_local_path(reciter, 18, 5, &root)
        .ends_with("Alafasy_64kbps/018/005.mp3"));
    assert!(resolver::whole_surah_local_path(surah18, reciter, &root)
        .ends_with("018-al-kahf-mishary-alafasy.mp3"));
    assert!(resolver::bismillah_local_path(reciter, &root).ends_with("Alafasy_64kbps/001/001.mp3"));

    assert_eq!(
        resolver::per_ayah_remote_url(reciter, 18, 5),
        "https://everyayah.com/data/Alafasy_64kbps/018005.mp3"
    );
    assert_eq!(
        resolver::bismillah_remote_url(reciter),
        "https://everyayah.com/data/Alafasy_64kbps/001001.mp3"
    );
}

#[test]
fn per_ayah_labels_track_the_playlist_order() {
    let catalog = Catalog::load();
    let segments = playback_segments(&catalog.surahs, 18, 18, 1, 3);
    let labels = resolver::per_ayah_labels(&segments);
    assert_eq!(labels, vec!["Bismillah", "18:1", "18:2", "18:3"]);
}
