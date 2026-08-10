# SMOKE-TEST.md — SundayEdit

Manual end-to-end runs that the automated suite cannot cover, because they need
real hardware, a real model, real API keys, or the GUI. Each row is a single
real-world run a human performs once per release (or when the relevant code
changes). The pure logic each one exercises is already unit-tested; these rows
verify the _wiring_ against the real world.

Status legend: ☐ not yet run · ✅ verified · ⚠️ ran with issues (note them)

## ASR — Phase 2 (transcription seams)

| #   | Area                       | What to do                                                                                                                        | Expected                                                                                                                                                 | Status                                                                      |
| --- | -------------------------- | --------------------------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------- |
| A1  | Local Whisper (real model) | Build `cargo build --features whisper`, download a model in-app (e.g. `large-v3-turbo`), run transcription on a short real video. | Captions appear with per-word confidence; amber/red highlights land on genuinely-uncertain words. No crash; GPU (Metal/CUDA) used when available.        | ☐ HARDWARE-UNVERIFIED                                                       |
| A2  | Local Whisper (no feature) | Default build (no `--features whisper`); attempt local transcription.                                                             | Clear, actionable error: "this build does not include local transcription — rebuild with `--features whisper`, or configure a cloud provider." No panic. | ✅ covered by unit test `local::stub_returns_actionable_error`              |
| A3  | OpenAI Whisper API         | Add a real OpenAI key in Settings → API keys; consent to upload; transcribe a clip < 25 MB.                                       | Verbose-JSON parsed into captions; backend shows "OpenAI Whisper"; word timings present; segment-level confidence applied.                               | ☐ NETWORK-UNVERIFIED                                                        |
| A4  | OpenAI oversized upload    | With OpenAI selected, point at audio > 25 MB.                                                                                     | Pre-upload `validation` error naming the 25 MB cap and suggesting local Whisper — _before_ any network call.                                             | ✅ covered by unit test `check_upload_size_message_is_clear_and_actionable` |
| A5  | AssemblyAI API             | Add a real AssemblyAI key; transcribe a clip.                                                                                     | Upload→request→poll loop completes; backend "AssemblyAI"; real per-word confidence drives the highlight tiers.                                           | ☐ NETWORK-UNVERIFIED                                                        |
| A6  | Deepgram API               | Add a real Deepgram key; transcribe a clip.                                                                                       | Single POST returns; backend "Deepgram"; punctuated words used; per-word confidence drives tiers.                                                        | ☐ NETWORK-UNVERIFIED                                                        |
| A7  | Empty/missing key          | Select any cloud provider with no key configured.                                                                                 | Fast `validation` error pointing at Settings → API keys; no file read, no network call.                                                                  | ✅ covered by unit test `wired_providers_reject_empty_key`                  |
| A8  | Cross-backend tier parity  | Transcribe the same clip locally and via a cloud provider.                                                                        | A word the model is equally (un)sure about lands in the same highlight tier across backends (confidence curve is shared).                                | ☐ NETWORK/HARDWARE-UNVERIFIED                                               |

## NLE — v0.7.0 (native-only wiring)

The timeline ops, compose builder, and store logic are exhaustively
unit/integration-tested (including 18 real-ffmpeg tests), but the full GUI
round-trip needs the native app (`npm run tauri dev`) and a real video file.

| #   | Area                      | What to do                                                                                          | Expected                                                                                                                                            | Status |
| --- | ------------------------- | --------------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------- | ------ |
| N1  | Import → NLE backfill     | Import a real video into a fresh project.                                                           | Media bin shows the clip AND the timeline has a video + caption track with one placed clip (`backfill_default_timeline`) — no empty lanes.          | ☐      |
| N2  | Thumbnails                | After N1, look at the media bin and the placed clip.                                                | Source-frame thumbnails appear (cached under appCacheDir/thumbnails); audio-only media keeps the kind icon.                                         | ☐      |
| N3  | Trim / split / move       | Drag clip edges, press B (or the inspector button) at the playhead, drag clips along/between lanes. | Trim clamps against neighbours without sliding content; split is lossless (also at speed ≠ 1); moves snap; undo/redo restores each step.            | ☐      |
| N4  | Compose export            | Export the multi-track timeline; mid-render, try cancel once, then run a full render.               | Progress advances (no hang at 0 %), cancel stops the render cleanly, full render produces an MP4 that plays with the expected clips/captions/audio. | ☐      |
| N5  | Preview proxy             | Trigger the preview-proxy render (arrangement the `<video>` path can't show faithfully).            | A low-res proxy renders and plays in the preview; editing afterwards invalidates the stale proxy.                                                   | ☐      |
| N6  | Track flags in export     | Disable one track, mute another, solo an audio track; export.                                       | Disabled track absent from the picture, muted track silent, solo isolates audio — export matches the flag state.                                    | ☐      |
| N7  | Remove-track/media guards | Try removing a track that still holds clips, and a media item still referenced by the timeline.     | Both are rejected with a clear surfaced message (no silent no-op, no crash); removal succeeds after the references are gone.                        | ☐      |

## GPU preview + clip effects — E6 (real WebKit only)

The compositor cannot run in the automated suite at all: jsdom has no WebGL2
and Playwright runs headless Chromium, so **every claim ADR-010 makes about
performance is only proven inside the real macOS WKWebView**. These rows are
the proof. Run them in `npm run tauri dev` (or a built app) on a real video.

| #   | Area                     | What to do                                                                                                        | Expected                                                                                                                                                             | Status |
| --- | ------------------------ | ----------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------ |
| E6a | User agent applied       | Open the devtools console in the running app and read `navigator.userAgent`.                                      | It ends with `Version/17.0 Safari/605.1.15 SundayEdit` — i.e. wry actually applied `tauri.macos.conf.json`. If not, the compositor will be ~42× slower per frame.    | ☐      |
| E6b | Flag off = no change     | With the GPU preview OFF (default), play, scrub and shuttle a project.                                            | The preview behaves exactly as in v0.7.0. No canvas in the DOM, no `pixi` chunk in the network panel.                                                                | ☐      |
| E6c | Flag on, healthy machine | Settings → Preview → enable the GPU compositor. Play a 1080p clip.                                                | The picture keeps playing (now drawn on a canvas), audio unaffected, karaoke captions still render ON TOP, and it holds 30 fps.                                      | ☐      |
| E6d | Transform + effects live | With the flag ON, scale/move/rotate a clip and add brightness/contrast/saturation/black & white in the inspector. | The preview shows them immediately — this is the whole point of the compositor. Compare against an export of the same frame: same direction and roughly same amount. | ☐      |
| E6e | Effects reach the export | With the flag OFF, add each curated effect and export.                                                            | The exported MP4 shows the effect (the export never depended on the preview path).                                                                                   | ☐      |
| E6f | Fallback is invisible    | Force a failure (disable hardware acceleration, or run over a remote session) with the flag ON.                   | The preview falls back to `<video>` with no black frame and no crash; Settings shows the "unavailable" note and the checkbox stays where you left it.                | ☐      |
| E6g | Toggle mid-session       | Turn the flag on and off a few times while playing.                                                               | No leaked WebGL contexts (the app keeps working after ~10 cycles), no audio glitch, playhead stays in sync.                                                          | ☐      |

## Seam-round fixes — E8 (things the suite proved, that only a human can see)

Each row below is a bug that WAS shipped in the E1–E6 work and is now fixed
with a regression test. The tests prove the logic; these rows prove the pixels.
Full write-up: `docs/OSS-PROGRAM-REPORT.md` §3.

| #   | Area                       | What to do                                                                                      | Expected                                                                                                                                                                            | Status |
| --- | -------------------------- | ----------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------ |
| E8a | Filmstrip appears unaided  | Open a project with a video clip and DO NOTHING — don't scroll, zoom or edit.                   | Source frames fill the clip box within a couple of seconds, on their own. (Before the fix the paint list was memoized on inputs that never changed when a tile finished rendering.) | ☐      |
| E8b | Coarse stand-in placement  | Zoom in quickly on a long clip and watch while the finer tiles render.                          | The placeholder is a blockier image in the RIGHT place — not the same image repeated and squeezed into each slot, and no bright band where several copies overlapped.               | ☐      |
| E8c | "Preview is approximate"   | With the GPU flag ON: crop a clip; then stack two clips on two video tracks under the playhead. | A small badge bottom-left names what the preview is not drawing (crop / only the top clip). It DISAPPEARS again when the crop or the stack is removed.                              | ☐      |
| E8d | Karaoke preview vs burn-in | Enable karaoke ("sweep"), burn in the same caption, and step a line frame by frame in both.     | The lit word changes on the SAME frame all the way through the line, not just on the first word — the `\k` ladder is cumulative, so a late drift is the failure mode.               | ☐      |
| E8e | Ladder never costs export  | Export once while playing back, and once parked.                                                | Both files have identical resolution and bitrate — the preview quality ladder only ever touches the preview proxy.                                                                  | ☐      |

Rows marked ☐ are P2c — see `docs/NEEDS-RICHARD.md` for what Richard needs to
supply (model download, API keys, a real video) and the exact commands.
