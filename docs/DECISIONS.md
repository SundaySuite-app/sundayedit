# Architecture Decision Records — SundayEdit

## ADR-001 — Tauri 2, matching SundayStage

**Status:** Accepted (2026-05-28)

Same stack as SundayStage (Tauri 2 + Rust + React + TS). One toolchain serves multiple products. Tauri's small footprint matters for a tool that also runs ffmpeg + Whisper locally — we want the app shell to be lean so the heavy lifting has headroom.

## ADR-002 — Pure-function caption operations

**Status:** Accepted (2026-05-28)

Every caption edit (split, merge, edit-word, retime, shift) is a pure function `(&Project, params) -> Result<Project>`. Never mutates in place.

Consequences:

- Undo is trivial: keep the previous `Project`.
- Operations are exhaustively unit-testable without a database or UI.
- Invariants (`Project::validate`) run after every operation, so corrupt state can never be persisted.

The renderer holds project state in the Zustand `useProjectStore`
(`src/lib/useProjectStore.ts`): snapshot undo/redo (each op replaces the whole
`Project`; undo keeps the previous snapshot) and a compare-and-swap commit so
concurrent op arrivals can't clobber each other. TanStack Query is used only
for read-only backend queries (export/style presets, model registry, cloud
providers, settings status) — never for project state. The Rust layer is
stateless for operations; the same operations feed the SQLite writer
(`services/project_file.rs`).

## ADR-003 — Confidence as first-class, calibrated empirically

**Status:** Accepted (2026-05-28)

Per-word confidence (0–100) is stored on every `Word` and drives the 4-tier highlight system. This is the product's killer feature.

Tier boundaries (85 / 70 / 50) were fitted by the calibration pass — see
`docs/CALIBRATION.md` for the headline recall/precision numbers and their
provenance (currently a reproducible modelled 1500-word dataset; the harness
refits identically when hand-labelled real recordings replace it). The
boundary logic lives in ONE place — `Word::confidence_tier()` in Rust,
mirrored by `confidenceTier()` in TS — so re-calibration changes one constant
set.

Locked or edited words are always tier 1 (no highlight) regardless of score — once a human has touched a word, we trust them.

## ADR-004 — i64 timestamps emit `number` in TS bindings

**Status:** Accepted (2026-05-28)

Rust uses `i64` milliseconds. ts-rs defaults to emitting `bigint` for i64, but Tauri's serde_json serializes i64 as a JSON number, and JS receives a `number` at runtime. The wire format is `number`, so the binding must say `number`.

We override per-field with `#[ts(type = "number")]`. JS `number` safely represents any video duration in ms (2^53 ms ≈ 285,000 years).

## ADR-005 — Local Whisper default, cloud opt-in

**Status:** Accepted (2026-05-28) — implementation pending Phase 2

The privacy + cost story. Video never leaves the machine unless the user explicitly enables a cloud provider (with a consent dialog). API keys go in the OS keychain via the `keyring` crate, never plaintext.

## ADR-006 — Stay a captioning tool; refuse scope creep

**Status:** ~~Accepted (2026-05-28)~~ **Superseded by ADR-007 (2026-07-15)**

SundayEdit is the world's best captioning tool, not the world's second-best video editor. No cuts/transitions/color-grading beyond what captions strictly need (the filler/silence ripple-edit in Phase 7.2 is the one deliberate exception, because it directly serves caption timing). Say no to 80% of feature requests in the first 12 months.

_Superseded: the strict "captioning tool only" boundary is replaced by ADR-007, which deliberately grows SundayEdit into a multi-track NLE while keeping captions first-class and the confidence flagship intact._

## ADR-007 — Evolve into a multi-track NLE; captions are a track type

**Status:** Accepted (2026-07-15) — supersedes ADR-006

The market pulled us past the caption-only line: users want to trim, arrange, and overlay footage in the same tool where they caption it. We now build a pragmatic multi-track non-linear editor.

The non-negotiable constraint: **the flagship is preserved.** Captions become one of four track kinds (`TrackKind` = `Video` / `Audio` / `Caption` / `Overlay` in `src-tauri/src/model.rs`), and confidence highlighting (ADR-003) is untouched — the NLE is built _around_ the caption pipeline, not on top of it.

Consequences:

- New model types: `MediaItem` (imported source), `Track` (a lane), `TimelineItem` (a clip placed on a track, with `in_ms`/`out_ms`/`timeline_start_ms`/`speed`/`transform`/`effects`/`transition_in`/`text`). All live in `model.rs` with ts-rs bindings under `src/lib/bindings`.
- `Project` gains `media` / `tracks` / `timeline_items` (all `#[serde(default)]` so old files load), guarded by `Project::validate_timeline()`.
- The pure-function operation model (ADR-002) extends unchanged: timeline edits are `(&Project, params) -> Result<Project>`, and snapshot undo still just keeps the previous `Project`.

## ADR-008 — OTIO-_shaped_ JSON model in-repo; no OTIO bindings

**Status:** Accepted (2026-07-15)

The timeline data model is deliberately **shaped like** OpenTimelineIO (media references, tracks, clips with source vs. timeline ranges) so the concepts are familiar and a future OTIO import/export adapter is straightforward.

But we do **not** take an OTIO dependency. The Rust and JS OTIO bindings are immature (native build complexity, thin/unstable JS surface), and we already own a clean serde model with ts-rs parity. We keep our own in-repo types (`model.rs`) and can write a translation layer to/from OTIO later if a real interop need appears.

## ADR-009 — Pragmatic preview; final compositing at export

**Status:** Accepted (2026-07-15)

Real-time multi-track compositing in the browser is expensive and not yet portable. We stage it:

1. **Now:** HTML5 `<video>` element driven by the playhead clock + a canvas overlay for captions/graphics. Cheap, instant, good enough for editing decisions on a single dominant video layer.
2. **Export:** authoritative compositing via `ffmpeg` `filter_complex` — the multi-track timeline is lowered to a filtergraph and rendered once. This is the source of truth; preview only approximates it.
3. **Fallback:** for arrangements the `<video>`+canvas path can't show faithfully, a preview-render proxy (a fast, low-res ffmpeg render of the region) fills the gap.
4. **Deferred:** a real-time WebCodecs compositor, gated behind a runtime capability check, so we only use it where the browser actually supports it and fall back cleanly otherwise.

This keeps "what you export matches what you saw" (Tech principles) honest: export is the ground truth, and preview fidelity improves in stages without blocking the NLE.
