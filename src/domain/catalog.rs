//! The Quran catalog — 114 surahs and the available reciters. Ported from the
//! Swift `QuranCatalog`. The JSON is embedded at compile time; a parse failure
//! is a build-correctness bug, so `load()` panics rather than returning a Result.

use serde::Deserialize;

/// One surah's catalog metadata.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Surah {
    pub number: u16,
    #[serde(rename = "nameTransliterated")]
    pub name_transliterated: String,
    #[serde(rename = "nameArabic")]
    pub name_arabic: String,
    #[serde(rename = "ayahCount")]
    pub ayah_count: u16,
    #[serde(rename = "revelationPlace")]
    pub revelation_place: String,
}

/// One reciter and the path slugs used to locate their audio.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Reciter {
    pub id: String,
    #[serde(rename = "displayName")]
    pub display_name: String,
    #[serde(rename = "fileSlug")]
    pub file_slug: String,
    #[serde(rename = "perAyahDirectorySlug")]
    pub per_ayah_directory_slug: String,
}

#[derive(Debug, Deserialize)]
struct CatalogPayload {
    surahs: Vec<Surah>,
    reciters: Vec<Reciter>,
}

/// The loaded catalog.
#[derive(Debug, Clone)]
pub struct Catalog {
    pub surahs: Vec<Surah>,
    pub reciters: Vec<Reciter>,
}

/// The catalog JSON, embedded into the binary at compile time.
const CATALOG_JSON: &str = include_str!("../../assets/quran-catalog.json");

impl Catalog {
    /// Parse the embedded catalog. Panics on malformed JSON — the asset is
    /// compiled in, so failure is a bug, not a runtime condition.
    pub fn load() -> Self {
        let payload: CatalogPayload = serde_json::from_str(CATALOG_JSON)
            .expect("embedded quran-catalog.json must be valid; parse failure is a build bug");
        Self {
            surahs: payload.surahs,
            reciters: payload.reciters,
        }
    }

    /// Look up a surah by its 1-based number.
    pub fn surah(&self, number: u16) -> Option<&Surah> {
        self.surahs.iter().find(|s| s.number == number)
    }

    /// Look up a reciter by its stable `id`.
    pub fn reciter(&self, id: &str) -> Option<&Reciter> {
        self.reciters.iter().find(|r| r.id == id)
    }
}
