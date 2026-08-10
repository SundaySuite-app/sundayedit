//! Seam guard: the macOS WebView user agent ↔ PixiJS's WebKit upload workaround.
//!
//! ADR-010 (docs/DECISIONS.md) measured a **42×** cliff inside Tauri's
//! WKWebView: uploading a 1080p `<video>` frame costs 0.69 ms via
//! `texImage2D` and 28.92 ms via `texSubImage2D`. PixiJS already knows this —
//! `glUploadVideoResource` passes `forceAllocation = isSafari()` — but
//! `isSafari()` is a **userAgent regex**:
//!
//! ```js
//! /^((?!chrome|android).)*safari/i
//! ```
//!
//! and wry only calls `setCustomUserAgent` when the config sets one, so the
//! stock WKWebView UA
//! `Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko)`
//! — **no `Safari` token** — never matched. Pixi's own WebKit workaround was
//! switched off in the only runtime where it matters, and the compositor ran
//! at 20 fps instead of 30 with 24 ms of composite time per frame.
//!
//! The fix is one config line (`app.windows[].userAgent` in
//! `src-tauri/tauri.macos.conf.json`), which is exactly the problem: it is
//! **implicit**. Nothing in the compositor code says "this only performs
//! because of a string in a JSON file", and strict JSON cannot carry a comment
//! that says so. This file is that comment, with teeth.
//!
//! It also pins the two things that make the fix *scoped*:
//!
//!   - **macOS only.** The base `tauri.conf.json` sets NO `userAgent`, so the
//!     Windows build keeps WebView2's own (Chromium) UA — where ADR-010
//!     measured no difference between the two upload paths (10.02 vs 10.16 ms)
//!     and where claiming to be macOS Safari would be a plain lie.
//!   - **No drift.** Tauri merges platform config with RFC 7386 JSON Merge
//!     Patch (`json_patch::merge`), where an ARRAY IS REPLACED WHOLESALE — so
//!     `tauri.macos.conf.json` must restate the entire window definition, and
//!     any window property added to the base file would silently vanish on
//!     macOS. The test below asserts the two definitions differ by the
//!     `userAgent` key and nothing else.

use std::path::{Path, PathBuf};

use serde_json::Value;

fn conf_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf()
}

fn read_conf(name: &str) -> Value {
    let path = conf_dir().join(name);
    let raw =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    serde_json::from_str(&raw).unwrap_or_else(|e| panic!("parse {}: {e}", path.display()))
}

fn window(conf: &Value) -> &serde_json::Map<String, Value> {
    conf["app"]["windows"][0]
        .as_object()
        .expect("app.windows[0] is an object")
}

/// Faithful reimplementation of PixiJS's `isSafari()`
/// (`/^((?!chrome|android).)*safari/i`): case-insensitively, the first
/// occurrence of `safari` must come BEFORE any occurrence of `chrome` or
/// `android`.
fn pixi_is_safari(ua: &str) -> bool {
    let ua = ua.to_lowercase();
    let Some(safari_at) = ua.find("safari") else {
        return false;
    };
    let blocker_at = [ua.find("chrome"), ua.find("android")]
        .into_iter()
        .flatten()
        .min();
    match blocker_at {
        Some(b) => safari_at < b,
        None => true,
    }
}

#[test]
fn macos_window_user_agent_satisfies_pixis_webkit_check() {
    let ua = window(&read_conf("tauri.macos.conf.json"))
        .get("userAgent")
        .and_then(Value::as_str)
        .expect(
            "src-tauri/tauri.macos.conf.json must set app.windows[0].userAgent — \
             without a Safari token PixiJS uploads every preview frame via \
             texSubImage2D, which ADR-010 measured at 42× the cost in WKWebView",
        )
        .to_string();

    assert!(
        pixi_is_safari(&ua),
        "the configured UA must match PixiJS's isSafari() regex; got {ua:?}"
    );
    assert!(
        ua.contains("Safari/605.1.15"),
        "keep the REAL WebKit Safari build token so the UA stays a truthful \
         WebKit identity, not an invented browser; got {ua:?}"
    );
    assert!(
        ua.contains("AppleWebKit/605.1.15"),
        "keep the engine token the stock WKWebView UA carries; got {ua:?}"
    );
    assert!(
        ua.starts_with("Mozilla/5.0 (Macintosh; Intel Mac OS X"),
        "keep the stock WKWebView UA shape — we ADD a Safari token, we do not \
         impersonate another platform; got {ua:?}"
    );
    assert!(
        ua.contains("SundayEdit"),
        "carry our own product token so a server log can tell this app apart \
         from a browser; got {ua:?}"
    );
}

#[test]
fn base_config_leaves_the_user_agent_alone() {
    // Windows/WebView2 is Chromium: ADR-010 measured NO difference between the
    // two upload paths there, so there is nothing to buy and a macOS UA would
    // be a lie. Scope the override to the platform that needs it.
    assert!(
        window(&read_conf("tauri.conf.json"))
            .get("userAgent")
            .is_none(),
        "tauri.conf.json must NOT set a userAgent — the override is macOS-only \
         (see tauri.macos.conf.json)"
    );
}

#[test]
fn macos_window_definition_matches_the_base_one_apart_from_the_user_agent() {
    // Tauri merges platform config with JSON Merge Patch, where arrays are
    // REPLACED, not merged. So this is not a partial override: the macOS file
    // owns the whole window definition, and any property added to the base
    // file has to be copied across or it disappears on macOS.
    let base = window(&read_conf("tauri.conf.json")).clone();
    let mut mac = window(&read_conf("tauri.macos.conf.json")).clone();
    assert!(mac.remove("userAgent").is_some());
    assert_eq!(
        mac, base,
        "tauri.macos.conf.json's window definition drifted from tauri.conf.json's. \
         Platform config REPLACES the windows array — copy the new/changed \
         property across (or drop it from both)."
    );
}

#[test]
fn pixi_is_safari_mirror_behaves_like_the_regex() {
    // The predicate above is the whole reason the config line exists; if the
    // mirror is wrong, the guard above proves nothing.
    let stock_wkwebview =
        "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko)";
    assert!(
        !pixi_is_safari(stock_wkwebview),
        "the stock Tauri WKWebView UA is exactly the case ADR-010 found failing"
    );
    assert!(pixi_is_safari(
        "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 \
         (KHTML, like Gecko) Version/17.0 Safari/605.1.15"
    ));
    // Chrome ships a Safari token too — and is deliberately excluded.
    assert!(!pixi_is_safari(
        "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 \
         (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36"
    ));
    assert!(!pixi_is_safari(
        "Mozilla/5.0 (Linux; Android 13) AppleWebKit/537.36 (KHTML, like Gecko) \
         Version/4.0 Safari/537.36"
    ));
    assert!(!pixi_is_safari("Mozilla/5.0 (Windows NT 10.0; Win64; x64)"));
}
