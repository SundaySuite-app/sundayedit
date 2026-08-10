/**
 * Filmstrip tile grid math, screen mapping, and the stale-while-loading
 * selection policy — all pure. The cache/IPC layer (`requestFilmstripTile`,
 * `useFilmstripTiles`) gets a lighter smoke test at the bottom, mocking Tauri
 * exactly like `MediaBin.test.tsx` / `ClipInspector.test.tsx` do.
 */
import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";

// Mock the lowest layer so the real `ipc.media.filmstripTile` wrapper runs.
const invoke = vi.fn();
let tauriEnv = false;
vi.mock("@tauri-apps/api/core", () => ({
  invoke: (...args: unknown[]) => invoke(...args),
  convertFileSrc: (p: string) => `asset://localhost/${p}`,
  isTauri: () => tauriEnv,
}));
vi.mock("@tauri-apps/api/path", () => ({
  appCacheDir: async () => "/cache",
  join: async (...parts: string[]) => parts.join("/"),
}));

import { renderHook, waitFor } from "@testing-library/react";
import type { TimelineItem } from "@/lib/bindings";
import { SAMPLE_PROJECT } from "@/lib/sampleProject";
import {
  TILE_BASE_SPAN_MS,
  TILE_MAX_TIER,
  clampTier,
  tileSpanMs,
  tileKey,
  tileIndexAt,
  tileRangeMs,
  parentTile,
  tilesForRange,
  pickZoomTier,
  sourceRangeForVisible,
  pxPerSourceMs,
  tileScreenRect,
  selectDisplayTiles,
  requestFilmstripTile,
  useFilmstripTiles,
  __resetFilmstripCacheForTests,
  type FilmTile,
} from "./filmstrip";

// ── grid math (mirrors src-tauri/src/services/tiles.rs's own test suite) ────

describe("tier / span math", () => {
  it("tier zero is the base span", () => {
    expect(tileSpanMs(0)).toBe(TILE_BASE_SPAN_MS);
  });

  it("spans halve exactly every tier", () => {
    for (let t = 0; t < TILE_MAX_TIER; t++) {
      expect(tileSpanMs(t + 1) * 2).toBe(tileSpanMs(t));
    }
  });

  it("finest tier is 250ms", () => {
    expect(tileSpanMs(TILE_MAX_TIER)).toBe(250);
  });

  it("clamps instead of going out of range", () => {
    expect(clampTier(999)).toBe(TILE_MAX_TIER);
    expect(clampTier(-4)).toBe(0);
    expect(tileSpanMs(999)).toBe(tileSpanMs(TILE_MAX_TIER));
  });
});

describe("indexing", () => {
  it("index is floor of time over span", () => {
    expect(tileIndexAt(0, 0)).toBe(0);
    expect(tileIndexAt(63_999, 0)).toBe(0);
    expect(tileIndexAt(64_000, 0)).toBe(1);
    expect(tileIndexAt(64_001, 0)).toBe(1);
  });

  it("negative time clamps to tile zero", () => {
    expect(tileIndexAt(-5_000, 4)).toBe(0);
  });

  it("range round-trips with index at every tier", () => {
    for (let tier = 0; tier <= TILE_MAX_TIER; tier++) {
      for (const index of [0, 1, 7, 4096]) {
        const [s, e] = tileRangeMs(tier, index);
        expect(tileIndexAt(s, tier)).toBe(index);
        expect(tileIndexAt(e - 1, tier)).toBe(index);
        expect(e - s).toBe(tileSpanMs(tier));
      }
    }
  });
});

describe("keys", () => {
  it("are stable and distinct", () => {
    expect(tileKey(3, 12)).toBe("z3i12");
    expect(tileKey(3, 12)).toBe(tileKey(3, 12));
    expect(tileKey(3, 12)).not.toBe(tileKey(4, 12));
    expect(tileKey(3, 12)).not.toBe(tileKey(3, 13));
  });

  it("clamps tier and index", () => {
    expect(tileKey(99, -4)).toBe(tileKey(TILE_MAX_TIER, 0));
  });
});

describe("tilesForRange", () => {
  it("returns tiles in order, abutting, spanning the range", () => {
    const tiles = tilesForRange(0, 200_000, 0);
    expect(tiles.map((t) => t.index)).toEqual([0, 1, 2, 3]);
    expect(tiles[0].startMs).toBeLessThanOrEqual(0);
    expect(tiles[3].endMs).toBeGreaterThanOrEqual(200_000);
    for (let i = 1; i < tiles.length; i++) {
      expect(tiles[i - 1].endMs).toBe(tiles[i].startMs);
    }
  });

  it("an end exactly on a boundary does not pull in the next tile", () => {
    expect(tilesForRange(0, 64_000, 0).map((t) => t.index)).toEqual([0]);
  });

  it("an empty range yields the containing tile", () => {
    expect(tilesForRange(70_000, 70_000, 0).map((t) => t.index)).toEqual([1]);
  });

  it("clamps a negative start", () => {
    const tiles = tilesForRange(-10_000, 1_000, 0);
    expect(tiles).toHaveLength(1);
    expect(tiles[0].startMs).toBe(0);
  });

  it("scrolling reuses tiles because the grid is absolute", () => {
    const a = tilesForRange(10_000, 90_000, 0).map((t) => t.key);
    const b = tilesForRange(50_000, 130_000, 0).map((t) => t.key);
    const shared = b.filter((k) => a.includes(k));
    expect(shared.length).toBeGreaterThanOrEqual(2);
  });
});

describe("parentTile", () => {
  it("is the inverse of the child relationship implied by tileRangeMs", () => {
    for (let tier = 1; tier <= TILE_MAX_TIER; tier++) {
      for (const index of [0, 1, 2, 3, 77]) {
        const p = parentTile(tier, index)!;
        const [ps, pe] = tileRangeMs(tier - 1, p);
        const [cs] = tileRangeMs(tier, index);
        expect(cs).toBeGreaterThanOrEqual(ps);
        expect(cs).toBeLessThan(pe);
      }
    }
  });

  it("has no parent at tier zero", () => {
    expect(parentTile(0, 5)).toBeNull();
  });
});

// ── screen ↔ source-grid mapping ─────────────────────────────────────────────

describe("pickZoomTier", () => {
  it("picks the coarsest tier whose tile fits the screen budget", () => {
    // At a very small px/ms even tier 0's 64s tile fits comfortably.
    expect(pickZoomTier(0.001)).toBe(0);
  });

  it("goes finer as px-per-ms grows", () => {
    const coarse = pickZoomTier(0.001);
    const fine = pickZoomTier(2);
    expect(fine).toBeGreaterThan(coarse);
  });

  it("never exceeds TILE_MAX_TIER even when nothing fits the budget", () => {
    expect(pickZoomTier(1000)).toBe(TILE_MAX_TIER);
  });

  it("degenerates to tier 0 on a non-positive or non-finite px-per-ms", () => {
    expect(pickZoomTier(0)).toBe(0);
    expect(pickZoomTier(-1)).toBe(0);
    expect(pickZoomTier(NaN)).toBe(0);
    expect(pickZoomTier(Infinity)).toBe(0);
  });

  it("respects a custom screen-width budget", () => {
    // A tighter budget needs a finer (or equal) tier than a looser one.
    const tight = pickZoomTier(0.01, 100);
    const loose = pickZoomTier(0.01, 10_000);
    expect(tight).toBeGreaterThanOrEqual(loose);
  });
});

function clip(extra?: Partial<TimelineItem>): {
  timeline_start_ms: number;
  in_ms: number;
  out_ms: number;
  speed: number;
} {
  return {
    timeline_start_ms: 1_000,
    in_ms: 2_000,
    out_ms: 12_000,
    speed: 1,
    ...extra,
  };
}

describe("sourceRangeForVisible", () => {
  it("maps 1:1 at speed 1 (source offset by in_ms - timeline_start_ms)", () => {
    const r = sourceRangeForVisible(clip(), 1_000, 3_000);
    expect(r).toEqual({ startMs: 2_000, endMs: 4_000 });
  });

  it("scales by speed", () => {
    // 2× speed: 1000ms of timeline covers 2000ms of source.
    const r = sourceRangeForVisible(clip({ speed: 2 }), 1_000, 2_000);
    expect(r).toEqual({ startMs: 2_000, endMs: 4_000 });
  });

  it("clamps to the clip's own in/out bounds", () => {
    // Visible window overruns the clip on both sides.
    const r = sourceRangeForVisible(clip(), -5_000, 50_000);
    expect(r.startMs).toBe(2_000); // in_ms
    expect(r.endMs).toBe(12_000); // out_ms
  });

  it("never returns an empty (or inverted) range", () => {
    const r = sourceRangeForVisible(clip(), 1_000, 1_000);
    expect(r.endMs).toBeGreaterThan(r.startMs);
  });
});

describe("pxPerSourceMs", () => {
  it("equals timeline px-per-ms at speed 1", () => {
    expect(pxPerSourceMs(clip(), 0.1)).toBeCloseTo(0.1);
  });

  it("divides by speed", () => {
    expect(pxPerSourceMs(clip({ speed: 2 }), 0.1)).toBeCloseTo(0.05);
  });

  it("floors speed at 0.01 (no divide-by-zero on a degenerate clip)", () => {
    expect(Number.isFinite(pxPerSourceMs(clip({ speed: 0 }), 0.1))).toBe(true);
  });
});

describe("tileScreenRect", () => {
  it("places a tile at the clip's speed-1 offset from in_ms", () => {
    const c = clip();
    const r = tileScreenRect(c, { startMs: 2_000, endMs: 4_000 }, 0.1);
    // (2000 - in_ms=2000)/1 * 0.1 = 0
    expect(r.leftPx).toBeCloseTo(0);
    expect(r.widthPx).toBeCloseTo(200); // 2000ms * 0.1 px/ms
  });

  it("compresses screen width at higher speed", () => {
    const c = clip({ speed: 2 });
    const r = tileScreenRect(c, { startMs: 2_000, endMs: 4_000 }, 0.1);
    expect(r.widthPx).toBeCloseTo(100); // half of the speed-1 width
  });
});

// ── stale-while-loading selection ────────────────────────────────────────────

function tile(tier: number, index: number): FilmTile {
  const [s, e] = tileRangeMs(tier, index);
  return { tier, index, startMs: s, endMs: e, key: tileKey(tier, index) };
}

describe("selectDisplayTiles", () => {
  it("shows a ready tile as itself", () => {
    const w = [tile(4, 3)];
    const [sel] = selectDisplayTiles(w, (k) => k === tile(4, 3).key);
    expect(sel.displayKey).toBe(tile(4, 3).key);
    expect(sel.stale).toBe(false);
  });

  it("falls back to the nearest ready ancestor while the exact tile loads", () => {
    const wanted = tile(4, 3); // parent at tier 3 is index 1
    const parentKey = tileKey(3, 1);
    const [sel] = selectDisplayTiles([wanted], (k) => k === parentKey);
    expect(sel.displayKey).toBe(parentKey);
    expect(sel.stale).toBe(true);
  });

  it("walks multiple levels up when only a coarser ancestor is ready", () => {
    const wanted = tile(4, 3); // parents: tier3/1, tier2/0, tier1/0, tier0/0
    const grandparentKey = tileKey(2, 0);
    const [sel] = selectDisplayTiles([wanted], (k) => k === grandparentKey);
    expect(sel.displayKey).toBe(grandparentKey);
    expect(sel.stale).toBe(true);
  });

  it("returns null when nothing in the ancestor chain is ready", () => {
    const [sel] = selectDisplayTiles([tile(4, 3)], () => false);
    expect(sel.displayKey).toBeNull();
    expect(sel.stale).toBe(false);
  });

  it("a tier-0 tile has no ancestor to fall back to", () => {
    const [sel] = selectDisplayTiles([tile(0, 5)], () => false);
    expect(sel.displayKey).toBeNull();
  });

  it("resolves each wanted tile independently", () => {
    const wanted = [tile(4, 3), tile(4, 4)];
    const ready = new Set([tile(4, 3).key]);
    const sel = selectDisplayTiles(wanted, (k) => ready.has(k));
    expect(sel[0].stale).toBe(false);
    expect(sel[1].displayKey).toBeNull();
  });
});

// ── cache + IPC smoke test (impure layer) ────────────────────────────────────

// The sample project's only media item (video, 18s, 1920×1080).
const MEDIA = SAMPLE_PROJECT.media[0];

beforeEach(() => {
  invoke.mockReset();
  tauriEnv = false;
  __resetFilmstripCacheForTests();
});

afterEach(() => {
  vi.restoreAllMocks();
});

describe("requestFilmstripTile", () => {
  it("resolves null off-Tauri without calling invoke", async () => {
    const url = await requestFilmstripTile(MEDIA, 4, 0);
    expect(url).toBeNull();
    expect(invoke).not.toHaveBeenCalled();
  });

  it("resolves null for audio-only media even under Tauri", async () => {
    tauriEnv = true;
    const url = await requestFilmstripTile(
      { ...MEDIA, kind: "audio_only" },
      4,
      0,
    );
    expect(url).toBeNull();
    expect(invoke).not.toHaveBeenCalled();
  });

  it("calls extract_filmstrip_tile with the tile's own ms range", async () => {
    tauriEnv = true;
    invoke.mockImplementation((cmd: string, args: Record<string, unknown>) =>
      cmd === "extract_filmstrip_tile"
        ? Promise.resolve(args.outPath)
        : Promise.reject(new Error(`unexpected: ${cmd}`)),
    );
    const url = await requestFilmstripTile(MEDIA, 4, 1);
    expect(url).toContain("asset://");
    const [cmd, args] = invoke.mock.calls[0] as [
      string,
      Record<string, unknown>,
    ];
    expect(cmd).toBe("extract_filmstrip_tile");
    const [expectStart, expectEnd] = tileRangeMs(4, 1);
    expect(args.startMs).toBe(expectStart);
    expect(args.endMs).toBe(expectEnd);
    expect(args.mediaPath).toBe(MEDIA.path);
  });

  it("memoizes concurrent requests for the same tile — ffmpeg spawns once", async () => {
    tauriEnv = true;
    let calls = 0;
    invoke.mockImplementation(
      async (cmd: string, args: Record<string, unknown>) => {
        if (cmd !== "extract_filmstrip_tile") throw new Error("unexpected");
        calls++;
        return args.outPath;
      },
    );
    const [a, b] = await Promise.all([
      requestFilmstripTile(MEDIA, 4, 0),
      requestFilmstripTile(MEDIA, 4, 0),
    ]);
    expect(a).toBe(b);
    expect(calls).toBe(1);
  });

  it("degrades to null (not a throw) when the backend errors", async () => {
    tauriEnv = true;
    invoke.mockImplementation(() => Promise.reject(new Error("ffmpeg failed")));
    await expect(requestFilmstripTile(MEDIA, 4, 0)).resolves.toBeNull();
  });
});

describe("useFilmstripTiles", () => {
  it("resolves empty off-Tauri — callers keep today's thumbnail backdrop", () => {
    const { result } = renderHook(() =>
      useFilmstripTiles(MEDIA, clip(), 0.1, 0, 10_000),
    );
    expect(result.current).toEqual([]);
    expect(invoke).not.toHaveBeenCalled();
  });

  it("resolves empty when `media` is undefined (dragging / non-video)", () => {
    const { result } = renderHook(() =>
      useFilmstripTiles(undefined, clip(), 0.1, 0, 10_000),
    );
    expect(result.current).toEqual([]);
  });

  it("populates tiles once the backend resolves, under Tauri", async () => {
    tauriEnv = true;
    invoke.mockImplementation((cmd: string, args: Record<string, unknown>) =>
      cmd === "extract_filmstrip_tile"
        ? Promise.resolve(args.outPath)
        : Promise.reject(new Error(`unexpected: ${cmd}`)),
    );
    const { result } = renderHook(() =>
      useFilmstripTiles(MEDIA, clip(), 0.1, 1_000, 3_000),
    );
    await waitFor(() => expect(result.current.length).toBeGreaterThan(0));
    for (const t of result.current) {
      expect(t.url).toContain("asset://");
      expect(t.widthPx).toBeGreaterThan(0);
    }
  });

  it("draws a coarse stand-in at the ANCESTOR's own rect, once", async () => {
    // The seam between `selectDisplayTiles` (which resolves a still-loading
    // tile to a ready coarser ANCESTOR) and the geometry the caller paints it
    // at. The ancestor's JPEG covers 2^n× the source range of the tile it
    // stands in for, so painting it into the CHILD's slot squeezes the whole
    // coarse strip into a quarter of the width — the frames shown are not the
    // frames that belong there, and every sibling child repeats the same
    // squeezed image (their opacities stacking). A stand-in must be drawn at
    // ITS OWN rect and let the clip box clip it.
    tauriEnv = true;
    const wide = clip({ timeline_start_ms: 0, in_ms: 0, out_ms: 200_000 });

    // Round 1: zoomed out far enough for tier 0 (64 s per tile). Let it land.
    invoke.mockImplementation((cmd: string, args: Record<string, unknown>) =>
      cmd === "extract_filmstrip_tile"
        ? Promise.resolve(args.outPath)
        : Promise.reject(new Error(`unexpected: ${cmd}`)),
    );
    const coarse = renderHook(() =>
      useFilmstripTiles(MEDIA, wide, 0.005, 0, 64_000),
    );
    await waitFor(() =>
      expect(coarse.result.current.length).toBeGreaterThan(0),
    );
    expect(coarse.result.current[0].tier).toBe(0);
    coarse.unmount();

    // Round 2: zoom in to tier 2 (16 s per tile) and never resolve the finer
    // tiles, so every wanted tile falls back to the ready tier-0 ancestor.
    invoke.mockImplementation(() => new Promise(() => {}));
    const { result } = renderHook(() =>
      useFilmstripTiles(MEDIA, wide, 0.02, 0, 64_000),
    );
    await waitFor(() => expect(result.current.length).toBeGreaterThan(0));

    expect(result.current.every((t) => t.stale)).toBe(true);
    // One entry PER DISTINCT ancestor, not per wanted tile: the visible window
    // plus overscan wants five tier-2 tiles, and they share just two tier-0
    // ancestors. Painting an ancestor once per child would stack its opacity
    // into a bright band.
    expect(result.current.map((t) => t.key)).toEqual(["z0i0", "z0i1"]);
    // Each is laid out at ITS OWN 64 000 ms span — 1280 px at 0.02 px/ms,
    // abutting — not squeezed into a 320 px tier-2 slot.
    expect(result.current[0].leftPx).toBeCloseTo(0);
    expect(result.current[0].widthPx).toBeCloseTo(64_000 * 0.02);
    expect(result.current[1].leftPx).toBeCloseTo(64_000 * 0.02);
    expect(result.current[1].widthPx).toBeCloseTo(64_000 * 0.02);
  });

  it("paints tiles that arrive while nothing else changes", async () => {
    // The cache settles OUTSIDE React. A paint list memoized only on the
    // geometry (`media`/`wanted`/`pxPerMs`/`item`) therefore keeps returning
    // the pre-load answer, and the strip stays blank until an unrelated scroll
    // or edit invalidates it. This test holds `item` at a STABLE reference —
    // which is what `ClipBox` actually passes, since the item comes from the
    // project store and does not change identity while a tile renders.
    tauriEnv = true;
    invoke.mockImplementation((cmd: string, args: Record<string, unknown>) =>
      cmd === "extract_filmstrip_tile"
        ? Promise.resolve(args.outPath)
        : Promise.reject(new Error(`unexpected: ${cmd}`)),
    );
    const stable = clip();
    const { result } = renderHook(() =>
      useFilmstripTiles(MEDIA, stable, 0.1, 1_000, 3_000),
    );
    await waitFor(() => expect(result.current.length).toBeGreaterThan(0));
  });
});
