# Distribution & auto-update (Phase 9.2)

SundayEdit ships as signed installers for macOS and Windows, with auto-update
over GitHub Releases. This doc is the checklist to make the pipeline live —
the code/config is already in place, what's left is **secrets and accounts**
only you can provide.

## How it works

1. You push a version tag (`vX.Y.Z`).
2. `.github/workflows/release.yml` builds on macOS (universal) and Windows,
   signs + notarizes (macOS), signs the updater bundle, and creates a
   **draft** GitHub Release with the installers and `latest.json`.
3. You review the draft and publish it.
4. Installed apps check `https://github.com/richardfossland/sundayedit/releases/latest/download/latest.json`
   on launch, verify the signature against the public key embedded in
   `tauri.conf.json`, and offer the update (the in-app banner).

The updater **public** key is committed in `tauri.conf.json`
(`plugins.updater.pubkey`). The matching **private** key was generated to
`~/.tauri/sundayedit_updater.key` (with an empty password) and is NOT in the
repo. Keep it safe — losing it means existing installs can no longer
auto-update (they'd need a manual reinstall with a new key).

## Required GitHub repository secrets

Settings → Secrets and variables → Actions → New repository secret.

### Updater (required — without these the release build fails)

| Secret                               | Value                                                                                                                                                              |
| ------------------------------------ | ------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| `TAURI_SIGNING_PRIVATE_KEY`          | Contents of `~/.tauri/sundayedit_updater.key`. Copy without echoing it: `cat ~/.tauri/sundayedit_updater.key \| pbcopy`, then paste.                               |
| `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` | The password for that key. It was generated with an **empty** password, so set this secret to an empty string (or regenerate the key with a password — see below). |

### macOS code signing + notarization (you already do this for SundayRec)

| Secret                       | Value                                                                                                                        |
| ---------------------------- | ---------------------------------------------------------------------------------------------------------------------------- |
| `APPLE_CERTIFICATE`          | Base64 of your "Developer ID Application" cert exported as `.p12`: `base64 -i cert.p12 \| pbcopy`.                           |
| `APPLE_CERTIFICATE_PASSWORD` | The password you set when exporting the `.p12`.                                                                              |
| `APPLE_SIGNING_IDENTITY`     | e.g. `Developer ID Application: Richard Fossland (TEAMID)`. Find it with `security find-identity -v -p codesigning`.         |
| `APPLE_ID`                   | Your Apple Developer account email.                                                                                          |
| `APPLE_PASSWORD`             | An **app-specific password** (appleid.apple.com → Sign-In and Security → App-Specific Passwords), not your account password. |
| `APPLE_TEAM_ID`              | Your 10-character Apple Team ID.                                                                                             |

### Windows code signing (deferred — see below)

Not wired yet. The pipeline currently produces an **unsigned** Windows
installer (it still works, but SmartScreen will warn). To add it, see
"Windows signing" below and add the relevant secrets.

## Cut a release

```bash
# bump version in package.json AND src-tauri/tauri.conf.json + Cargo.toml
# (keep them in sync), then:
git tag v0.2.0
git push origin v0.2.0
```

Watch the `release` workflow, then publish the draft Release it created.

> First release: there is no `latest.json` yet, so `checkForUpdate()` simply
> returns null in older builds — expected. Auto-update kicks in from the
> release _after_ the first one.

## Regenerating the updater key with a password (optional, recommended)

```bash
npx tauri signer generate -w ~/.tauri/sundayedit_updater.key -f   # prompts for a password
```

Then update `plugins.updater.pubkey` in `tauri.conf.json` with the new
public key, and update the `TAURI_SIGNING_PRIVATE_KEY` /
`TAURI_SIGNING_PRIVATE_KEY_PASSWORD` secrets. Note: rotating the key means
apps signed with the old key won't accept updates signed with the new one —
only do this before you have real users.

## ffmpeg sidecar (wired)

ffmpeg + ffprobe are bundled as Tauri `externalBin` sidecars, so import,
waveform, and burn-in work without a system ffmpeg:

- `bundle.externalBin` in `tauri.conf.json` lists `binaries/ffmpeg` +
  `binaries/ffprobe`.
- At runtime `services/video.rs` resolves the binary next to the app
  executable first (the bundled sidecar), then falls back to PATH, with a
  `SUNDAYEDIT_FFMPEG` / `SUNDAYEDIT_FFPROBE` env override for dev/tests.
- The binaries are **not committed** (too large). Fetch them before every
  build: `node scripts/fetch-ffmpeg.mjs` — it copies the `ffmpeg-static` +
  `@ffprobe-installer/ffprobe` binaries into `src-tauri/binaries/` with the
  Rust target-triple suffix. CI runs this automatically (see
  `release.yml`); each runner fetches its own platform's binaries.

> **Licensing:** these are GPL/LGPL ffmpeg builds. Before any _public_
> release, confirm GPL compliance (offer the corresponding source) or swap to
> an LGPL/own build. Fine for private test builds.

> macOS release builds are **universal** (arm64 + `x86_64` in one app/DMG,
> `tauri build --target universal-apple-darwin`). The fetch script's
> `--universal` flag fetches the non-host arch's ffmpeg/ffprobe from the
> same upstreams and `lipo`s the pair into `ffmpeg-universal-apple-darwin`
> etc. — a universal build needs both the per-arch sidecars (tauri-build
> validates each cargo slice) and the `-universal` ones (the bundler).
> Local Whisper on Intel Macs runs on the CPU backend (Metal is
> Apple-Silicon-only upstream) with an instruction baseline every
> 10.15-capable Intel Mac has — see
> `src-tauri/cmake/ggml-ci-portable.cmake`.

## macOS ships a custom webview user agent

`src-tauri/tauri.macos.conf.json` overrides the window's user agent on **macOS
only**:

```
Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.0 Safari/605.1.15 SundayEdit
```

**Why it exists.** Tauri's WKWebView reports no `Safari` token, and PixiJS
detects WebKit by user-agent regex to decide how it uploads video frames to the
GPU. Without the token it takes the `texSubImage2D` path, which ADR-010
measured at **42×** the cost of `texImage2D` in this webview (28.92 ms vs
0.69 ms per frame, two 1080p layers) — enough to drop the GPU preview from
30 fps to 20. Adding the token restores Pixi's own workaround without patching
Pixi internals. Full measurements: `docs/DECISIONS.md`, ADR-010 and its
addendum.

**Scope and risk.**

- **macOS only.** Tauri merges `tauri.<platform>.conf.json` over the base
  config, and the base config sets no `userAgent`, so Windows/WebView2 keeps its
  native Chromium UA (where ADR-010 measured no difference between the upload
  paths).
- A user agent is public, and some sites and servers branch on it. Here the
  exposure is essentially nil: **the webview makes no external network
  requests.** Every HTTP call — Whisper model download, Claude / OpenAI /
  AssemblyAI / Deepgram — is made from Rust with `reqwest`, which has its own
  user agent and is unaffected. If a future feature ever loads third-party web
  content inside the webview, re-evaluate this.
- The value is guarded by `src-tauri/tests/webview_user_agent.rs`, which
  reimplements Pixi's `isSafari()` regex and also asserts that the macOS window
  definition has not drifted from the base one — platform config merging
  **replaces** the `windows` array, so a property added to `tauri.conf.json`
  silently disappears on macOS unless it is copied across.
- **Not yet verified on the rig:** that wry applies the value in the shipped
  app over `tauri://`. Check `navigator.userAgent` once in a real build.

## Deferred — required before a real public 1.0

These are intentionally **not** wired yet (they need binaries/infra, not just
config):

- ~~**Whisper model download.**~~ ✅ Done — the app downloads the chosen ggml
  model on first run from the Hugging Face `whisper.cpp` repo
  (`asr_download_model`: atomic `.part`→rename, progress events, cancel, size
  verify), driven by the model picker / onboarding. No bundling, no own CDN.
- **Windows code signing.** Import a code-signing cert on the runner (or use
  Azure Trusted Signing) and set `bundle.windows.certificateThumbprint` /
  the signing env. EV cert avoids SmartScreen warnings.
- **Beta channel.** A second updater endpoint + `prerelease` releases for
  opt-in testers.
- **End-to-end update test.** Build vX, install it, publish vX+1, confirm the
  in-app banner downloads + relaunches into the new version before any public
  announcement.

## Local production build (unsigned, for smoke-testing the bundle)

```bash
npm ci
node scripts/fetch-ffmpeg.mjs  # bundle ffmpeg/ffprobe (required by externalBin)
npm run tauri build            # add --features llm,whisper,diarize for full native AI
```

Artifacts land in `src-tauri/target/release/bundle/`.

For a release-like **universal** macOS bundle (needs both rust targets:
`rustup target add aarch64-apple-darwin x86_64-apple-darwin`):

```bash
node scripts/fetch-ffmpeg.mjs --universal
CMAKE_PROJECT_TOP_LEVEL_INCLUDES="$PWD/src-tauri/cmake/ggml-ci-portable.cmake" \
  npm run tauri build -- --target universal-apple-darwin --features whisper,llm
```

Artifacts land in `src-tauri/target/universal-apple-darwin/release/bundle/`.

## Notarization: how the switch works (and two traps)

`release.yml` notarizes **only on a tag push**. A manual `workflow_dispatch`
run deliberately does not, which is the escape hatch for shipping while
Apple's Program License Agreement is unsigned (notarytool answers HTTP 403
until it is accepted, failing the macOS job _after_ a clean build and
code-sign). Nothing needs to be toggled when the agreement is accepted —
tag a release and notarization happens again.

Two traps this cost us a build each to learn:

1. **Setting `APPLE_ID: ''` does not disable notarization.** An empty-but-
   defined environment variable still reads as `Ok("")` on the Tauri side,
   so the bundler enters the notarize path and then dies on
   `Team ID must be at least 3 characters`. The variables have to not exist
   at all — hence the conditional step that exports them via `$GITHUB_ENV`.
2. **A `type: boolean` workflow input cannot be compared to a literal.**
   GitHub casts both sides to a number, so `"true"` and `"false"` both
   become NaN and neither equals `true`. A notarize toggle built that way
   would silently mean its default forever. The rule is positional instead.

An un-notarized build is still signed with the Developer ID cert and runs;
Gatekeeper shows a first-open warning (right-click → **Open**, or
`xattr -cr /Applications/SundayEdit.app`). Auto-update is unaffected — the
updater verifies its own minisign signature against the pubkey in
`tauri.conf.json`.
