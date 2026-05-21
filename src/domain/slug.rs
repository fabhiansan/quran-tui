//! Surah-name slugging — a 1:1 port of `QuranContentResolver.surahSlug` (§12.4).

use unicode_normalization::UnicodeNormalization;

/// Build the filesystem slug for a transliterated surah name.
///
/// Algorithm: diacritic-fold (NFD + strip combining marks), lowercase, then per
/// character keep `a-z`/`0-9`, collapse runs of `-`/space to a single `-`, and
/// **drop** apostrophes/quotes (and anything else) entirely. Trim leading and
/// trailing `-`.
///
/// The apostrophe rule is load-bearing: `"Al-Mu'minun"` must slug to
/// `"al-muminun"`, not `"al-mu-minun"`.
pub fn surah_slug(name: &str) -> String {
    let mut result = String::new();
    let mut previous_was_dash = false;

    // NFD decomposes accented letters into base + combining mark; dropping the
    // combining marks performs the diacritic fold.
    for ch in name.nfd().flat_map(char::to_lowercase) {
        if is_combining_mark(ch) {
            continue;
        }
        match ch {
            'a'..='z' | '0'..='9' => {
                result.push(ch);
                previous_was_dash = false;
            }
            '-' | ' ' if !previous_was_dash => {
                result.push('-');
                previous_was_dash = true;
            }
            // A separator already inside a run, an apostrophe/quote, or any
            // other character: dropped — an apostrophe must NOT become a
            // separator (`"Al-Mu'minun"` → `"al-muminun"`).
            _ => {}
        }
    }

    result.trim_matches('-').to_string()
}

/// True for Unicode combining marks (the ranges that matter for Latin-script
/// diacritics produced by NFD decomposition).
fn is_combining_mark(ch: char) -> bool {
    matches!(
        ch as u32,
        0x0300..=0x036F   // Combining Diacritical Marks
            | 0x1AB0..=0x1AFF // Combining Diacritical Marks Extended
            | 0x1DC0..=0x1DFF // Combining Diacritical Marks Supplement
            | 0x20D0..=0x20FF // Combining Diacritical Marks for Symbols
            | 0xFE20..=0xFE2F // Combining Half Marks
    )
}
