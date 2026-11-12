//! Sunday-link deep-link import commands — Phase 8.
//!
//! Thin layer over `services::deeplink`. The native deep-link plugin (wired in
//! `lib.rs`) emits the raw URL to the renderer; the renderer calls
//! `deeplink_parse_import` to validate + structure it, then drives the normal
//! import + context/glossary seeding flow itself.

use crate::error::AppResult;
use crate::services::deeplink::{ImportRequest, captions_callback_url, parse_import_url};

/// Parse a `sundayedit://import?…` URL into a validated [`ImportRequest`].
#[tauri::command]
pub fn deeplink_parse_import(url: String) -> AppResult<ImportRequest> {
    parse_import_url(&url)
}

/// Build the hand-back URL (`<returnTo>://captions?path=…[&recording=…]`) for the
/// caller that launched us, pointing at a freshly written caption sidecar. When
/// `recording_path` is given (the media the caller sent us), it's echoed back so
/// the source app attaches the captions to the right recording. The renderer
/// opens the URL after a successful export so the source app can pick up the result.
#[tauri::command]
pub fn deeplink_captions_callback_url(
    return_to: String,
    sidecar_path: String,
    recording_path: Option<String>,
) -> AppResult<String> {
    captions_callback_url(&return_to, &sidecar_path, recording_path.as_deref())
}
