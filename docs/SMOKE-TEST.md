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

Rows marked ☐ are P2c — see `docs/NEEDS-RICHARD.md` for what Richard needs to
supply (model download, API keys, a real video) and the exact commands.
