# Third-Party Notices

SundayEdit is built on top of several open-source projects. This file lists
code that has been vendored (copied and adapted into this repository's source
tree, as opposed to installed as an npm/cargo dependency), along with the
originating project, its license, and what changed in the port.

---

## Clypra

- **Project**: [Clypra](https://github.com/AIEraDev/Clypra)
- **License**: MIT
- **Vendored from commit**: `2e85676f0c56d1e5f28fabcd9a3ab9952442a35b`
- **Copyright**: Copyright (c) 2026 Clypra Contributors

Four pure modules were lifted and adapted (OSS-INTEGRATION-PROGRAM stage
E1 — see `docs/OSS-INTEGRATION-PROGRAM.md`). All are new files with no
behavior change to any existing SundayEdit code path; each file's header
comment repeats this attribution and documents its specific adaptation.

| SundayEdit file                            | Clypra source path                              | What changed                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                   |
| ------------------------------------------ | ----------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `src/lib/animation.ts`                     | `src/core/evaluation/animation.ts`              | Dropped `evaluateVisualPropertyKeyframes` (duplicated the generic evaluator against a Clypra-specific type SundayEdit doesn't have). Otherwise a verbatim port — pure keyframe/easing math with no clip/store coupling.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                        |
| `src/features/timeline/gizmoCalculator.ts` | `src/components/editor/transform/calculator.ts` | Re-derived against SundayEdit's fractional-of-frame `Transform` model (`x, y, scale, rotation_deg` — no independent width/height), instead of Clypra's absolute-pixel `Clip` box (`x, y, width, height`). Consequences: uniform-scale-only (aspect is always locked, by construction of the data model — no aspect-lock toggle), the text-auto-height branch was dropped (out of scope for pure geometry), and the center/edge snap guide (`snapCenterToCanvas`) is an original pure-function implementation inspired by, not lifted from, Clypra's React-stateful `TransformOverlay.tsx` snap UX. The 8-handle resize math, rotation-delta-with-snap, and rotation-aware cursor mapping are ported near-verbatim.                                                                                                                                                                                                                                                                                                                                                                                                                                             |
| `src/features/timeline/playbackClock.ts`   | `src/core/playback/PlaybackClock.ts`            | Ported to SundayEdit's millisecond time domain (upstream is seconds; seconds now survive only inside the `ClockTimeSource` boundary that reads `AudioContext.currentTime`). The AudioContext and `requestAnimationFrame` are injected instead of newed up/global, so the clock is testable headless. Upstream's forward-only 0.1–4× speed became our signed shuttle rate (reverse plays to 0 exactly as forward plays to the duration); `setRate` re-anchors instead of doing a pause→play cycle that frame-snapped the playhead. Fixed two upstream defects: the clock froze while the AudioContext was still suspended (we ride `performance.now()` until the audio clock runs, then adopt it without a jump), and a missed `completeSeek()` wedged playback (the seek freeze is now opt-in via `seek(ms, { hold: true })`). The generation counter is bumped on every transport change, not only `play()`. The module-level singleton (`getPlaybackClock`) was dropped. Kept intact: AudioContext-derived time, generation-guarded ticks, ~10 fps throttled UI notification vs imperative reads, frame-boundary snap on pause/seek, and stall compensation. |
| `src/features/timeline/mediaReconcile.ts`  | `src/core/playback/PreviewPlaybackScheduler.ts` | Only the decide/execute _shape_ was taken: a pure `reconcileMedia(timeline, snapshots) → MediaAction[]` with a reason tag per action, plus upstream's ±2% `playbackRate` nudge for sub-frame drift. All tolerances are SundayEdit's own: the drift budget is one frame at the project fps while playing and half a frame while pinned (upstream: 0.5–2 s), and upstream's 400 ms/1500 ms seek rate-limits are replaced by a state-based guard (no second seek while the element still reports `seeking`). The nudge is applied to audio-bearing elements too, which upstream excludes. Times are milliseconds; transitions, prewarm targets and per-clip source times are resolved by the caller, so the module has no dependency on the project model. Coexists with the existing `src/features/timeline/mediaSync.ts` (untouched, still in use) as its E2 successor.                                                                                                                                                                                                                                                                                         |

### MIT License text (Clypra)

```
MIT License

Copyright (c) 2026 Clypra Contributors

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```

### Planned, not yet vendored

The OSS-integration program's later stages (E4/E5/E6 — see
`docs/OSS-INTEGRATION-PROGRAM.md`) call for pulling in additional Clypra
dependencies that are **not part of this stage** and have no code in this
repository yet:

- **jassub** (subtitle rendering) — **NOT vendored, and not currently planned
  for default builds.** Its npm metadata declares
  `LGPL-2.1-or-later AND (FTL OR GPL-2.0-or-later) AND MIT AND
MIT-Modern-Variant AND ISC AND NTP AND Zlib AND BSL-1.0` — it bundles
  libass, FreeType and fribidi, so it is **not MIT** (an earlier draft of
  `docs/OSS-INTEGRATION-PROGRAM.md` said so in error). Adopting it in a
  proprietary build carries LGPL obligations (replaceable/dynamically linked
  component, written offer of source, attribution). Stage E4a therefore ships
  karaoke captions with **no new dependency** — `\k` tags generated into our
  own ASS output plus our own canvas renderer — and jassub stays an explicit
  owner decision (E4b).
- **mediabunny** (media demux/decode, MPL-2.0) — candidate for stage E5, not
  yet vendored or installed.
- **@clypra-studio/*** packages (MIT, requires `pixi.js@^8`) — candidate for
  stage E6, gated on the E5 compositor decision (ADR-010). Not yet installed.

This section will be updated with license text and vendoring details when
each of those lands.

---

## ffmpeg / ffprobe

SundayEdit bundles `ffmpeg`/`ffprobe` as Tauri sidecar binaries (not vendored
source — pulled at build time via `scripts/fetch-ffmpeg.mjs`). Licensing
details, including the GPL/LGPL compliance note for public releases, are
already documented in `docs/DISTRIBUTION.md` (see "ffmpeg sidecar (wired)")
and are not duplicated here.
