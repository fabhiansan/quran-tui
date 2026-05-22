//! Verse text and translation, fetched on demand and cached per surah.
//! Pure data types — the fetch/cache I/O lives in `content::verses`.

use serde::{Deserialize, Serialize};

/// One ayah: Arabic text, Latin transliteration, and Indonesian translation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Verse {
    /// Arabic (Uthmani) text.
    pub ar: String,
    /// Latin transliteration.
    pub latin: String,
    /// Indonesian translation.
    pub id: String,
}

/// Every ayah of one surah, in ayah order — `verses[0]` is ayah 1.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SurahVerses {
    pub surah: u16,
    pub verses: Vec<Verse>,
}

impl SurahVerses {
    /// Look up a verse by its 1-based ayah number.
    pub fn ayah(&self, ayah: u16) -> Option<&Verse> {
        let index = (ayah as usize).checked_sub(1)?;
        self.verses.get(index)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> SurahVerses {
        let verse = |s: &str| Verse {
            ar: format!("ar-{s}"),
            latin: format!("latin-{s}"),
            id: format!("id-{s}"),
        };
        SurahVerses {
            surah: 112,
            verses: vec![verse("1"), verse("2")],
        }
    }

    #[test]
    fn ayah_lookup_is_one_based() {
        let sv = sample();
        assert_eq!(sv.ayah(1).unwrap().ar, "ar-1");
        assert_eq!(sv.ayah(2).unwrap().id, "id-2");
    }

    #[test]
    fn ayah_zero_and_out_of_range_are_none() {
        let sv = sample();
        assert!(sv.ayah(0).is_none());
        assert!(sv.ayah(3).is_none());
    }
}
