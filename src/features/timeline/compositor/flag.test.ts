import { describe, it, expect, beforeEach, vi, afterEach } from "vitest";

import {
  initialEnabled,
  selectCompositorActive,
  useCompositorFlag,
} from "./flag";

const KEY = "sundayedit.gpuCompositor";

beforeEach(() => {
  localStorage.clear();
  useCompositorFlag.setState({ enabled: false, unavailableReason: null });
});

afterEach(() => {
  vi.restoreAllMocks();
});

describe("compositor flag — persistence", () => {
  it("is OFF by default", () => {
    // The acceptance bar for the whole stage: a fresh install renders exactly
    // as it did before E6.
    expect(initialEnabled()).toBe(false);
    expect(useCompositorFlag.getState().enabled).toBe(false);
  });

  it("persists the user's choice", () => {
    useCompositorFlag.getState().setEnabled(true);
    expect(localStorage.getItem(KEY)).toBe("1");
    expect(initialEnabled()).toBe(true);

    useCompositorFlag.getState().setEnabled(false);
    expect(localStorage.getItem(KEY)).toBe("0");
    expect(initialEnabled()).toBe(false);
  });

  it("treats an unreadable localStorage as OFF rather than throwing", () => {
    vi.spyOn(Storage.prototype, "getItem").mockImplementation(() => {
      throw new Error("blocked");
    });
    expect(initialEnabled()).toBe(false);
  });

  it("still flips in memory when the write fails", () => {
    vi.spyOn(Storage.prototype, "setItem").mockImplementation(() => {
      throw new Error("quota");
    });
    useCompositorFlag.getState().setEnabled(true);
    expect(useCompositorFlag.getState().enabled).toBe(true);
  });
});

describe("compositor flag — automatic off-switch", () => {
  it("does not rewrite the user's setting when the runtime says no", () => {
    // Conflating the two would make the toggle look like it flips itself, and
    // the user would never learn WHY the GPU path is not running.
    useCompositorFlag.getState().setEnabled(true);
    useCompositorFlag.getState().reportUnavailable("no-webgl2");
    expect(useCompositorFlag.getState().enabled).toBe(true);
    expect(localStorage.getItem(KEY)).toBe("1");
  });

  it("stops the compositor being active", () => {
    useCompositorFlag.getState().setEnabled(true);
    expect(selectCompositorActive(useCompositorFlag.getState())).toBe(true);
    useCompositorFlag.getState().reportUnavailable("context-lost");
    expect(selectCompositorActive(useCompositorFlag.getState())).toBe(false);
  });

  it("clears a stale reason when the user opts in again", () => {
    useCompositorFlag.getState().setEnabled(true);
    useCompositorFlag.getState().reportUnavailable("probe-threw");
    useCompositorFlag.getState().setEnabled(false);
    useCompositorFlag.getState().setEnabled(true);
    expect(useCompositorFlag.getState().unavailableReason).toBeNull();
    expect(selectCompositorActive(useCompositorFlag.getState())).toBe(true);
  });

  it("keeps the reason when the flag is turned OFF, so the notice can explain itself", () => {
    useCompositorFlag.getState().setEnabled(true);
    useCompositorFlag.getState().reportUnavailable("no-webgl2");
    useCompositorFlag.getState().setEnabled(false);
    expect(useCompositorFlag.getState().unavailableReason).toBe("no-webgl2");
  });
});

describe("selectCompositorActive", () => {
  it("is the AND of the setting and the runtime", () => {
    const s = useCompositorFlag.getState();
    expect(selectCompositorActive({ ...s, enabled: false })).toBe(false);
    expect(
      selectCompositorActive({
        ...s,
        enabled: false,
        unavailableReason: "no-webgl2",
      }),
    ).toBe(false);
    expect(
      selectCompositorActive({
        ...s,
        enabled: true,
        unavailableReason: "no-webgl2",
      }),
    ).toBe(false);
    expect(
      selectCompositorActive({ ...s, enabled: true, unavailableReason: null }),
    ).toBe(true);
  });
});
