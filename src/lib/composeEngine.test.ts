import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";

import {
  COMPOSE_PROGRESS_EVENT,
  subscribeComposeProgress,
  renderPreviewProxy,
  defaultComposeSettings,
  detectDefaultEncoder,
} from "./composeEngine";
import { SAMPLE_PROJECT } from "./sampleProject";
import type { ComposeProgress, Project } from "./bindings";

// Mock the lowest layer (Tauri core + event bus). `tauriEnv` flips what
// `isTauri()` reports per-test; `listen` is the dynamic-import target that
// `subscribeComposeProgress` reaches for under Tauri. `renderPreviewProxy`
// routes through `ipc.compose.previewProxy`, which bottoms out in the same
// mocked `invoke` — so the command-name assertions below still see it.
const invoke = vi.fn();
let tauriEnv = false;
vi.mock("@tauri-apps/api/core", () => ({
  invoke: (...args: unknown[]) => invoke(...args),
  isTauri: () => tauriEnv,
}));

const listen = vi.fn();
vi.mock("@tauri-apps/api/event", () => ({
  listen: (...args: unknown[]) => listen(...args),
}));

/** Flush the dynamic-import → listen → store-unlisten microtask chain. */
const flush = () => new Promise((r) => setTimeout(r, 0));

function tick(fraction: number, done = false): ComposeProgress {
  return {
    out_ms: Math.round(fraction * 12_000),
    total_ms: 12_000,
    fraction,
    frame: Math.round(fraction * 360),
    done,
  };
}

function dispatchWindowTick(fraction: number) {
  window.dispatchEvent(
    new CustomEvent(COMPOSE_PROGRESS_EVENT, { detail: tick(fraction) }),
  );
}

beforeEach(() => {
  invoke.mockReset();
  listen.mockReset();
  tauriEnv = false;
});

afterEach(() => {
  vi.restoreAllMocks();
});

describe("subscribeComposeProgress — window path (browser/E2E)", () => {
  it("delivers window CustomEvents to cb and stops after unsubscribe", () => {
    const cb = vi.fn();
    const unsub = subscribeComposeProgress(cb);

    dispatchWindowTick(0.5);
    expect(cb).toHaveBeenCalledTimes(1);
    expect(cb).toHaveBeenCalledWith(tick(0.5));

    unsub();
    dispatchWindowTick(0.75);
    expect(cb).toHaveBeenCalledTimes(1);
    // Off-Tauri the event-bus module must never be touched.
    expect(listen).not.toHaveBeenCalled();
  });

  it("ignores events without a ComposeProgress detail", () => {
    const cb = vi.fn();
    const unsub = subscribeComposeProgress(cb);
    window.dispatchEvent(new Event(COMPOSE_PROGRESS_EVENT));
    expect(cb).not.toHaveBeenCalled();
    unsub();
  });
});

describe("subscribeComposeProgress — Tauri path", () => {
  it("never registers a Tauri listener when disposed before the import resolves", async () => {
    tauriEnv = true;
    listen.mockResolvedValue(vi.fn());
    const cb = vi.fn();

    const unsub = subscribeComposeProgress(cb);
    unsub(); // synchronously — the dynamic import is still in flight

    await flush();
    expect(listen).not.toHaveBeenCalled();
    expect(cb).not.toHaveBeenCalled();
  });

  it("unlistens when disposed after registration but before the listen promise resolves", async () => {
    tauriEnv = true;
    const unlistenSpy = vi.fn();
    let resolveListen!: (u: () => void) => void;
    listen.mockImplementation(
      () => new Promise<() => void>((r) => (resolveListen = r)),
    );

    const unsub = subscribeComposeProgress(vi.fn());
    await flush(); // import resolved → listen() called, its promise pending
    expect(listen).toHaveBeenCalledTimes(1);

    unsub(); // dispose while listen's promise is still pending
    resolveListen(unlistenSpy);
    await flush();
    expect(unlistenSpy).toHaveBeenCalledTimes(1); // no leaked listener
  });

  it("drops Tauri events that fire after dispose (unlisten still async)", async () => {
    tauriEnv = true;
    const cb = vi.fn();
    let handler: ((e: { payload: ComposeProgress }) => void) | undefined;
    listen.mockImplementation(
      (_evt: string, h: (e: { payload: ComposeProgress }) => void) => {
        handler = h;
        return Promise.resolve(vi.fn());
      },
    );

    const unsub = subscribeComposeProgress(cb);
    await flush();
    expect(handler).toBeDefined();

    unsub();
    handler!({ payload: tick(0.4) }); // event in the dispose→unlisten gap
    expect(cb).not.toHaveBeenCalled();
  });

  it("dedupes double sources: once the Tauri bus delivers, window events are dropped", async () => {
    tauriEnv = true;
    const cb = vi.fn();
    const unlistenSpy = vi.fn();
    let handler!: (e: { payload: ComposeProgress }) => void;
    listen.mockImplementation(
      (_evt: string, h: (e: { payload: ComposeProgress }) => void) => {
        handler = h;
        return Promise.resolve(unlistenSpy);
      },
    );

    const unsub = subscribeComposeProgress(cb);
    await flush();

    // Before any Tauri-bus delivery the window path carries events (the E2E
    // mock is exactly this: isTauri() true, but only CustomEvents ever fire).
    dispatchWindowTick(0.25);
    expect(cb).toHaveBeenCalledTimes(1);

    // A Tauri-bus event arrives → the bus is authoritative from now on.
    handler({ payload: tick(0.5) });
    expect(cb).toHaveBeenCalledTimes(2);

    // The same tick re-broadcast as a CustomEvent must NOT double-fire.
    dispatchWindowTick(0.5);
    expect(cb).toHaveBeenCalledTimes(2);

    handler({ payload: tick(0.75) });
    expect(cb).toHaveBeenCalledTimes(3);

    unsub();
    expect(unlistenSpy).toHaveBeenCalledTimes(1);
  });
});

describe("renderPreviewProxy", () => {
  it("is a no-op resolving false off-Tauri", async () => {
    tauriEnv = false;
    await expect(
      renderPreviewProxy(SAMPLE_PROJECT, "/out/proxy.mp4"),
    ).resolves.toBe(false);
    expect(invoke).not.toHaveBeenCalled();
  });

  it("invokes compose_preview_proxy (via ipc.compose.previewProxy) under Tauri and resolves true", async () => {
    tauriEnv = true;
    invoke.mockResolvedValueOnce(undefined);
    await expect(
      renderPreviewProxy(SAMPLE_PROJECT, "/out/proxy.mp4"),
    ).resolves.toBe(true);
    expect(invoke).toHaveBeenCalledWith("compose_preview_proxy", {
      project: SAMPLE_PROJECT,
      output: "/out/proxy.mp4",
    });
  });

  // The preview quality ladder's rung (features/timeline/previewQuality.ts)
  // reaches the proxy render as a percentage of the project's frame geometry —
  // `compose::proxy_settings` derives the render size from what it is handed
  // (and still caps height at 480 on top). Export never comes through here.
  it("hands the project through UNMODIFIED at full scale", async () => {
    tauriEnv = true;
    invoke.mockResolvedValueOnce(undefined);
    await renderPreviewProxy(SAMPLE_PROJECT, "/out/proxy.mp4", 100);
    expect(invoke.mock.calls[0][1].project).toBe(SAMPLE_PROJECT);
  });

  it("scales the requested proxy geometry to the tier below full scale", async () => {
    tauriEnv = true;
    invoke.mockResolvedValueOnce(undefined);
    await renderPreviewProxy(SAMPLE_PROJECT, "/out/proxy.mp4", 50);
    const sent = invoke.mock.calls[0][1].project as Project;
    // 1920×1080 → 960×540, aspect kept, both even (H.264 yuv420p).
    expect(sent.video_width).toBe(960);
    expect(sent.video_height).toBe(540);
    expect(sent.video_width % 2).toBe(0);
    expect(sent.video_height % 2).toBe(0);
    // Only the canvas moved — the timeline itself is untouched.
    expect(sent.timeline_items).toBe(SAMPLE_PROJECT.timeline_items);
    expect(SAMPLE_PROJECT.video_width).toBe(1920);
  });

  it("rounds an odd scaled dimension up to an even one", async () => {
    tauriEnv = true;
    invoke.mockResolvedValueOnce(undefined);
    const odd: Project = {
      ...SAMPLE_PROJECT,
      video_width: 1000,
      video_height: 606,
    };
    await renderPreviewProxy(odd, "/out/proxy.mp4", 25);
    const sent = invoke.mock.calls[0][1].project as Project;
    expect(sent.video_width).toBe(250);
    expect(sent.video_height).toBe(152); // 151.5 → 152
  });

  it("ignores a nonsense scale rather than requesting a zero-size proxy", async () => {
    tauriEnv = true;
    invoke.mockResolvedValueOnce(undefined);
    await renderPreviewProxy(SAMPLE_PROJECT, "/out/proxy.mp4", 0);
    expect(invoke.mock.calls[0][1].project).toBe(SAMPLE_PROJECT);
  });
});

describe("defaultComposeSettings", () => {
  it("passes through the project's own geometry", () => {
    expect(defaultComposeSettings(SAMPLE_PROJECT)).toEqual({
      width: 1920,
      height: 1080,
      fps: 30,
      codec: "h264",
      encoder: "cpu",
      bitrate_kbps: null,
    });
  });

  it("falls back to 1920x1080@30 for zero or negative geometry", () => {
    const broken: Project = {
      ...SAMPLE_PROJECT,
      video_width: 0,
      video_height: -720,
      video_fps: 0,
    };
    const s = defaultComposeSettings(broken);
    expect(s.width).toBe(1920);
    expect(s.height).toBe(1080);
    expect(s.fps).toBe(30);
  });

  it("rounds fractional frame rates", () => {
    const ntsc: Project = { ...SAMPLE_PROJECT, video_fps: 29.97 };
    expect(defaultComposeSettings(ntsc).fps).toBe(30);
  });

  // Regression (seam-compose-settings-missing-even-up / diff-compose-settings-
  // odd-dims): H.264/yuv420p needs EVEN output dimensions. The Rust side owns
  // even_up() for exactly this but the export settings used to copy odd probe
  // dims verbatim — aborting transition exports (xfade size mismatch) and
  // silently shrinking plain ones. The default settings must round up, exactly
  // like Rust `compose::even_up` (mirrored in the Rust integration test
  // `compose_even_dimensions.rs` — keep the two in sync).
  it("rounds odd source dimensions UP to even (H.264/yuv420p requirement)", () => {
    const oddCapture: Project = {
      ...SAMPLE_PROJECT,
      video_width: 641,
      video_height: 481,
    };
    const s = defaultComposeSettings(oddCapture);
    expect(s.width).toBe(642);
    expect(s.height).toBe(482);

    const screenGrab: Project = {
      ...SAMPLE_PROJECT,
      video_width: 1080,
      video_height: 607,
    };
    expect(defaultComposeSettings(screenGrab).height).toBe(608);
  });
});

describe("detectDefaultEncoder", () => {
  it("resolves cpu off-Tauri without ever calling invoke", async () => {
    tauriEnv = false;
    await expect(detectDefaultEncoder()).resolves.toBe("cpu");
    expect(invoke).not.toHaveBeenCalled();
  });

  it("relays the platform pick from compose_default_encoder under Tauri", async () => {
    tauriEnv = true;
    invoke.mockResolvedValueOnce("video-toolbox");
    await expect(detectDefaultEncoder()).resolves.toBe("video-toolbox");
    expect(invoke).toHaveBeenCalledWith("compose_default_encoder");
  });

  it("falls back to cpu if the round-trip rejects", async () => {
    tauriEnv = true;
    invoke.mockRejectedValueOnce(new Error("no such command"));
    await expect(detectDefaultEncoder()).resolves.toBe("cpu");
  });
});
