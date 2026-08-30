/**
 * Panel edits are undoable — end to end (R1 trust round).
 *
 * TranslatePanel is the worst case of the bug this pins: "Replace captions"
 * rewrites the text of EVERY caption in the project, and it used to land via
 * `useProjectStore.setProject`, which touched neither `past` nor `future`. The
 * panel's own docstring promised "the editor's undo can revert it". It could
 * not. A mistaken target language meant re-transcribing or reverting to the
 * last manual save.
 *
 * This wires the panel exactly the way `App.tsx` does — `onProjectChange` is
 * the store's `commit` — mounts the real ⌘Z hotkey, and drives the whole path
 * through the real `ipc` wrappers (only Tauri's `invoke` is mocked, so a drift
 * in command names or argument shapes fails here too).
 */

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";

import { TranslatePanel } from "./TranslatePanel";
import { SAMPLE_PROJECT } from "@/lib/sampleProject";
import { useLocale } from "@/lib/i18n";
import {
  selectCanRedo,
  useProjectStore,
  useUndoHotkeys,
} from "@/lib/useProjectStore";
import type { Project, TranslationResult } from "@/lib/bindings";

const invoke = vi.fn();
vi.mock("@tauri-apps/api/core", () => ({
  invoke: (...args: unknown[]) => invoke(...args),
  isTauri: () => false,
}));

/** The captions the mocked backend hands back — every word replaced. */
const TRANSLATED = SAMPLE_PROJECT.captions.map((c) => ({
  ...c,
  words: c.words.map((w) => ({ ...w, text: `EN:${w.text}` })),
}));

const RESULT: TranslationResult = {
  captions: TRANSLATED,
  target_language: "en",
  warnings: [],
} as unknown as TranslationResult;

function firstWord(p: Project | null): string {
  return p?.captions[0]?.words[0]?.text ?? "";
}

/** App wires every dock panel's `onProjectChange` to the store's `commit`. */
function Harness() {
  const project = useProjectStore((s) => s.project);
  const commit = useProjectStore((s) => s.commit);
  useUndoHotkeys();
  if (!project) return null;
  return <TranslatePanel project={project} onProjectChange={commit} />;
}

beforeEach(() => {
  invoke.mockReset();
  useLocale.setState({ lang: "en" });
  useProjectStore.setState({
    project: null,
    past: [],
    future: [],
    busy: false,
    inFlight: false,
    savedSnapshot: null,
    filePath: null,
    saving: false,
  });
  useProjectStore.getState().reset(SAMPLE_PROJECT, "/tmp/talk.sundayedit");
  invoke.mockImplementation((cmd: string) => {
    switch (cmd) {
      case "translate_supported_languages":
        return Promise.resolve([{ code: "en", name: "English" }]);
      case "translate_estimate":
        return Promise.resolve({
          caption_count: SAMPLE_PROJECT.captions.length,
          estimated_cost_usd: 0.01,
          input_tokens: 10,
          output_tokens: 10,
        });
      case "translate_captions":
        return Promise.resolve(RESULT);
      default:
        return Promise.reject(new Error(`unexpected command: ${cmd}`));
    }
  });
});

afterEach(() => {
  cleanup();
});

describe("TranslatePanel → undo", () => {
  it("⌘Z restores the pre-translation captions", async () => {
    const original = firstWord(SAMPLE_PROJECT);
    render(<Harness />);

    // Run, then commit the translation onto the project.
    fireEvent.click(await screen.findByText(/Translate to English/));
    fireEvent.click(await screen.findByText("Replace captions"));

    await vi.waitFor(() =>
      expect(firstWord(useProjectStore.getState().project)).toBe(
        `EN:${original}`,
      ),
    );
    // The whole track was rewritten — this is exactly the edit that used to be
    // unrecoverable.
    expect(
      useProjectStore
        .getState()
        .project!.captions.every((c) =>
          c.words.every((w) => w.text.startsWith("EN:")),
        ),
    ).toBe(true);

    // ⌘Z on the document (not in a field) must put every caption back.
    fireEvent.keyDown(document, { key: "z", metaKey: true });

    expect(useProjectStore.getState().project).toBe(SAMPLE_PROJECT);
    expect(firstWord(useProjectStore.getState().project)).toBe(original);
    expect(selectCanRedo(useProjectStore.getState())).toBe(true);
  });

  it("marks the project dirty so the save indicator and close guard fire", async () => {
    render(<Harness />);
    fireEvent.click(await screen.findByText(/Translate to English/));
    fireEvent.click(await screen.findByText("Replace captions"));

    await vi.waitFor(() =>
      expect(useProjectStore.getState().project).not.toBe(SAMPLE_PROJECT),
    );
    expect(useProjectStore.getState().savedSnapshot).toBe(SAMPLE_PROJECT);
  });
});
