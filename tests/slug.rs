//! Tests for `surah_slug` — these mirror the Swift app's DEBUG assertions and
//! must pass identically (Phase 1 gate).

use quran_tui::domain::slug::surah_slug;

#[test]
fn basic_names() {
    assert_eq!(surah_slug("Al-Fatihah"), "al-fatihah");
    assert_eq!(surah_slug("Al-Ikhlas"), "al-ikhlas");
    assert_eq!(surah_slug("Al-Falaq"), "al-falaq");
    assert_eq!(surah_slug("An-Nas"), "an-nas");
}

#[test]
fn apostrophes_are_dropped_not_hyphenated() {
    assert_eq!(surah_slug("Al-Mu'minun"), "al-muminun");
    assert_eq!(surah_slug("Ash-Shu'ara"), "ash-shuara");
    assert_eq!(surah_slug("Ali 'Imran"), "ali-imran");
}

#[test]
fn curly_quotes_are_dropped() {
    assert_eq!(surah_slug("Al-Mu\u{2019}minun"), "al-muminun");
    assert_eq!(surah_slug("\u{201C}An-Nas\u{201D}"), "an-nas");
}

#[test]
fn separator_runs_collapse_and_trim() {
    assert_eq!(surah_slug("  Al--Kahf  "), "al-kahf");
    assert_eq!(surah_slug("---"), "");
    assert_eq!(surah_slug("An Nisa"), "an-nisa");
}

#[test]
fn diacritics_are_folded() {
    assert_eq!(surah_slug("T\u{0101}-H\u{0101}"), "ta-ha");
    assert_eq!(surah_slug("\u{00C9}clair"), "eclair");
}
