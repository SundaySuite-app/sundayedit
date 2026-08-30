import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";

import {
  duplicateTimelineItem,
  newestTimelineItemId,
} from "./timelineOpsExtra";
import { IPCError } from "@/lib/ipc";
import { SAMPLE_PROJECT } from "@/lib/sampleProject";
import type { Project } from "@/lib/bindings";

const invoke = vi.fn();
vi.mock("@tauri-apps/api/core", () => ({
  invoke: (...args: unknown[]) => invoke(...args),
}));

beforeEach(() => invoke.mockReset());
afterEach(() => vi.restoreAllMocks());

describe("duplicateTimelineItem", () => {
  it("calls op_duplicate_timeline_item with camelCase args", async () => {
    invoke.mockResolvedValueOnce(SAMPLE_PROJECT);
    await duplicateTimelineItem(SAMPLE_PROJECT, "ti1");
    expect(invoke).toHaveBeenCalledWith("op_duplicate_timeline_item", {
      project: SAMPLE_PROJECT,
      itemId: "ti1",
    });
  });

  it("wraps a Rust AppError as IPCError, same as ipc.ts's call()", async () => {
    invoke.mockRejectedValueOnce({
      code: "not_found",
      message: "timeline item nope not found",
    });
    await expect(
      duplicateTimelineItem(SAMPLE_PROJECT, "nope"),
    ).rejects.toBeInstanceOf(IPCError);
  });

  it("rethrows a plain Error unchanged", async () => {
    invoke.mockRejectedValueOnce(new Error("boom"));
    await expect(duplicateTimelineItem(SAMPLE_PROJECT, "ti1")).rejects.toThrow(
      "boom",
    );
  });
});

describe("newestTimelineItemId", () => {
  it("finds the one id present after but not before", () => {
    const after: Project = {
      ...SAMPLE_PROJECT,
      timeline_items: [
        ...SAMPLE_PROJECT.timeline_items,
        {
          ...SAMPLE_PROJECT.timeline_items[0],
          id: "ti2",
          timeline_start_ms: 18_000,
        },
      ],
    };
    expect(newestTimelineItemId(SAMPLE_PROJECT, after)).toBe("ti2");
  });

  it("returns null when nothing was added", () => {
    expect(newestTimelineItemId(SAMPLE_PROJECT, SAMPLE_PROJECT)).toBeNull();
  });
});
