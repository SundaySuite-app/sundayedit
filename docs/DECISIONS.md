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

## ADR-010 — PixiJS 8 fed by hidden `<video>` is the compositor; WebCodecs is deferred

**Status:** Accepted (2026-08-10) — OSS-integrasjonsprogram E5 (spike)

ADR-009 deferred "a real-time WebCodecs compositor, gated behind a runtime
capability check". E5 was the spike that had to choose between two ways of
building it:

- **(A) PixiJS 8** compositing hidden `<video>` elements (Clypra's model, and
  the prerequisite for E6 — `@clypra-studio/engine` peer-depends on
  `pixi.js@^8`).
- **(B) WebCodecs** — `VideoDecoder` via bilibili/WebAV or mediabunny, decoding
  into frames we composite ourselves.

**Decision:** the compositor is **PixiJS 8 fed by hidden `<video>` elements**.
WebCodecs is **not** adopted as the frame source now; it is kept as a targeted
upgrade for frame-exactness, behind the same capability gate. The current
`<video>`-plus-canvas preview stays the fallback and stays the default until
E6 gives the GPU compositor something to do.

### What was measured, and where

Target runtime probed directly in **macOS WKWebView** — the engine Tauri
actually uses — with a throwaway Swift harness (`wkrun.swift`) that hosts a
bare `WKWebView` and posts results back over a script message handler. Machine:
macOS 26.5.2 (25F84), Safari/WebKit 26.5.2, Apple Silicon, WebGL renderer
reported as "Apple GPU". Scene: two 1080p30 H.264 layers, top layer scaled
0.55 / rotated 8° / alpha 0.85, into a 1920×1080 target, paced at 30 fps.

**WKWebView supports WebCodecs.** `VideoDecoder`, `VideoEncoder`,
`AudioDecoder`, `AudioEncoder`, `VideoFrame`, `EncodedVideoChunk` all present;
`VideoDecoder.isConfigSupported` returns true for `avc1.42E01E`, `avc1.640028`,
`hvc1.1.6.L120.90` and `vp09.00.10.08` at 1080p, and `VideoEncoder` for
`avc1.640028`. Absent: `ImageDecoder`, `MediaStreamTrackProcessor`,
`SharedArrayBuffer` (`crossOriginIsolated` false), `performance.memory`.
WebGL2 and WebGPU are both available. So availability is not the reason
WebCodecs loses — it is available, and it still is not worth taking yet.

| WKWebView, 2×1080p30 + transform | C: `<video>`+canvas2d (no dep) | A: PixiJS 8 **as shipped** | A′: PixiJS 8 **+ UA fix** | B: WebAV `MP4Clip`+canvas2d |
| -------------------------------- | ------------------------------ | -------------------------- | ------------------------- | --------------------------- |
| startup → first composited frame | 53 ms                          | 481 ms                     | 139 ms                    | 79 ms                       |
| sustained fps (target 30)        | 30.0, 0 late                   | **20.2, 43 late**          | 30.0, 0 late              | 30.0, 0 late                |
| composite mean / p95             | 0.5 / 1 ms                     | **24.3 / 25 ms**           | 0.4 / 1 ms                | 0.1 / 1 ms                  |
| random seek mean / p95           | 12.2 / 22 ms                   | 63 / 73 ms                 | 15.1 / 27 ms              | **65.1 / 125 ms**           |
| ±1 frame step, mean              | 2.2 ms                         | 52.8 ms                    | 3.4 ms                    | ~0 ms (prefetched)          |
| peak WebContent RSS              | 37 MB                          | 354 MB                     | 61 MB                     | 93 MB (10 s clip)           |
| bundle cost (min+gzip)           | 0                              | 157.2 kB                   | 157.2 kB                  | 45.7 kB                     |

### The finding that decided it

PixiJS **as shipped is 12× too slow in our runtime, for a reason that is one
config line to fix.** Pixi's `glUploadVideoResource` passes
`forceAllocation = isSafari()`, and `isSafari()` is a userAgent regex
(`/^((?!chrome|android).)*safari/i`). Tauri's WKWebView reports
`Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko)`
— **no `Safari` token** — because wry only calls `setCustomUserAgent` when
`user_agent` is configured, and `src-tauri/tauri.conf.json` does not set it. So
Pixi's WebKit workaround never fires and every frame uploads via
`texSubImage2D`.

Measured on raw WebGL, uploading both 1080p layers, drawing them and forcing
completion with a 1-pixel `readPixels` each frame:

| WKWebView, per frame, 2 layers    | Chromium (headless, SwiftShader) |
| --------------------------------- | -------------------------------- |
| `texImage2D(video)` — 0.69 ms     | 10.02 ms                         |
| `texSubImage2D(video)` — 28.92 ms | 10.16 ms                         |

A **42×** cliff in WebKit; no difference at all in Chromium. This is why
Chromium benchmarking would have missed it entirely. Pinning
`forceAllocation = true` on Pixi's video uploader took the same scene from
25.64 ms to 0.59 ms per frame; setting a UA that carries a `Safari` token gets
the same result without touching Pixi internals.

Two further measurements removed the usual argument for (B):

- **The frame source is not the bottleneck.** In the same hand-rolled WebGL
  compositor, uploading from `<video>` cost 1.82 ms/frame and uploading from a
  WebCodecs `VideoFrame` cost 1.86 ms/frame. WebCodecs buys frame-exactness,
  not throughput.
- **WebAV's decoded-frame cache does not scale for free.** Walking a 60 s 1080p
  clip end to end: `MP4Clip` peaked at 235 MB RSS, the `<video>` element at
  48 MB — per clip, on a multi-track timeline.

### Consequences

- E6 is unblocked on its own terms: `@clypra-studio/engine` gets the `pixi.js@^8`
  it peer-depends on, and the compositor holds 30 fps at 1080p with two layers
  and 24 ms of headroom per frame.
- **We must own the WKWebView upload workaround, and guard it.** Preferred form:
  set `app.windows[].userAgent` in `src-tauri/tauri.conf.json` to a string that
  ends in a `Safari/605.1.15` token plus our own product token (validated: 0.4 ms
  composite, 30 fps, 61 MB). It is public config rather than Pixi internals — but
  it is _implicit_, so it needs a comment pointing here and a startup assertion
  that fails loudly if a Pixi frame costs more than a frame budget. The
  alternative (overriding `renderer.texture._uploads.video` to force allocation)
  is explicit but reaches into a private field.
- Preview stays a **fallback ladder**, per ADR-009: no WebGL2 → current
  `<video>`+canvas path; WebGL2 → Pixi compositor; neither ever becomes the
  export truth. ffmpeg `filter_complex` remains authoritative (ADR-009 is
  unchanged by this ADR).
- No dependency was added by the spike. `package.json` and `package-lock.json`
  are byte-identical to before it; pixi.js, `@webav/av-cliper` and mp4box were
  installed in a scratch project outside the repo and removed with it.

### What was NOT measured (and must not be reported as if it were)

- **Frame fidelity.** Latency and throughput were measured; whether the
  `<video>` element lands on the _exact_ frame the timeline asked for was not.
  That is the real argument for WebCodecs and it is still open.
- **The shipped binary.** All WKWebView numbers come from a bare `WKWebView`
  in `wkrun.swift` served over `http://127.0.0.1`, not from SundayEdit.app over
  the `tauri://` protocol. The UA claim is read from wry 0.55.1 + our config, not
  observed in the running app — verify it in the app before relying on it.
- **`@clypra-studio/engine` itself.** Not installed, not benchmarked. Its 233
  effects are filters over render textures, but nothing here proves they keep
  the fast upload path.
- **Chromium as a performance reference.** The Playwright run was headless with
  SwiftShader (software GL); its absolute numbers mean nothing. It is cited
  only to show the `texSubImage2D` cliff is WebKit-specific.
- **Windows/Linux.** WebView2 and WebKitGTK were not probed at all.
- **Audio.** Out of scope here; the clock and A/V drift belong to E2.
- Timing granularity: WebKit clamps `performance.now()` to ~1 ms, so sub-ms
  figures are means over 90–150 frames, never single-frame truth.

### What would change this decision

1. **E6 turns out not to need Pixi** — if the curated effect subset is small
   enough to express as our own shaders, control C (no dependency, 0.5 ms
   composite, 37 MB) beats both candidates on every axis measured here, and the
   157 kB and the WebKit workaround buy nothing. _On the evidence in this ADR,
   preview performance alone does not justify a GPU compositor; E6 does._
2. **Frame-exactness fails a real test** — if `<video>` seeking proves not
   frame-accurate for our footage, WebCodecs becomes the frame source feeding
   the same Pixi compositor (measured: same upload cost), with a bounded frame
   cache instead of `MP4Clip`'s.
3. **Pixi fixes `isSafari()` upstream**, or starts detecting WebKit by feature
   rather than UA — then the workaround is deleted and the guard test becomes a
   regression test for the upstream fix.
4. **We ship to Windows/Linux** — WebView2 and WebKitGTK must be measured
   before the capability gate opens there.
5. **HDR / >2 layers / 4K** — everything here is 1080p, SDR, two layers.

### Addendum — the user-agent fix as shipped (2026-08-10, E6)

The precondition above is now in the tree. `src-tauri/tauri.macos.conf.json`
sets

```
Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.0 Safari/605.1.15 SundayEdit
```

Four things about it are deliberate:

- **The stock WKWebView shape is preserved**; a `Version/17.0 Safari/605.1.15`
  token and a bare product token are _appended_. We do not impersonate a
  different platform or engine — `AppleWebKit/605.1.15` is what this webview
  genuinely is. `Version/17.0` is cosmetic (only the `Safari/` token drives
  `isSafari()`) and is deliberately conservative rather than tracking the host's
  real Safari version, which would go stale in the config on every macOS
  release.
- **macOS only.** Tauri merges `tauri.<platform>.conf.json` over the base file,
  so Windows/WebView2 keeps its own Chromium UA — where the table above measured
  no difference between the two upload paths (10.02 vs 10.16 ms) and where a
  macOS UA would simply be false.
- **Risk, stated plainly:** a UA is public, and some sites and servers branch on
  it. In SundayEdit that risk is near zero and worth naming precisely — the
  **webview makes no external network requests at all**. Every HTTP call
  (Whisper model download, Claude/OpenAI/AssemblyAI/Deepgram) is made by Rust
  `reqwest`, which has its own user agent and is untouched by this setting. The
  string is therefore seen by our own local asset protocol and by PixiJS's
  feature check, and by nothing else. If a future feature ever loads third-party
  web content into the webview, revisit this.
- **It is guarded**, because a config string that silently controls a 42×
  performance cliff is exactly the kind of implicit dependency that rots:
  `src-tauri/tests/webview_user_agent.rs` reimplements Pixi's `isSafari()`
  regex, asserts the configured UA satisfies it, asserts the base config does
  NOT set one, and asserts the macOS window definition has not drifted from the
  base one (platform config merging REPLACES arrays — a property added to the
  base file would otherwise vanish on macOS).

Still not verified, and still listed above under "what was NOT measured": the
UA as observed in the running SundayEdit.app over `tauri://`. The config is
correct and the guard is honest about what it checks — a rig test remains the
only proof that wry applies it end to end.

## ADR-011 — Protected gaps ride on `TimelineItem.locked`; no gap entity

**Status:** Accepted (2026-08-10) — OSS-integrasjonsprogram E3

The gap engine (`detect_gaps` / `insert_gap_with_ripple` / `remove_gap_with_ripple` / `pack_track` in `src-tauri/src/services/timeline_ops.rs`) needs a notion of a **protected gap**: empty time a ripple must not consume, so the material downstream keeps its timecode.

The tempting model is a persisted gap entity (`ProtectedGap { track_id, start_ms, end_ms }` in a new table) — and it is the wrong one. Gaps have no independent identity: they are defined entirely by the clips around them, so a persisted gap has to be re-derived, migrated, and garbage-collected on every single timeline edit. That is a whole class of drift bugs bought for nothing.

**Decision:** protection is _derived, never stored_. The marker is the existing, already-persisted `TimelineItem.locked` flag: **the gap immediately before a locked clip is protected**, because closing or shrinking past it would move a clip the user deliberately pinned. `Gap.protected` is computed by `detect_gaps` at query time.

Consequences:

- **No schema migration.** `SCHEMA_VERSION` stays at 4; no new table, no new column, no backfill.
- **No new UI vocabulary.** The lock affordance already exists on clips and already means "do not move this". Ripple protection is the same promise, honoured by more operations.
- **Nothing to garbage-collect.** Delete the locked clip and the protection disappears with it, automatically and correctly.
- Gaps are exposed as a _query_ (`timeline_detect_gaps`), not as project state — the frontend never has to keep a gap list in sync.
- Cost: a gap cannot be protected without a clip after it to anchor the protection, and the two ideas cannot be separated later without revisiting this ADR. Both are acceptable — a trailing gap has nothing to protect (the track simply ends), and no UX has asked for "pin this emptiness but let the clip after it slide".

The ripple semantics that follow: a shift **stops at** a protected gap. Clips before it absorb as much of the shift as the gap has headroom (clamp, don't reject — a fully pinned track is a no-op, never an error), and the locked clip never moves. `pack_track` treats locked clips as anchors: everything upstream packs left, so the protected gap survives and grows, and the clips after the anchor pack against the anchor's end instead of sliding past it.

## ADR-012 — Timeline caches are addressed on a fixed grid per zoom tier

**Status:** Accepted (2026-08-10) — OSS-integrasjonsprogram E3

Filmstrips (and, later, waveforms) are expensive to render and cheap to reuse — but only if the cache key is stable. Keyed by "the range currently visible at the current zoom", every scroll and every zoom step invalidates everything, because two viewports never line up twice.

**Decision:** cached timeline artefacts are addressed on an **absolute grid per zoom tier**, defined once in `src-tauri/src/services/tiles.rs` and shared by every consumer:

- tier `t` has span `TILE_BASE_SPAN_MS >> t` (64 s at tier 0 down to 250 ms at tier 8),
- tile `i` covers `[i*span, (i+1)*span)`, anchored at timeline zero — never at the viewport,
- 64 s is chosen so every tier halves _exactly_, with no integer-division rounding, which is what makes tiles nest: tile `i` at tier `t` is precisely tiles `2i` and `2i+1` at tier `t+1`.

Consequences:

- **Scrolling reuses tiles** — panning only asks for the tiles that entered the view.
- **Zooming reuses the centre** — because the boundaries nest, an already-rendered tier-`t` tile is a valid coarse stand-in for its two children while they render (`parent_tile` / `child_tiles`).
- The scheme is media-agnostic on purpose. `services::video::filmstrip_tile_args` / `extract_filmstrip_tile` consume it today; the waveform cache adopts the same grid later rather than inventing a second one. (This is the one place the upstream inspiration applied the idea to video and forgot its own waveform cache — we do not repeat that.)
- `tile_key(tier, index)` is opaque and filename-safe; `tile_file_name(media_key, …)` scopes it by content hash, so a moved file keeps its rendered tiles.

## ADR-013 — A curated effect registry, not an effect library

**Status:** Accepted (2026-08-10) — OSS-integrasjonsprogram E6

E6's brief was "install `@clypra-studio/engine`, mount it on the compositor,
curate a starting subset". We installed `pixi.js@^8` (the compositor ADR-010
chose) and **did not install the effect engine**.

The reason is the architecture invariant, not the dependency's quality:
**ffmpeg `filter_complex` is the export truth** (ADR-009). An effect catalogue
of 233 GPU filters is 229 ways for the preview to promise something the export
cannot deliver — and "what gets exported matches what the user saw in preview"
is a product promise, not an implementation detail. A library of preview-only
effects would have to be hidden behind a second gate anyway; the gate is the
real deliverable, so we built the gate and skipped the library.

**Decision:** clip effects are a **curated registry** — a small list of effects
that BOTH the Pixi preview and the ffmpeg export can produce, defined twice
(`src-tauri/src/services/effects.rs`, `src/features/timeline/effects/
registry.ts`) and pinned against each other by
`src-tauri/tests/effects_registry_parity.rs`.

| id           | params                    | ffmpeg              | Pixi                 |
| ------------ | ------------------------- | ------------------- | -------------------- |
| `brightness` | `amount` −1…1 (default 0) | `eq=brightness=<a>` | colour-matrix offset |
| `contrast`   | `amount` 0…3 (default 1)  | `eq=contrast=<a>`   | colour-matrix scale  |
| `saturation` | `amount` 0…3 (default 1)  | `eq=saturation=<a>` | luma-preserving mix  |
| `grayscale`  | —                         | `hue=s=0`           | saturation 0         |

Three rules keep the seam shut, each with a test on both sides:

1. **Unknown kinds emit nothing.** A `kind` outside the registry is inert in
   the export and unselectable in the UI — never an invented filter name that
   aborts the render, which is precisely how the transition picker broke before
   (ADR-010's sibling guard, `compose_xfade_vocabulary.rs`).
2. **Neutral emits nothing.** An enabled effect at its default produces the
   identical filtergraph to no effect at all.
3. **Out of range clamps, never rejects** — the `timeline_ops` house rule.

Consequences:

- **No schema change.** `TimelineItem.effects` was already in the model
  (`Effect { id, kind, params, enabled }`); the registry narrows what may go in
  it. Old project files keep loading; an unrecognised effect renders as it
  always did (as nothing).
- **The Pixi side is an approximation, and says so.** `vf_eq` works in YUV
  (`v = contrast*(v−0.5) + 0.5 + brightness` on luma, chroma scaled about 128);
  a WebGL colour matrix works in RGB. The matrices use the same formulas with
  Rec.601 weights, and the inspector prints the exact ffmpeg fragment the clip
  will export with, so the user reads the truth rather than trusting the
  approximation. Exactness, when it is needed, comes from the preview-render
  proxy — real ffmpeg (ADR-009 §3).
- **Parity is tested against real ffmpeg, not against a string.**
  `effects_ffmpeg_parity.rs` renders each effect through the real
  `build_filter_complex` and MEASURES the output with `signalstats` (YAVG /
  SATAVG) against a flat colour fixture, so a filter that parses but does
  nothing — or does the opposite — fails.
- **Effect order is colour → geometry.** Effects are applied before
  `transform_filters` in the item chain, and before the sprite transform in the
  preview. After the opacity mixer the stream is RGBA and a luma filter no
  longer means what the slider said.
- **An enabled effect leaves the simple burn-in path**, even a neutral one
  (`is_pristine_primary_item` still refuses any enabled effect). Deliberately
  stricter than "emits a filter": the general composite path is correct for
  everything, and the fast path should only ever be taken when it is provably
  identical.
- **Cost of the two implementations:** they can drift. That is bought back by
  the parity test, which reads the TypeScript literal out of the actual source
  — the same technique `compose_xfade_vocabulary.rs` uses on the transition
  picker.

### The compositor flag

The Pixi compositor itself (`src/features/timeline/compositor/`) is **off by
default** and gated twice: a persisted user setting (`localStorage`, next to
the locale) AND a runtime capability probe (WebGL2 with
`failIfMajorPerformanceCaveat` — a SwiftShader context is slower than the
`<video>` path it would replace). The two are stored separately on purpose: an
automatic fallback must not rewrite the user's setting, or the toggle appears
to flip itself and the user never learns why.

- With the flag off, MediaPlayer renders exactly the pre-E6 markup — asserted
  literally in `MediaPlayer.compositor.test.tsx`, because a stray wrapper or
  inline style on the default path is the failure no behavioural test notices.
- `pixi.js` is behind `React.lazy` + a dynamic import, so the renderer chunk
  (~247 kB gzip for the full barrel — more than the 157 kB ADR-010 measured for
  a trimmed build) is never fetched by a user who has not opted in.
- MediaPlayer keeps owning the `<video>`: the compositor never seeks, plays,
  pauses or loads it. Pixi's `VideoSource` defaults (`autoPlay`/`autoLoad`) are
  both switched off, and the texture is refreshed explicitly once per rendered
  frame — the element is usually paused (the reconcile loop scrubs it), and a
  paused video fires no frame callbacks.
- What the compositor buys today: the preview finally shows a clip's
  **transform and effects**, which the `<video>` path cannot. What it does not
  model: `crop`, and stacked layers (one element, one texture) — both reported
  as `unsupported` by `describeScene` rather than silently diverging.

## ADR-014 — We ship macOS + Windows only; Linux-only advisories are not product vulnerabilities

**Status:** Accepted (2026-08-30)

`release.yml` builds exactly two platforms:

```yaml
matrix:
  include:
    - platform: macos-latest
      args: "--target universal-apple-darwin --features whisper,llm"
    - platform: windows-latest
```

There is no Linux release job, and no Linux artifact has ever left this repo.
`ci.yml`'s only Linux runner (`web:`) runs Node — it never invokes cargo. All
Rust checks run on `macos-latest`.

This has a standing consequence for dependency triage, which is why it is
written down rather than rediscovered each time.

**`Cargo.lock` is cross-platform; the shipped dependency graph is not.** The
lockfile records the union of every target's resolution, so scanners flag
crates we never compile. Tauri's Linux backend (GTK/WebKitGTK) is the big one:
`glib`, `gtk`, `atk`, `gdk`, `gio`, `pango`, `soup3`, `webkit2gtk`,
`javascriptcore-rs`. None of them exist on our targets.

Verify before acting on such an advisory, from `src-tauri/`:

```
cargo tree -i <crate> --target aarch64-apple-darwin  --edges normal
cargo tree -i <crate> --target x86_64-pc-windows-msvc --edges normal
cargo tree -i <crate> --target x86_64-unknown-linux-gnu --edges normal
```

`warning: nothing to print.` on a target means the crate is absent there. If it
is absent from both shipped targets, the advisory does not describe a hole in
the product, and the alert is dismissed as **not used** with that evidence —
not silently ignored. Keeping such alerts open is itself a hazard: the noise
hides the next real finding.

Worked example — GHSA-wrw7-89jp-8q8g (`glib` 0.18.5, unsoundness in
`VariantStrIter`'s `Iterator`/`DoubleEndedIterator` impls), dismissed
2026-08-30:

- absent on `aarch64-apple-darwin` and `x86_64-pc-windows-msvc`
- present only on `x86_64-unknown-linux-gnu`, via
  `glib ← atk/gdk/gio ← gtk 0.18.2 ← tauri 2.11.5`
- unfixable regardless: `gtk 0.18.2` requires `glib = "^0.18"`, so
  `cargo update -p glib` locks 0 packages. glib 0.20 needs Tauri itself to move
  off the gtk 0.18 stack — upstream's call, and worth nothing to us until we
  ship Linux.

**If we ever add a Linux target, this ADR is void** and the whole GTK stack
must be re-triaged as shipped code.
