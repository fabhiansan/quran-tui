//! Content resolution — a 1:1 port of `QuranContentResolver` (§12.1–12.3).
//!
//! Resolution priority:
//! 1. Every per-ayah file (plus bismillah where required) exists → [`Resolution::Local`].
//! 2. Else, a single full surah whose whole-surah file exists → [`Resolution::WholeSurahFallback`].
//! 3. Else → [`Resolution::Missing`].

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::domain::catalog::{Reciter, Surah};
use crate::domain::segment::PlaybackSegment;
use crate::domain::slug::surah_slug;

/// Outcome of resolving a playback request against local files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolution {
    /// Every requested file is present locally; play this exact list.
    Local(Vec<PathBuf>),
    /// Per-ayah files were absent but a whole-surah file exists.
    WholeSurahFallback(PathBuf),
    /// Files are missing and must be downloaded first.
    Missing { count: usize },
}

/// One ayah file that must be downloaded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MissingFile {
    pub local_path: PathBuf,
    pub remote_url: String,
    /// Human-readable label, e.g. `36:12` or `bismillah`.
    pub label: String,
}

/// Zero-pad a number to three digits.
fn three_digit(n: u16) -> String {
    format!("{n:03}")
}

/// `<root>/<perAyahDirectorySlug>/<NNN>/<AAA>.mp3`
pub fn per_ayah_local_path(
    reciter: &Reciter,
    surah_number: u16,
    ayah: u16,
    root: &Path,
) -> PathBuf {
    root.join(&reciter.per_ayah_directory_slug)
        .join(three_digit(surah_number))
        .join(format!("{}.mp3", three_digit(ayah)))
}

/// `<root>/<NNN>-<surah_slug>-<fileSlug>.mp3`
pub fn whole_surah_local_path(surah: &Surah, reciter: &Reciter, root: &Path) -> PathBuf {
    let slug = surah_slug(&surah.name_transliterated);
    root.join(format!(
        "{}-{}-{}.mp3",
        three_digit(surah.number),
        slug,
        reciter.file_slug
    ))
}

/// `<root>/<perAyahDirectorySlug>/001/001.mp3` — ayah 1 of surah 1 doubles as
/// the standalone bismillah clip.
pub fn bismillah_local_path(reciter: &Reciter, root: &Path) -> PathBuf {
    root.join(&reciter.per_ayah_directory_slug)
        .join("001")
        .join("001.mp3")
}

/// `https://everyayah.com/data/<perAyahDirectorySlug>/<NNN><AAA>.mp3`
pub fn per_ayah_remote_url(reciter: &Reciter, surah_number: u16, ayah: u16) -> String {
    format!(
        "https://everyayah.com/data/{}/{}{}.mp3",
        reciter.per_ayah_directory_slug,
        three_digit(surah_number),
        three_digit(ayah)
    )
}

/// `https://everyayah.com/data/<perAyahDirectorySlug>/001001.mp3`
pub fn bismillah_remote_url(reciter: &Reciter) -> String {
    format!(
        "https://everyayah.com/data/{}/001001.mp3",
        reciter.per_ayah_directory_slug
    )
}

/// Prepend the bismillah clip before a segment iff the range starts at ayah 1
/// and the surah is not 1 (Al-Fatihah contains it) and not 9 (At-Tawbah has no
/// basmala). Port of `QuranContentResolver.shouldPrependBismillah` (§12.2).
pub fn should_prepend_bismillah(segment: &PlaybackSegment) -> bool {
    *segment.range.start() == 1 && segment.surah.number != 1 && segment.surah.number != 9
}

/// Resolve a playback request against local files.
pub fn resolve(segments: &[PlaybackSegment], reciter: &Reciter, root: &Path) -> Resolution {
    if segments.is_empty() {
        return Resolution::Missing { count: 0 };
    }

    // 1. Per-ayah files for every segment.
    if let Some(paths) = existing_per_ayah_paths(segments, reciter, root) {
        return Resolution::Local(paths);
    }

    // 2. Whole-surah fallback for a single full surah.
    if let Some(segment) = single_full_surah(segments) {
        let path = whole_surah_local_path(&segment.surah, reciter, root);
        if path.is_file() {
            return Resolution::WholeSurahFallback(path);
        }
    }

    // 3. Missing.
    Resolution::Missing {
        count: missing_files(segments, reciter, root).len(),
    }
}

/// The per-ayah file list (with bismillah prepends) iff *every* file exists.
fn existing_per_ayah_paths(
    segments: &[PlaybackSegment],
    reciter: &Reciter,
    root: &Path,
) -> Option<Vec<PathBuf>> {
    let mut paths = Vec::new();
    for segment in segments {
        if should_prepend_bismillah(segment) {
            let bismillah = bismillah_local_path(reciter, root);
            if !bismillah.is_file() {
                return None;
            }
            paths.push(bismillah);
        }
        for ayah in segment.range.clone() {
            let path = per_ayah_local_path(reciter, segment.surah.number, ayah, root);
            if !path.is_file() {
                return None;
            }
            paths.push(path);
        }
    }
    Some(paths)
}

/// `Some` when `segments` is exactly one surah played in full.
fn single_full_surah(segments: &[PlaybackSegment]) -> Option<&PlaybackSegment> {
    if segments.len() != 1 {
        return None;
    }
    let segment = &segments[0];
    if *segment.range.start() == 1 && *segment.range.end() == segment.surah.ayah_count {
        Some(segment)
    } else {
        None
    }
}

/// Every per-ayah (and bismillah) file the request needs that is not on disk,
/// de-duplicated. Drives both the "Missing" count and the downloader.
pub fn missing_files(
    segments: &[PlaybackSegment],
    reciter: &Reciter,
    root: &Path,
) -> Vec<MissingFile> {
    let mut seen = HashSet::new();
    let mut missing = Vec::new();

    for segment in segments {
        if should_prepend_bismillah(segment) {
            let path = bismillah_local_path(reciter, root);
            if !path.is_file() && seen.insert(path.clone()) {
                missing.push(MissingFile {
                    local_path: path,
                    remote_url: bismillah_remote_url(reciter),
                    label: "bismillah".to_string(),
                });
            }
        }
        for ayah in segment.range.clone() {
            let path = per_ayah_local_path(reciter, segment.surah.number, ayah, root);
            if !path.is_file() && seen.insert(path.clone()) {
                missing.push(MissingFile {
                    local_path: path,
                    remote_url: per_ayah_remote_url(reciter, segment.surah.number, ayah),
                    label: format!("{}:{}", segment.surah.number, ayah),
                });
            }
        }
    }
    missing
}

/// The per-ayah (and bismillah) path list for `segments`, built unconditionally
/// without checking whether the files exist — the path layout is deterministic.
/// Parallel to [`per_ayah_labels`]. Used to assemble a playlist's full track
/// list once [`missing_files`] has confirmed every file is present.
pub fn per_ayah_paths(
    segments: &[PlaybackSegment],
    reciter: &Reciter,
    root: &Path,
) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    for segment in segments {
        if should_prepend_bismillah(segment) {
            paths.push(bismillah_local_path(reciter, root));
        }
        for ayah in segment.range.clone() {
            paths.push(per_ayah_local_path(
                reciter,
                segment.surah.number,
                ayah,
                root,
            ));
        }
    }
    paths
}

/// Human-readable label per track, parallel to the per-ayah path order
/// produced by a `Local` resolution (bismillah prepends included).
pub fn per_ayah_labels(segments: &[PlaybackSegment]) -> Vec<String> {
    let mut labels = Vec::new();
    for segment in segments {
        if should_prepend_bismillah(segment) {
            labels.push("Bismillah".to_string());
        }
        for ayah in segment.range.clone() {
            labels.push(format!("{}:{}", segment.surah.number, ayah));
        }
    }
    labels
}
