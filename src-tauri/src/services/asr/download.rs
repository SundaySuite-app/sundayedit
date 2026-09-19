//! Whisper model download — Phase 2.1 / 9.2.
//!
//! Models are too large to bundle (75 MB – 3 GB), so the installer ships
//! without them and the chosen ggml model is fetched on first run from the
//! Hugging Face `whisper.cpp` repo (the URL lives in `model::info().url`).
//!
//! The download streams to a `<file>.part` sibling and is renamed into place
//! only after the byte count is verified, so an interrupted or truncated
//! download never leaves a half-written model that `is_downloaded()` would
//! treat as valid. Progress is reported per chunk and the loop is
//! cancellable via a shared flag.
//!
//! The pure helpers (`download_fraction`, `verify_download`, `part_path`) are
//! unit-tested; the streaming `download_model` itself needs the network.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use super::model::WhisperModel;
use crate::error::{AppError, AppResult};

/// Streamed to the UI as a model download progresses.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../src/lib/bindings/DownloadProgress.ts")]
pub struct DownloadProgress {
    #[ts(type = "number")]
    pub downloaded_bytes: u64,
    /// Total size from the server's Content-Length, or null if it didn't
    /// send one (then the bar is indeterminate).
    #[ts(type = "number | null")]
    pub total_bytes: Option<u64>,
    /// 0..1 when the total is known, else null.
    pub fraction: Option<f32>,
}

impl DownloadProgress {
    fn at(downloaded: u64, total: Option<u64>) -> Self {
        Self {
            downloaded_bytes: downloaded,
            total_bytes: total,
            fraction: download_fraction(downloaded, total),
        }
    }
}

/// Completion fraction, or `None` when the total is unknown / zero.
pub fn download_fraction(downloaded: u64, total: Option<u64>) -> Option<f32> {
    match total {
        Some(t) if t > 0 => Some((downloaded as f64 / t as f64) as f32),
        _ => None,
    }
}

/// Sanity-check a finished download. When the server gave a Content-Length
/// the bytes on disk must match it exactly; otherwise we at least require a
/// non-trivial size so a truncated response or an HTML error page is
/// rejected rather than saved as a "model".
pub fn verify_download(written: u64, content_length: Option<u64>) -> AppResult<()> {
    match content_length {
        Some(expected) if written != expected => Err(AppError::Network(format!(
            "incomplete download: got {written} of {expected} bytes"
        ))),
        None if written < 1_000_000 => Err(AppError::Network(format!(
            "download too small ({written} bytes) — not a model file"
        ))),
        _ => Ok(()),
    }
}

/// The temp path a download streams into before being renamed into place.
pub fn part_path(final_path: &Path) -> PathBuf {
    let mut p = final_path.as_os_str().to_owned();
    p.push(".part");
    PathBuf::from(p)
}

/// Download `model` into `models_dir`, reporting progress and honouring
/// `cancel`. No-op (returns the existing path) if already downloaded.
pub async fn download_model(
    model: WhisperModel,
    models_dir: &Path,
    cancel: &AtomicBool,
    mut on_progress: impl FnMut(DownloadProgress),
) -> AppResult<PathBuf> {
    let final_path = model.path_in(models_dir);
    if model.is_downloaded(models_dir) {
        return Ok(final_path);
    }
    std::fs::create_dir_all(models_dir)?;

    let url = model.info().url;
    let mut resp = crate::services::http::client()
        .get(&url)
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(|e| AppError::Network(e.to_string()))?;
    let total = resp.content_length();

    let tmp = part_path(&final_path);
    let _ = std::fs::remove_file(&tmp); // drop any stale partial
    let mut file = std::fs::File::create(&tmp)?;

    let mut downloaded: u64 = 0;
    on_progress(DownloadProgress::at(0, total));

    loop {
        if cancel.load(Ordering::Relaxed) {
            drop(file);
            let _ = std::fs::remove_file(&tmp);
            return Err(AppError::Network("download cancelled".into()));
        }
        match resp
            .chunk()
            .await
            .map_err(|e| AppError::Network(e.to_string()))?
        {
            Some(bytes) => {
                file.write_all(&bytes)?;
                downloaded += bytes.len() as u64;
                on_progress(DownloadProgress::at(downloaded, total));
            }
            None => break,
        }
    }
    file.flush()?;
    drop(file);

    if let Err(e) = verify_download(downloaded, total) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    std::fs::rename(&tmp, &final_path)?;
    Ok(final_path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fraction_with_known_total() {
        assert_eq!(download_fraction(50, Some(100)), Some(0.5));
        assert_eq!(download_fraction(0, Some(100)), Some(0.0));
        assert_eq!(download_fraction(100, Some(100)), Some(1.0));
    }

    #[test]
    fn fraction_is_none_without_total() {
        assert_eq!(download_fraction(50, None), None);
        assert_eq!(download_fraction(50, Some(0)), None);
    }

    #[test]
    fn verify_rejects_incomplete_against_content_length() {
        assert!(verify_download(50, Some(100)).is_err());
    }

    #[test]
    fn verify_accepts_exact_content_length() {
        assert!(verify_download(100, Some(100)).is_ok());
    }

    #[test]
    fn verify_rejects_tiny_download_without_content_length() {
        // A 500-byte body is almost certainly an error page, not a model.
        assert!(verify_download(500, None).is_err());
    }

    #[test]
    fn verify_accepts_large_download_without_content_length() {
        assert!(verify_download(2_000_000, None).is_ok());
    }

    #[test]
    fn part_path_appends_suffix() {
        assert_eq!(
            part_path(Path::new("/models/ggml-base.bin")),
            PathBuf::from("/models/ggml-base.bin.part"),
        );
    }
}
