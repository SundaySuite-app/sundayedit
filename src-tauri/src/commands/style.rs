//! Style Tauri commands — Phase 5.
//!
//! The visual style editor lives in the renderer (CSS preview is a
//! webview job). Rust owns the preset catalog and the ASS conversion
//! (burn-in, Phase 6.2). The renderer's `styleToCss` mirrors the same
//! Style fields so preview ≈ burn-in.

use crate::services::style_presets::{StylePreset, catalog};

#[tauri::command]
pub fn style_list_presets() -> Vec<StylePreset> {
    catalog()
}
