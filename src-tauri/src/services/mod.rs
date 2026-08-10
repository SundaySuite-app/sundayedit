//! Business logic services. Each module is pure (functional) where
//! possible — input state, output new state, no shared mutation.
//!
//!   - `operations`   — caption split/merge/edit/shift (Phase 3.1)
//!   - `export`       — SRT/VTT/ASS writers (Phase 6.1)
//!   - `video`        — ffprobe metadata, format validation, content hash (Phase 1.1)
//!   - `project_file` — .sundayedit SQLite save/load (Phase 1.1)
//!   - `waveform`     — audio extraction + multi-zoom peaks (Phase 1.2)
//!   - `asr`          — speech recognition: confidence, models, cloud, local (Phase 2)
//!   - `glossary`     — post-transcription auto-correction (Phase 3.4)
//!   - `style_presets`— bundled subtitle style presets (Phase 5.1/5.3)
//!   - `burnin`        — ffmpeg libass burn-in command (Phase 6.2)
//!   - `export_presets`— platform export presets + validation (Phase 6.3)
//!   - `find_replace`  — find & replace + bulk ops (Phase 7.3)
//!   - `filler`        — filler/silence detection + ripple cuts (Phase 7.2)
//!   - `reflow`        — CPS/line-length/line-count readability analysis + timing repair (Phase 7)
//!   - `llm`           — Claude API client + AI polish/suggest/translate (Phase 4.1/4.3/7.1)
//!   - `diarize`       — speaker diarization + roster management (Phase 4.2)
//!   - `tiles`         — fixed-grid tile addressing for filmstrip/waveform caches (E3)

pub mod asr;
pub mod burnin;
pub mod compose;
pub mod deeplink;
pub mod diarize;
pub mod document;
pub mod export;
pub mod export_presets;
pub mod filler;
pub mod find_replace;
pub mod glossary;
pub mod highlight_reel;
pub mod llm;
pub mod operations;
pub mod project_file;
pub mod reflow;
pub mod secrets;
pub mod style_presets;
pub mod tiles;
pub mod timeline_ops;
pub mod video;
pub mod waveform;
