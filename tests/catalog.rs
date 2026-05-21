//! Tests for the embedded catalog (Phase 1 gate).

use quran_tui::domain::catalog::Catalog;
use quran_tui::domain::slug::surah_slug;

#[test]
fn loads_114_surahs_and_the_reciters() {
    let catalog = Catalog::load();
    assert_eq!(catalog.surahs.len(), 114);
    assert!(catalog.reciters.len() >= 2);
}

#[test]
fn surah_numbers_run_1_through_114() {
    let catalog = Catalog::load();
    for (i, s) in catalog.surahs.iter().enumerate() {
        assert_eq!(s.number, (i + 1) as u16);
    }
}

#[test]
fn every_surah_slugs_to_a_nonempty_string() {
    let catalog = Catalog::load();
    for s in &catalog.surahs {
        assert!(
            !surah_slug(&s.name_transliterated).is_empty(),
            "surah {} ({}) produced an empty slug",
            s.number,
            s.name_transliterated
        );
    }
}

#[test]
fn known_reciters_are_present() {
    let catalog = Catalog::load();
    assert!(catalog.reciter("alafasy").is_some());
    assert!(catalog.surah(18).is_some());
    assert!(catalog.surah(200).is_none());
}
