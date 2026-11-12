//! Whisper model registry — Phase 2.1.
//!
//! Models are NOT bundled in the installer (each is 75 MB – 3 GB). On
//! first run the user picks a size; we download the chosen ggml model
//! from Hugging Face to the app data dir. This module is the catalog +
//! path resolution; the actual download is a Tauri command (with progress).

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use ts_rs::TS;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "kebab-case")]
#[ts(export, export_to = "../../src/lib/bindings/WhisperModel.ts")]
pub enum WhisperModel {
    Tiny,
    Base,
    Small,
    Medium,
    LargeV3,
    LargeV3Turbo,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../src/lib/bindings/WhisperModelInfo.ts")]
pub struct WhisperModelInfo {
    pub model: WhisperModel,
    /// ggml filename, e.g. "ggml-large-v3-turbo.bin".
    pub filename: String,
    /// Approx download size in MB — shown so users on slow links can choose.
    pub approx_mb: u32,
    /// Download URL (Hugging Face ggml repo).
    pub url: String,
    /// One-line tradeoff description for the picker.
    pub description: String,
    /// Whether it handles non-English well.
    pub multilingual: bool,
    /// The size we recommend by default.
    pub recommended: bool,
}

impl WhisperModel {
    pub fn all() -> Vec<WhisperModel> {
        use WhisperModel::*;
        vec![Tiny, Base, Small, Medium, LargeV3, LargeV3Turbo]
    }

    pub fn filename(&self) -> &'static str {
        use WhisperModel::*;
        match self {
            Tiny => "ggml-tiny.bin",
            Base => "ggml-base.bin",
            Small => "ggml-small.bin",
            Medium => "ggml-medium.bin",
            LargeV3 => "ggml-large-v3.bin",
            LargeV3Turbo => "ggml-large-v3-turbo.bin",
        }
    }

    pub fn info(&self) -> WhisperModelInfo {
        use WhisperModel::*;
        const BASE_URL: &str = "https://huggingface.co/ggerganov/whisper.cpp/resolve/main";
        let (approx_mb, description, multilingual, recommended) = match self {
            Tiny => (
                75,
                "Fastest. Rough drafts, quick tests. Lower accuracy.",
                true,
                false,
            ),
            Base => (142, "Fast. Acceptable for clean English.", true, false),
            Small => (466, "Good balance for most short videos.", true, false),
            Medium => (
                1500,
                "High accuracy. Slower; needs a capable machine.",
                true,
                false,
            ),
            LargeV3 => (2960, "Best raw accuracy. Slowest.", true, false),
            // Turbo is the sweet spot: near-large accuracy, ~5× faster.
            LargeV3Turbo => (
                1620,
                "Recommended. Near-large accuracy at ~5× the speed.",
                true,
                true,
            ),
        };
        WhisperModelInfo {
            model: *self,
            filename: self.filename().to_string(),
            approx_mb,
            url: format!("{BASE_URL}/{}", self.filename()),
            description: description.to_string(),
            multilingual,
            recommended,
        }
    }

    /// The default recommendation for a first-time user.
    pub fn default_recommended() -> WhisperModel {
        WhisperModel::LargeV3Turbo
    }

    /// Resolve where a downloaded model lives inside the app data dir.
    pub fn path_in(&self, models_dir: &Path) -> PathBuf {
        models_dir.join(self.filename())
    }

    /// Has the model been downloaded already?
    pub fn is_downloaded(&self, models_dir: &Path) -> bool {
        self.path_in(models_dir).is_file()
    }
}

/// Full catalog for the model-picker UI.
pub fn catalog() -> Vec<WhisperModelInfo> {
    WhisperModel::all().iter().map(|m| m.info()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_has_all_models() {
        let c = catalog();
        assert_eq!(c.len(), 6);
    }

    #[test]
    fn exactly_one_recommended() {
        let recommended: Vec<_> = catalog().into_iter().filter(|m| m.recommended).collect();
        assert_eq!(recommended.len(), 1);
        assert_eq!(recommended[0].model, WhisperModel::LargeV3Turbo);
    }

    #[test]
    fn default_recommendation_is_turbo() {
        assert_eq!(
            WhisperModel::default_recommended(),
            WhisperModel::LargeV3Turbo
        );
    }

    #[test]
    fn urls_point_to_ggml_repo() {
        for info in catalog() {
            assert!(
                info.url
                    .starts_with("https://huggingface.co/ggerganov/whisper.cpp")
            );
            assert!(info.url.ends_with(".bin"));
        }
    }

    #[test]
    fn path_and_download_check() {
        let dir = tempfile::tempdir().unwrap();
        let m = WhisperModel::Base;
        assert!(!m.is_downloaded(dir.path()));
        std::fs::write(m.path_in(dir.path()), b"fake model").unwrap();
        assert!(m.is_downloaded(dir.path()));
    }

    #[test]
    fn filenames_are_distinct() {
        let names: std::collections::HashSet<_> =
            WhisperModel::all().iter().map(|m| m.filename()).collect();
        assert_eq!(names.len(), 6, "every model has a distinct filename");
    }
}
