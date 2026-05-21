//! On-demand audio download from everyayah.com, on a short-lived worker thread.
//!
//! Each file is written atomically: downloaded to `<file>.part`, then renamed.
//! A failure removes the partial file and stops the worker.

use std::fs::{self, File};
use std::io;
use std::path::Path;
use std::thread;
use std::time::Duration;

use crossbeam_channel::Sender;

use crate::content::resolver::MissingFile;
use crate::event::{AppMessage, DownloadUpdate};
use crate::model::output::OutputId;

/// Per-request network timeout.
const TIMEOUT: Duration = Duration::from_secs(30);

/// Spawn a worker that downloads every file in `missing`, reporting progress to
/// `tx` tagged with `output_id`.
pub fn download_missing(missing: Vec<MissingFile>, tx: Sender<AppMessage>, output_id: OutputId) {
    thread::Builder::new()
        .name(format!("downloader-{output_id}"))
        .spawn(move || run(missing, &tx, output_id))
        .expect("failed to spawn download worker");
}

fn run(missing: Vec<MissingFile>, tx: &Sender<AppMessage>, output_id: OutputId) {
    let total = missing.len();
    let send = |update| {
        let _ = tx.send(AppMessage::Download(output_id, update));
    };

    send(DownloadUpdate::Progress {
        done: 0,
        total,
        label: "starting".to_string(),
    });

    for (i, file) in missing.iter().enumerate() {
        if let Err(err) = download_one(&file.remote_url, &file.local_path) {
            tracing::warn!("download failed for {}: {err}", file.label);
            send(DownloadUpdate::Failed(format!("{} — {err}", file.label)));
            return;
        }
        send(DownloadUpdate::Progress {
            done: i + 1,
            total,
            label: file.label.clone(),
        });
    }

    send(DownloadUpdate::Completed);
}

/// Download `url` to `local_path` atomically. On any error the partial `.part`
/// file is removed so only complete `.mp3` files ever appear on disk.
fn download_one(url: &str, local_path: &Path) -> anyhow::Result<()> {
    if let Some(parent) = local_path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = local_path.with_extension("part");

    let result = (|| -> anyhow::Result<()> {
        let response = ureq::get(url).timeout(TIMEOUT).call()?;
        let mut reader = response.into_reader();
        let mut file = File::create(&tmp)?;
        io::copy(&mut reader, &mut file)?;
        file.sync_all()?;
        Ok(())
    })();

    match result {
        Ok(()) => {
            fs::rename(&tmp, local_path)?;
            Ok(())
        }
        Err(err) => {
            let _ = fs::remove_file(&tmp);
            Err(err)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::content::resolver;
    use crate::domain::catalog::Catalog;

    /// Downloads one real ayah from everyayah.com. Ignored by default because
    /// it needs network access — run with `cargo test -- --ignored`.
    #[test]
    #[ignore = "requires network access"]
    fn downloads_a_single_ayah() {
        let catalog = Catalog::load();
        let reciter = catalog.reciter("alafasy").unwrap();
        let dir = std::env::temp_dir().join("quran-tui-download-test");
        let _ = fs::remove_dir_all(&dir);

        let local = resolver::per_ayah_local_path(reciter, 112, 1, &dir);
        let url = resolver::per_ayah_remote_url(reciter, 112, 1);
        download_one(&url, &local).expect("download should succeed");

        assert!(local.is_file(), "downloaded file should exist");
        assert!(
            local.metadata().unwrap().len() > 0,
            "file should be non-empty"
        );
        let _ = fs::remove_dir_all(&dir);
    }
}
