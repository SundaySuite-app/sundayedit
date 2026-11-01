# SundayEdit — product vision & layout

Moved verbatim from CLAUDE.md (2026-11-01) to keep the always-loaded agent context small.

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

## Out of scope for v1

- Color grading
- Motion graphics / keyframe animation authoring
- A plugin/effect SDK for third parties

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

