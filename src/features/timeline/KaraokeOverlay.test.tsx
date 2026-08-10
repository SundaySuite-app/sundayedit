import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, render } from "@testing-library/react";

import type { Caption, KaraokeOptions, Project, Word } from "@/lib/bindings";
import { DEFAULT_KARAOKE_OPTIONS } from "@/features/export/karaokeOptions";
import { SAMPLE_PROJECT } from "@/lib/sampleProject";
import { KaraokeOverlay } from "./KaraokeOverlay";
import { publishPlayheadMs } from "./playhead";

// jsdom has no ResizeObserver; the overlay installs one to track its frame
// height for font-size scaling. A no-op stub is enough — without an observed
// resize the default (0) frameHeight stands, which is fine for asserting
// per-word state/colour, independent of font sizing.
beforeEach(() => {
  vi.stubGlobal(
    "ResizeObserver",
    class {
      observe() {}
      unobserve() {}
      disconnect() {}
    },
  );
});

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
  act(() => publishPlayheadMs(0));
});

function w(text: string, start: number, end: number, confidence = 90): Word {
  return {
    text,
    start_ms: start,
    end_ms: end,
    confidence,
    edited: false,
    locked: false,
    polished: false,
    alternates: [],
  };
}

function caption(words: Word[]): Caption {
  return {
    id: "c1",
    start_ms: words[0].start_ms,
    end_ms: words[words.length - 1].end_ms,
    words,
    speaker_id: null,
    style_id: null,
    notes: null,
    ai_generated: true,
    last_edited_at: 0,
    track_id: null,
  };
}

/** SAMPLE_PROJECT with a single controlled caption and given karaoke options. */
function projectWith(words: Word[], karaoke: Partial<KaraokeOptions>): Project {
  return {
    ...SAMPLE_PROJECT,
    captions: [caption(words)],
    export_config: {
      ...SAMPLE_PROJECT.export_config,
      karaoke: { ...DEFAULT_KARAOKE_OPTIONS, ...karaoke },
    },
  };
}

describe("KaraokeOverlay", () => {
  it("renders nothing when karaoke is disabled", () => {
    const project = projectWith([w("hi", 0, 400)], { enabled: false });
    act(() => publishPlayheadMs(100));
    const { container } = render(<KaraokeOverlay project={project} />);
    expect(
      container.querySelector('[data-testid="karaoke-overlay"]'),
    ).toBeNull();
  });

  it("renders nothing when the playhead is outside every caption", () => {
    const project = projectWith([w("hi", 0, 400)], { enabled: true });
    act(() => publishPlayheadMs(9000));
    const { container } = render(<KaraokeOverlay project={project} />);
    expect(
      container.querySelector('[data-testid="karaoke-overlay"]'),
    ).toBeNull();
  });

  it("renders nothing when there is no project", () => {
    act(() => publishPlayheadMs(100));
    const { container } = render(<KaraokeOverlay project={undefined} />);
    expect(container.firstChild).toBeNull();
  });

  it("renders nothing when export_config.karaoke is unset (the pre-E4a default)", () => {
    const project: Project = {
      ...SAMPLE_PROJECT,
      captions: [caption([w("hi", 0, 400)])],
      // No `karaoke` key at all — matches every pre-E4a project file.
      export_config: { ...SAMPLE_PROJECT.export_config },
    };
    act(() => publishPlayheadMs(100));
    const { container } = render(<KaraokeOverlay project={project} />);
    expect(
      container.querySelector('[data-testid="karaoke-overlay"]'),
    ).toBeNull();
  });

  it("highlight mode: reveals a word in full the instant it becomes active, not just once done", () => {
    const project = projectWith(
      [w("one", 0, 300), w("two", 300, 600), w("three", 600, 900)],
      { enabled: true, style: "highlight" },
    );
    act(() => publishPlayheadMs(450)); // inside "two"
    const { container } = render(<KaraokeOverlay project={project} />);
    const spans = Array.from(container.querySelectorAll("[data-state]"));
    expect(spans.map((s) => s.getAttribute("data-state"))).toEqual([
      "done",
      "active",
      "pending",
    ]);
    // Highlight mode: Active is filled the same as Done, not Pending's colour.
    const [done, active, pending] = spans as HTMLElement[];
    expect(active.style.color).toBe(done.style.color);
    expect(active.style.color).not.toBe(pending.style.color);
  });

  it("pending words render in the project's pending_color", () => {
    const project = projectWith([w("one", 0, 300), w("two", 300, 600)], {
      enabled: true,
      style: "highlight",
      pending_color: "#123456",
    });
    act(() => publishPlayheadMs(0)); // "two" is pending
    const { container } = render(<KaraokeOverlay project={project} />);
    const pending = container.querySelector(
      '[data-state="pending"]',
    ) as HTMLElement;
    expect(pending.style.color).toBe("rgb(18, 52, 86)"); // #123456
  });

  it("sweep mode: the active word is partially filled proportional to progress", () => {
    const project = projectWith([w("only", 0, 400)], {
      enabled: true,
      style: "sweep",
    });
    act(() => publishPlayheadMs(100)); // 25% through a 400ms word
    const { container } = render(<KaraokeOverlay project={project} />);
    const active = container.querySelector(
      '[data-state="active"]',
    ) as HTMLElement;
    expect(active).toBeTruthy();
    expect(active.getAttribute("data-progress")).toBe("25");
    // Two layers: the pending-coloured base text and the clipped fill duplicate.
    expect(active.querySelectorAll("span").length).toBe(2);
  });

  // A trailing filler word keeps the caption active past the target word's
  // end (a single-word caption would end at the same instant its one word
  // does, so there'd be no `tMs` where the word reads Done and the caption
  // is still active to render it).
  it("confidence tint colours a low-confidence done word with low_confidence_color", () => {
    const project = projectWith(
      [w("kerigma", 0, 400, 38), w("filler", 400, 500, 90)], // 38 < 70 threshold
      {
        enabled: true,
        style: "highlight",
        confidence_tint: true,
        low_confidence_color: "#ff00aa",
      },
    );
    act(() => publishPlayheadMs(450)); // "kerigma" done, "filler" active
    const { container } = render(<KaraokeOverlay project={project} />);
    const done = container.querySelector('[data-state="done"]') as HTMLElement;
    expect(done.style.color).toBe("rgb(255, 0, 170)");
  });

  it("without confidence tint, a done word uses the style's plain foreground colour regardless of confidence", () => {
    const project = projectWith(
      [w("kerigma", 0, 400, 38), w("filler", 400, 500, 90)],
      { enabled: true, style: "highlight", confidence_tint: false },
    );
    act(() => publishPlayheadMs(450));
    const { container } = render(<KaraokeOverlay project={project} />);
    const done = container.querySelector('[data-state="done"]') as HTMLElement;
    expect(done.style.color).toBe("rgb(255, 255, 255)"); // SAMPLE_PROJECT default_style.color_fg
  });

  it("a word at/above the confidence threshold is never tinted even with confidence_tint on", () => {
    const project = projectWith(
      [w("velkommen", 0, 400, 96), w("filler", 400, 500, 90)],
      {
        enabled: true,
        style: "highlight",
        confidence_tint: true,
        confidence_threshold: 70,
      },
    );
    act(() => publishPlayheadMs(450));
    const { container } = render(<KaraokeOverlay project={project} />);
    const done = container.querySelector('[data-state="done"]') as HTMLElement;
    expect(done.style.color).toBe("rgb(255, 255, 255)");
  });

  it("confidence tinting never touches a Pending word's colour (matches ASS: \\c only overrides PrimaryColour)", () => {
    const project = projectWith(
      [w("shaky", 0, 300, 10), w("next", 300, 600, 90)],
      {
        enabled: true,
        style: "highlight",
        confidence_tint: true,
        pending_color: "#00ff00",
      },
    );
    act(() => publishPlayheadMs(0)); // "next" is pending, and low-confidence "shaky" is active
    const { container } = render(<KaraokeOverlay project={project} />);
    const pending = container.querySelector(
      '[data-state="pending"]',
    ) as HTMLElement;
    expect(pending.style.color).toBe("rgb(0, 255, 0)"); // pending_color, not tinted
  });

  it("re-renders as the shared playhead advances (subscribes via usePlayheadMs)", () => {
    const project = projectWith([w("one", 0, 300), w("two", 300, 600)], {
      enabled: true,
      style: "highlight",
    });
    act(() => publishPlayheadMs(0));
    const { container } = render(<KaraokeOverlay project={project} />);
    let spans = container.querySelectorAll("[data-state]");
    expect(spans[0].getAttribute("data-state")).toBe("active");

    act(() => publishPlayheadMs(400));
    spans = container.querySelectorAll("[data-state]");
    expect(spans[0].getAttribute("data-state")).toBe("done");
    expect(spans[1].getAttribute("data-state")).toBe("active");
  });
});
