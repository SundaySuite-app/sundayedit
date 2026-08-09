# Changelog

All notable changes to SundayEdit are documented here.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).
(This file starts at 0.5.1 — the first broadly published release; earlier
history lives in the git log and `docs/ARCHITECTURE.md`'s phase table.)

## [Unreleased]

### Fixed

- Night bug-hunt: **22 adversarially confirmed findings fixed**, each with a
  regression test. Highlights:
  - Fresh import now backfills the NLE state (media item + video/caption
    tracks + one placed clip) — the media bin and lanes are no longer empty on
    first import; load-time backfill only fires for genuine v≤3 files
    (`tracks_persisted` marker), so a deliberately emptied timeline stays
    empty on reload.
  - Compose export honors `Track.enabled` / `muted` / `solo`; the simple-path
    export gained progress events + cancel (previously hung at 0 % and
    ignored cancel); odd source dimensions are evened up for libx264 yuv420p.
  - Transition picker offers only real ffmpeg `xfade` names; legacy kinds
    normalize and self-heal — proven by an integration test composing every
    picker option with real ffmpeg.
  - Timeline ops: trim clamps against neighbours without sliding content,
    slow-motion split keeps spans lossless (f64), overlapping add places into
    the gap; `timeline_end_ms` computed in f64 with TS-parity guarded.
  - Store/interaction: compare-and-swap project commits (no concurrent
    clobbering), latest-wins for in-flight op arrivals, latched
    "video unavailable" overlay resets on source change, drag pointercancel
    aborts cleanly, stale preview proxy invalidated on edit, drag-drop import
    listener race/leak fixed, shuttle keys ignore modifier combos.

### Added

- Deferred NLE UI surface: clip/media-bin **thumbnails** (per-media cache),
  **split at playhead** (B key + inspector button), **ripple-delete**
  (Delete), **remove track / remove media** with backend rejections surfaced
  in the UI.
- NLE i18n completed: the media-bin/track/inspector keys translated across all
  7 locales (sv/da/de/fr/pl were falling back to English).

### Changed

- Render-efficiency pass (measured): Timeline ruler/lane-headers/lanes are
  memoized subtrees — per playhead tick only the timecode, playhead line, and
  MediaPlayer reconcile; redundant caption-vec clones removed in Rust ops;
  `validate_timeline` guarded by a 5000-clip stress test.
- Dependencies: npm/cargo/actions minor-patch groups bumped; `quinn-proto`
  0.11.14 → 0.11.16 (RUSTSEC advisory); `zip` 2 → 4, `sqlx` 0.8 → 0.9,
  `ts-rs` 11 → 12.

Gates: vitest 318 · cargo ~635 (clippy `-D warnings`) · Playwright 49/49 ·
18 real-ffmpeg integration tests.

## [0.7.0] — 2026-07-15

### Added

- **Multi-track NLE** (ADR-007/008/009): four track kinds
  (Video/Audio/Caption/Overlay) — captions stay first-class and the
  confidence-highlighting pipeline is untouched. Media pool + media bin,
  timeline lanes with drag/trim/snap, clip inspector, transitions,
  transforms, speed; 15 pure-function timeline ops mirroring the caption-op
  contract.
- **Compose export**: the timeline is flattened by a pure-function-built
  ffmpeg `filter_complex` (per-item trim/transform/overlay, `xfade`
  transitions, `amix` audio, ASS caption layer last) with progress + cancel.
- **Preview**: real `<video>` bound to the playhead clock + canvas overlay,
  with a low-res preview-proxy render fallback.
- **Sermon highlight-reel studio**: batch clips per platform (#11), batch
  burn-in moved to a blocking thread (#12).
- **Universal macOS binary** — Intel + Apple Silicon in one DMG, with lipo'd
  ffmpeg/ffprobe sidecars (#13).
- Deep-link: canonical sunday-contracts `MediaHandoff` superset (#9).

### Fixed

- Local transcription: Metal GPU, UI-freeze, real progress, cancel (#10).

## [0.6.0] — 2026-06-13

> ⚠️ The `v0.6.0` tag exists but the release build **failed to publish** — no
> binaries shipped for this version. Everything below reached users with
> v0.7.0.

### Added

- Sunday Account: read the shared cross-app account session; `sunday-auth`
  via git tag so CI builds standalone (#5, #8).
- Deep-link: echo the recording path back in the captions hand-back (#6).

### Fixed

- Export audit (#2): comma field-injection, hex panic, retime overflow,
  silent save.
- Find & replace: zero-width/no-op edge cases + launch-time failure toasts (#3).
- Contiguous VTT cue numbering + dead filler entries dropped (#7).
- CI: mac notarization re-enabled via a fresh app-specific password (#4).

## [0.5.1] — 2026-06-09

Last published release before 0.7.0 (marked Latest).

### Added

- SundayEdit branding: official logo + icon, favicon/title, grouped dock-rail
  clusters, ⌘K command palette.

### Changed

- Version alignment + cleanup; dropped the "Verbatim" working title from
  remaining comments.
- CI: mac notarization temporarily disabled (stale app password) so the build
  could ship.

[Unreleased]: https://github.com/richardfossland/sundayedit/compare/v0.7.0...HEAD
[0.7.0]: https://github.com/richardfossland/sundayedit/compare/v0.6.0...v0.7.0
[0.6.0]: https://github.com/richardfossland/sundayedit/compare/v0.5.1...v0.6.0
[0.5.1]: https://github.com/richardfossland/sundayedit/compare/v0.5.0...v0.5.1
