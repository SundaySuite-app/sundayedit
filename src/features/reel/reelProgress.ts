/**
 * Batch-render progress plumbing for the Highlight Reel Studio.
 *
 * `reel_render_all` emits `reel-render-progress` from Rust on the Tauri event
 * bus. That bus does not exist in the browser/E2E build, where the mock backend
 * re-emits the same payload as a `window` CustomEvent instead. We listen to
 * BOTH — exactly as `lib/composeEngine.ts` does for `compose-render-progress`
 * — so one subscriber works in the real app and under Playwright without
 * reimplementing Tauri's event layer here.
 *
 * The command name and the event name itself live in `lib/ipc.ts`; this module
 * only adds the dual-channel subscription on top of `ipc.reel.onRenderProgress`.
 */

import { isTauri } from "@tauri-apps/api/core";

import type { ReelRenderProgress } from "@/lib/bindings";
import { REEL_RENDER_PROGRESS_EVENT, reel } from "@/lib/ipc";

/**
 * Subscribe to highlight-reel batch progress. Returns an unsubscribe function.
 *
 * Once the Tauri bus has delivered a tick it becomes the authoritative source
 * and window CustomEvents are ignored, so a backend that (someday) emits on
 * both channels cannot double-count a single item into `cb`.
 */
export function subscribeReelProgress(
  cb: (p: ReelRenderProgress) => void,
): () => void {
  let disposed = false;
  let tauriDelivered = false;

  const onWindow = (e: Event) => {
    if (disposed || tauriDelivered) return;
    const detail = (e as CustomEvent).detail as ReelRenderProgress | undefined;
    if (detail) cb(detail);
  };
  window.addEventListener(REEL_RENDER_PROGRESS_EVENT, onWindow);

  let unlistenTauri: (() => void) | undefined;
  if (isTauri()) {
    reel
      .onRenderProgress((p) => {
        // Guards the gap between dispose() and the async unlisten resolving.
        if (disposed) return;
        tauriDelivered = true;
        cb(p);
      })
      .then((un) => {
        if (disposed) un();
        else unlistenTauri = un;
      })
      .catch(() => {
        // No Tauri event bus (browser mock) — the window listener carries it.
      });
  }

  return () => {
    disposed = true;
    window.removeEventListener(REEL_RENDER_PROGRESS_EVENT, onWindow);
    unlistenTauri?.();
    unlistenTauri = undefined;
  };
}
