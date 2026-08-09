# SundayEdit

AI-assisted captioning grown into a pragmatic multi-track video editor. Desktop-native (Tauri 2), local-first, macOS (universal Intel + Apple Silicon) and Windows. Standalone product — own brand. Optional (never required) integration with [SundayRec](https://github.com/SundaySuite-app/sundayrec).

> **Status:** v0.7.0 — multi-track NLE on top of the captioning flagship, plus a hardening round (22 adversarially confirmed bugs fixed, each with a regression test). ~635 Rust unit/integration tests, 318 vitest, 49 Playwright e2e, 18 real-ffmpeg integration tests. See `CHANGELOG.md`.

## Two genuine innovations

### #1 — Confidence highlighting

Every word gets a confidence score from the ASR model. SundayEdit shows them as colour-coded highlights (4 tiers, with accessibility underlines). The words the AI is sure about, you don't touch. You fix only the ones that light up amber or red. **Human review at 10× speed.** The tier boundaries are fitted by a calibration harness, not guessed — see `docs/CALIBRATION.md`.

### #2 — Context priming + glossary

Tell SundayEdit what the video is about before transcribing. Whisper biases recognition toward your names, jargon, and foreign words. "Han snakker om kerigma" becomes "Han snakker om kerygma" with no manual correction. Glossary terms can be suggested by AI from the transcript or extracted from reference documents.

## What works today

**Captioning (the flagship):**

- Local Whisper (feature-gated `whisper-rs`, in-app model download) + cloud providers (OpenAI / AssemblyAI / Deepgram — BYOK, keys in the OS keychain, explicit upload consent, off by default)
- Caption editor: confidence highlighting, inline word edit, alternate-picker, lock, focus mode, Tab review, undo/redo
- Context priming + glossary with auto-correction, AI term suggestion, reference-document extraction
- AI polish, diarization (sidecar-gated), translation, filler/silence removal with ripple, find & replace
- Export: SRT, VTT, ASS, TXT, JSON, DOCX + **burn-in to MP4** via ffmpeg/libass, platform presets, pre-render validation
- Visual style editor: presets, live WYSIWYG preview (mirrors the ASS burn-in), 9-grid anchoring, safe-area guide

**Multi-track NLE (v0.7.0):**

- Four track kinds — Video / Audio / Caption / Overlay. Captions are a first-class track; the confidence pipeline is untouched (ADR-007).
- Media bin with import, thumbnails, drag-to-place; timeline with trim/move/split-at-playhead/ripple-delete, snap, transitions (real ffmpeg `xfade` vocabulary), transforms, speed
- Export composites the whole timeline via a pure-function-built ffmpeg `filter_complex` (progress events + cancel); track enabled/muted/solo honored
- Pragmatic preview: HTML5 `<video>` + canvas overlay, with a low-res preview-proxy render fallback (ADR-009)
- Sermon highlight-reel studio: batch clips per platform

**Everywhere:** 7 locales (en, no, sv, da, de, fr, pl — full catalogs), onboarding, deep-link integration (`sundayedit://import` + captions hand-back), signed/notarized release pipeline with auto-update.

## Competitive positioning

|                         | Premiere Pro | Descript    | CapCut       | **SundayEdit**          |
| ----------------------- | ------------ | ----------- | ------------ | ----------------------- |
| Price                   | $23/mo       | $24/mo      | Free-ish     | **~$9/mo Pro**          |
| Focus                   | Everything   | Doc + video | TikTok-first | **Captions-first NLE**  |
| Confidence highlighting | No           | No          | No           | **Yes (calibrated)**    |
| Context priming         | No           | No          | No           | **Yes**                 |
| Works offline           | Partial      | No          | No           | **Yes (local Whisper)** |
| Video never uploaded    | —            | Uploads     | Uploads      | **Local by default**    |

## Stack

- **Tauri 2** (Rust) + React 19 + TypeScript + Tailwind v4 — same toolchain as SundayStage
- **whisper-rs** for local speech recognition (feature-gated)
- **ffmpeg** sidecar for probe, audio extraction, thumbnails, burn-in, and the multi-track compose export
- **SQLite** project files (in-code schema, `SCHEMA_VERSION 4`) — see `docs/ARCHITECTURE.md`
- **ts-rs** auto-generates the TypeScript bindings from the Rust models (76 binding files, wire-format-correct: `number` not `bigint` for i64 ms)
- **Zustand** `useProjectStore` with snapshot undo/redo holds editor state; TanStack Query only for a few read-only queries

## Quickstart

```bash
npm install
npm run tauri dev          # builds Rust + opens the app
```

## Development

```bash
npm run check              # lint + typecheck + vitest + clippy + cargo test
npm run test               # vitest (318)
npm run test:rust          # cargo test (~635)
npm run test:e2e           # Playwright (49)

cd src-tauri
cargo test --lib export_bindings   # regenerate TS bindings
```

The 18 real-ffmpeg integration tests (compose against actual media) are `#[ignore]`d by default; run them with ffmpeg on PATH via `cargo test -- --ignored` (some need `SUNDAYEDIT_TEST_VIDEO`).

## Documentation

- `docs/ARCHITECTURE.md` — data model, NLE timeline model, flow, phase status
- `docs/DECISIONS.md` — ADRs (Tauri, pure-function ops, calibration, NLE evolution, preview strategy)
- `docs/CALIBRATION.md` — how the confidence tiers were fitted
- `docs/DISTRIBUTION.md` — release pipeline (signed/notarized, universal macOS + Windows)
- `docs/SMOKE-TEST.md` + `docs/NEEDS-RICHARD.md` — native-only manual verification rows
- `docs/integration.md` — deep-link contract with SundayRec

## License

TBD — likely a source-available commercial license, given the standalone commercial intent.
