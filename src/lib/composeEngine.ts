/**
 * Compose-engine helpers (Task U) — the thin, non-`ipc.ts` glue the multi-track
 * export + preview-render UI needs.
 *
 * `ipc.compose.render` / `ipc.compose.cancel` already live in `ipc.ts` (owned by
 * the Rust agent). This module only adds the pieces the frontend phase needs and
 * that `ipc.ts` doesn't own yet:
 *
 *   - `subscribeComposeProgress` — the render command streams `ComposeProgress`
 *     on the `compose-render-progress` event. Under Tauri that's the Tauri event
 *     bus; the browser/E2E mock re-emits it as a `window` CustomEvent (it can't
 *     reproduce the bus). We listen to BOTH so the progress modal works in the
 *     real app and in Playwright without reimplementing Tauri's event layer.
 *   - `renderPreviewProxy` — invokes `compose_preview_proxy` directly (the Rust
 *     command lands this phase); guarded so it degrades to a no-op off-Tauri.
 *   - `defaultComposeSettings` — project-derived H264/CPU defaults.
 */

import { invoke, isTauri } from "@tauri-apps/api/core";
import type { ComposeProgress, ComposeSettings, Project } from "./bindings";

export const COMPOSE_PROGRESS_EVENT = "compose-render-progress";

/**
 * Subscribe to compose-render progress. Returns an unsubscribe function. Listens
 * to the `window` CustomEvent (browser/E2E) and, under Tauri, the Tauri event of
 * the same name — whichever the running backend emits reaches `cb`.
 */
export function subscribeComposeProgress(
  cb: (p: ComposeProgress) => void,
): () => void {
  let disposed = false;
  // Once the Tauri bus has delivered an event it is the authoritative source:
  // window CustomEvents are dropped from then on, so a backend that emits on
  // both channels can't double-fire a single progress tick into `cb`. (The E2E
  // mock sets `__TAURI_INTERNALS__` — so `isTauri()` is true — but only ever
  // dispatches window CustomEvents; those keep flowing because no Tauri-bus
  // event ever arrives there.)
  let tauriDelivered = false;

  const onWindow = (e: Event) => {
    if (disposed || tauriDelivered) return;
    const detail = (e as CustomEvent).detail as ComposeProgress | undefined;
    if (detail) cb(detail);
  };
  window.addEventListener(COMPOSE_PROGRESS_EVENT, onWindow);

  let unlistenTauri: (() => void) | undefined;
  if (isTauri()) {
    import("@tauri-apps/api/event")
      .then(({ listen }) => {
        // Unsubscribed while the dynamic import was in flight — never register
        // a listener we'd only have to tear down again.
        if (disposed) return undefined;
        return listen<ComposeProgress>(COMPOSE_PROGRESS_EVENT, (e) => {
          // Guards the gap between dispose() and the async unlisten resolving.
          if (disposed) return;
          tauriDelivered = true;
          cb(e.payload);
        });
      })
      .then((un) => {
        if (!un) return;
        if (disposed) un();
        else unlistenTauri = un;
      })
      .catch(() => {
        // No Tauri event bus (browser mock) — the window listener carries it.
      });
  }

  return () => {
    disposed = true;
    window.removeEventListener(COMPOSE_PROGRESS_EVENT, onWindow);
    unlistenTauri?.();
    unlistenTauri = undefined;
  };
}

/**
 * Render a fast preview proxy of the whole timeline to `output`. Backed by the
 * Rust `compose_preview_proxy` command. No-op resolving `false` off-Tauri so the
 * browser/demo degrades gracefully; resolves `true` when a proxy was produced.
 */
export async function renderPreviewProxy(
  project: Project,
  output: string,
): Promise<boolean> {
  if (!isTauri()) return false;
  await invoke("compose_preview_proxy", { project, output });
  return true;
}

/**
 * Round up to the nearest even dimension, never below 2 — mirrors Rust
 * `compose::even_up`. H.264/`yuv420p` requires even output dimensions; odd
 * ones (screen/web captures probe as odd) otherwise abort a transition export
 * (xfade size mismatch) or silently shrink the plain path by one pixel.
 */
const evenUp = (n: number): number => {
  const m = Math.max(2, Math.round(n));
  return m % 2 ? m + 1 : m;
};

/** H264 / CPU output settings derived from the project's own frame geometry. */
export function defaultComposeSettings(project: Project): ComposeSettings {
  const width = project.video_width > 0 ? project.video_width : 1920;
  const height = project.video_height > 0 ? project.video_height : 1080;
  const fps = project.video_fps > 0 ? Math.round(project.video_fps) : 30;
  return {
    width: evenUp(width),
    height: evenUp(height),
    fps,
    codec: "h264",
    encoder: "cpu",
    bitrate_kbps: null,
  };
}
