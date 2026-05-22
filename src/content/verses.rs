//! On-demand fetch of verse text + translation from equran.id.
//!
//! Mirrors [`content::downloader`](crate::content::downloader): a short-lived
//! worker thread checks the on-disk cache first, fetches from the network only
//! on a miss, and writes the cache file atomically (`.part`, then rename).
//! Results return to the main loop via [`AppMessage::Verses`].

use std::fs;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

use anyhow::Context;
use crossbeam_channel::Sender;
use serde::Deserialize;

use crate::domain::verses::{SurahVerses, Verse};
use crate::event::AppMessage;

/// Per-request network timeout.
const TIMEOUT: Duration = Duration::from_secs(30);

/// Spawn a worker that loads surah `surah`'s verses — from the on-disk cache if
/// present, otherwise from equran.id — and reports the result to `tx`.
pub fn fetch_surah(surah: u16, verses_dir: PathBuf, tx: Sender<AppMessage>) {
    thread::Builder::new()
        .name(format!("verses-{surah}"))
        .spawn(move || {
            let result = load_or_fetch(surah, &verses_dir).map_err(|e| e.to_string());
            let _ = tx.send(AppMessage::Verses { surah, result });
        })
        .expect("failed to spawn verse worker");
}

/// The cache path for one surah: `<dir>/NNN.json`.
fn cache_path(dir: &Path, surah: u16) -> PathBuf {
    dir.join(format!("{surah:03}.json"))
}

/// Return the cached verses if a valid cache file exists, otherwise fetch from
/// the network, write the cache, and return.
fn load_or_fetch(surah: u16, dir: &Path) -> anyhow::Result<SurahVerses> {
    let path = cache_path(dir, surah);
    if let Some(cached) = read_cache(&path) {
        return Ok(cached);
    }
    let fetched = fetch_remote(surah)?;
    write_cache(&path, &fetched);
    Ok(fetched)
}

/// Read and parse a cache file. Any error (absent, corrupt) yields `None`, so
/// the caller falls back to a network fetch.
fn read_cache(path: &Path) -> Option<SurahVerses> {
    let text = fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

/// Fetch one surah's verses from equran.id.
fn fetch_remote(surah: u16) -> anyhow::Result<SurahVerses> {
    let url = format!("https://equran.id/api/v2/surat/{surah}");
    let body = ureq::get(&url)
        .timeout(TIMEOUT)
        .call()
        .with_context(|| format!("requesting surah {surah}"))?
        .into_string()
        .context("reading response body")?;
    let response: ApiResponse =
        serde_json::from_str(&body).context("parsing equran.id response")?;
    let verses = response
        .data
        .ayat
        .into_iter()
        .map(|a| Verse {
            ar: a.teks_arab,
            latin: a.teks_latin,
            id: a.teks_indonesia,
        })
        .collect();
    Ok(SurahVerses { surah, verses })
}

/// Write `verses` to `path` atomically via a `.part` sibling + rename.
fn write_cache(path: &Path, verses: &SurahVerses) {
    if let Some(parent) = path.parent() {
        if let Err(err) = fs::create_dir_all(parent) {
            tracing::warn!("could not create verses cache dir: {err}");
            return;
        }
    }
    let json = match serde_json::to_string(verses) {
        Ok(json) => json,
        Err(err) => {
            tracing::warn!("could not serialize verses: {err}");
            return;
        }
    };
    let tmp = path.with_extension("part");
    if let Err(err) = fs::write(&tmp, json) {
        tracing::warn!("could not write {tmp:?}: {err}");
        return;
    }
    if let Err(err) = fs::rename(&tmp, path) {
        tracing::warn!("could not finalize {path:?}: {err}");
        let _ = fs::remove_file(&tmp);
    }
}

/// equran.id `/api/v2/surat/{n}` response — only the fields we consume.
#[derive(Deserialize)]
struct ApiResponse {
    data: ApiData,
}

#[derive(Deserialize)]
struct ApiData {
    ayat: Vec<ApiAyah>,
}

#[derive(Deserialize)]
struct ApiAyah {
    #[serde(rename = "teksArab")]
    teks_arab: String,
    #[serde(rename = "teksLatin")]
    teks_latin: String,
    #[serde(rename = "teksIndonesia")]
    teks_indonesia: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_path_is_zero_padded() {
        let dir = Path::new("/tmp/verses");
        assert_eq!(cache_path(dir, 1), Path::new("/tmp/verses/001.json"));
        assert_eq!(cache_path(dir, 112), Path::new("/tmp/verses/112.json"));
    }

    /// Fetches one real surah from equran.id. Ignored by default because it
    /// needs network access — run with `cargo test -- --ignored`.
    #[test]
    #[ignore = "requires network access"]
    fn fetches_a_real_surah() {
        let surah = fetch_remote(112).expect("fetch should succeed");
        assert_eq!(surah.surah, 112);
        assert_eq!(surah.verses.len(), 4);
        assert!(!surah.verses[0].ar.is_empty());
        assert!(!surah.verses[0].id.is_empty());
    }
}
