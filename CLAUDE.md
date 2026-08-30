# CLAUDE.md — SundayEdit

SundayEdit is a desktop AI captioning application. Mac and Windows, equal first-class support. Standalone product, own brand. Optional integration with Sunday Account.

## Target user

A content creator, video editor, podcaster, educator, journalist, marketer, or church volunteer who:

- Records or edits video with spoken content
- Currently uses Premiere Pro, Descript, CapCut, or Veed.io for captions
- Is frustrated by either price (Adobe), bloat (Descript), or quality (free tools)
- Wants control: desktop performance, offline capability, no required cloud upload

## Core promises

1. **Faster than any competitor from raw video to broadcast-ready captions** — because the AI does 92% of the work and tells you exactly where to look for the 8% it's unsure about.
2. **Context-aware:** tell SundayEdit what the video is about, and accuracy on names, jargon, foreign words goes way up.
3. **Privacy-first:** local Whisper by default. Cloud transcription is optional, off by default, requires explicit consent.
4. **Affordable:** 1/3 the price of Premiere Pro Creative Cloud.
5. **Polished export:** SRT, VTT, ASS, plus burn-in to MP4 with full styling.

## The two genuine innovations

### #1 — Confidence highlighting

Every word gets a confidence score from the ASR model. SundayEdit shows them to you as colour-coded highlights in the editor. The 92% the AI is sure about, you don't touch. You fix only the 8% that are amber. **This makes human review 10× faster.**

### #2 — Context priming + glossary

Before transcription you tell SundayEdit what the video is about: names of people, technical terms, foreign words. Whisper biases recognition toward these terms. The same video goes from "Han snakker om kerigma og frelse" to "Han snakker om kerygma og frelse" without manual correction.

## Competitive positioning

- vs **Premiere Pro:** focused product, 1/3 the price, AI-native from the start
- vs **Descript:** not a "doc pretending to be a video editor" — we are a captioning tool, no scope creep
- vs **CapCut:** professional output for professional work; we are not TikTok-first
- vs **Veed.io / Kapwing:** desktop-native performance, no upload required, works offline

## Tech principles

- **Local-first.** Cloud features are opt-in.
- **The video file never leaves the user's machine** unless they explicitly request cloud transcription.
- **AI is in service of human judgment** — we surface uncertainty, we don't hide it.
- **The editor must feel instant.** No spinners during normal editing.
- **Output quality is non-negotiable** — what gets exported must match what the user saw in preview, pixel-perfect.

## Stack

- **Tauri 2** (Rust backend) + React 19 + TypeScript + Tailwind v4
- **whisper-rs** (whisper.cpp Rust bindings) for local speech recognition
- **ffmpeg** sidecar binary for video I/O + burn-in rendering
- **SQLite** via `sqlx` for project files
- **shadcn/ui** primitives, customized — dark-first, professional (think Linear, Final Cut)
- **TanStack Query** (server state) + **Zustand** (UI state)

## Out of scope for v1

- Color grading
- Motion graphics / keyframe animation authoring
- A plugin/effect SDK for third parties

## NLE architecture

SundayEdit is evolving from a caption-only editor into a pragmatic multi-track
NLE, without diluting the flagship. The shape:

- **Multi-track timeline.** Four track kinds — `Video`, `Audio`, `Caption`,
  `Overlay` (`TrackKind` in `src-tauri/src/model.rs`). Captions remain a
  **first-class track type**, and the confidence-highlighting pipeline (killer
  #1) is untouched — the NLE grows around it, not over it.
- **Pragmatic preview.** HTML5 `<video>` + a canvas overlay drives the live
  preview now; final compositing is done by `ffmpeg` `filter_complex` at export.
  A real-time WebCodecs compositor is deferred behind a capability check (see
  ADR-009), with a preview-render proxy fallback in between.
- **State in one store.** All editor state lives in a single Zustand
  `useProjectStore` (`src/lib/useProjectStore.ts`) with snapshot undo/redo —
  each op replaces the whole `Project`, and undo just keeps the previous
  snapshot (`.run` / `.undo` / `.redo`, `selectCanUndo` / `selectCanRedo`).

See `docs/DECISIONS.md` (ADR-007/008/009) and `docs/ARCHITECTURE.md` for the
data model (`MediaItem` / `Track` / `TimelineItem`).

## Repository layout

```
src/                  React frontend
├── app/              Route/page-level components
├── features/         Feature modules (project, transcribe, editor, style, export)
├── components/       Shared UI primitives
├── lib/              Utilities, hooks, IPC client, types
└── styles/           Globals, design tokens

src-tauri/            Rust backend
└── src/
    ├── commands/     Tauri command handlers
    ├── services/     Business logic (operations, export, ASR, project_file, …)
    ├── model.rs      Project data model (Project, Caption, Word, MediaItem,
    │                 Track, TimelineItem, Transform, Effect, Transition, …)
    ├── error.rs
    └── lib.rs

docs/                 Architecture, decisions, calibration data
tests/fixtures/       Test video files (Creative Commons)
```

> **Note:** there are no SQL migration files and no `db/` Rust module. The
> SQLite schema is created in-code in `services/project_file.rs::ensure_schema`
> (`SCHEMA_VERSION = 5`), which also backfills a default video + caption track
> when opening older files.

## Project file format

`.sundayedit` files are SQLite databases — decided and shipped
(`services/project_file.rs`, `SCHEMA_VERSION = 5`, in-code schema creation +
in-code migrations; a `tracks_persisted` meta marker distinguishes genuine
v≤3 files, so load-time timeline backfill never resurrects a deliberately
emptied timeline). Containing:

- Reference to source video (absolute path + content hash for path stability)
- Project settings + metadata
- Caption data (with per-word timing + confidence)
- Style data
- Context + glossary
- NLE state: media pool, tracks, timeline items
- Clips / highlight-reel plans

## Calibration discipline

Confidence highlighting is the killer feature. It MUST be calibrated empirically:

- Take 10 real transcripts, label every word manually as correct/incorrect
- Plot precision/recall against confidence thresholds
- Choose tier boundaries based on real data
- Document in `docs/CALIBRATION.md` so reviewers (and us) trust the numbers
