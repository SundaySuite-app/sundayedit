# NEEDS-RICHARD.md — SundayEdit

Things that cannot be verified in the build/test sandbox because they need a
real model, real API keys, a real device, or the GUI (P2c). The code for each
is wired and compiles; the pure logic is unit-tested. These are the runs only
Richard can do. Mirror of the ☐ rows in `docs/SMOKE-TEST.md`.

## ASR — Phase 2

### Local Whisper on a real device (HARDWARE-UNVERIFIED)

- Needs: a C/C++ toolchain + cmake, a downloaded GGML model, a real machine.
- Build with the feature on:
  ```sh
  cargo build --manifest-path src-tauri/Cargo.toml --features whisper
  ```
  (or `npm run tauri build -- --features whisper` for the full app).
- Run a short real video through it and confirm SMOKE-TEST row **A1**:
  captions + sensible confidence highlights, GPU used when available, no crash.
- Default builds (no feature) correctly stub this with an actionable error —
  that path _is_ tested (`local::stub_returns_actionable_error`), so the only
  unverified part is the live whisper-rs invocation.

### Cloud providers — BYOK live calls (NETWORK-UNVERIFIED)

The three providers are fully wired. Only the live HTTP round-trips are
unverified; key guards, error-body extraction, MIME inference, upload-size
preflight, and every response→`Transcript` mapping are pure and unit-tested.

Provide one key per provider you want to verify (Settings → API keys; stored in
the OS keychain, never plaintext — env-var fallback shown for reference):

| Provider       | Env var (fallback)   | SMOKE row                               |
| -------------- | -------------------- | --------------------------------------- |
| OpenAI Whisper | `OPENAI_API_KEY`     | A3 (and oversized A4 is already tested) |
| AssemblyAI     | `ASSEMBLYAI_API_KEY` | A5                                      |
| Deepgram       | `DEEPGRAM_API_KEY`   | A6                                      |

For each: consent to upload, transcribe a short real clip, confirm the backend
label and that captions/confidence look right. For A8, transcribe the same clip
locally and via cloud and confirm a borderline word lands in the same highlight
tier (the confidence curve is shared across backends).

> Privacy reminder: cloud is OFF by default and the consent dialog must show
> before the first upload (`CloudProvider::consent_text`). The video/audio never
> leaves the machine unless the user explicitly chooses a cloud provider.

## NLE — v0.7.0 (GUI-only wiring)

- Needs: the native app (`npm run tauri dev`) + any real video file. No keys,
  no model.
- The logic is heavily tested headless (timeline ops, compose `filter_complex`
  builder, store CAS commits, 18 real-ffmpeg integration tests) — what's left
  is one native click-through of SMOKE-TEST rows **N1–N7**:
  1. **N1/N2** — import a real video: media bin + a populated video/caption
     lane appear (import backfill), thumbnails render.
  2. **N3** — trim/split (B at playhead)/move clips; undo/redo behaves.
  3. **N4** — compose export: progress advances, cancel works, the final MP4
     plays correctly.
  4. **N5** — preview-proxy render appears and is invalidated by the next edit.
  5. **N6** — track enabled/muted/solo flags are honored in the export.
  6. **N7** — removing a still-referenced track/media is rejected with a clear
     message in the UI.
