/**
 * ExportConfigPanel — karaoke section (E4a). The rest of the panel's
 * controls are covered by `exportConfig.test.ts`'s pure patch-helper tests;
 * this file focuses on the new karaoke on/off + style + confidence-tint UI,
 * since it introduces a second nested `patchKaraoke` helper worth pinning.
 */

import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";

import { ExportConfigPanel } from "./ExportConfigPanel";
import { SAMPLE_PROJECT } from "@/lib/sampleProject";
import { useLocale } from "@/lib/i18n";
import type { Project } from "@/lib/bindings";

// The locale store defaults to Norwegian ("no") and persists across tests —
// pin English so role-name queries below match the strings asserted against.
beforeEach(() => {
  useLocale.setState({ lang: "en" });
});

afterEach(cleanup);

function renderPanel(project: Project = SAMPLE_PROJECT) {
  let latest = project;
  const onProjectChange = (p: Project) => {
    latest = p;
  };
  const utils = render(
    <ExportConfigPanel project={project} onProjectChange={onProjectChange} />,
  );
  return { ...utils, getLatest: () => latest };
}

describe("ExportConfigPanel — karaoke", () => {
  it("is off and collapsed by default (SAMPLE_PROJECT has no export_config.karaoke)", () => {
    renderPanel();
    const toggle = screen.getByRole("checkbox", {
      name: /karaoke/i,
    }) as HTMLInputElement;
    expect(toggle.checked).toBe(false);
    // The style/tint controls only appear once enabled.
    expect(screen.queryByRole("button", { name: /highlight/i })).toBeNull();
  });

  it("enabling it patches export_config.karaoke with the default options", () => {
    const { getLatest } = renderPanel();
    fireEvent.click(screen.getByRole("checkbox", { name: /karaoke/i }));
    expect(getLatest().export_config.karaoke).toMatchObject({
      enabled: true,
      style: "highlight",
      confidence_tint: false,
    });
  });

  it("reveals fill-style and confidence-tint controls once enabled", () => {
    const project: Project = {
      ...SAMPLE_PROJECT,
      export_config: {
        ...SAMPLE_PROJECT.export_config,
        karaoke: {
          enabled: true,
          style: "highlight",
          pending_color: "#7A7A7A",
          confidence_tint: false,
          confidence_threshold: 70,
          low_confidence_color: "#F5A524",
        },
      },
    };
    const { getLatest } = renderPanel(project);
    const sweepButton = screen.getByRole("button", { name: /sweep/i });
    fireEvent.click(sweepButton);
    expect(getLatest().export_config.karaoke).toMatchObject({ style: "sweep" });

    const tintToggle = screen.getByRole("checkbox", { name: /confidence/i });
    fireEvent.click(tintToggle);
    expect(getLatest().export_config.karaoke).toMatchObject({
      confidence_tint: true,
    });
  });

  it("preserves the rest of KaraokeOptions when patching a single field (no reset to defaults)", () => {
    const project: Project = {
      ...SAMPLE_PROJECT,
      export_config: {
        ...SAMPLE_PROJECT.export_config,
        karaoke: {
          enabled: true,
          style: "sweep",
          pending_color: "#123456",
          confidence_tint: true,
          confidence_threshold: 55,
          low_confidence_color: "#abcdef",
        },
      },
    };
    const { getLatest } = renderPanel(project);
    fireEvent.click(screen.getByRole("button", { name: /highlight/i }));
    expect(getLatest().export_config.karaoke).toEqual({
      enabled: true,
      style: "highlight",
      pending_color: "#123456",
      confidence_tint: true,
      confidence_threshold: 55,
      low_confidence_color: "#abcdef",
    });
  });
});
