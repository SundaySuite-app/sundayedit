/**
 * The E2E mock's own contract (R1 trust round).
 *
 * `mock-backend.ts` used to end its dispatch with
 * `default: return Promise.resolve(undefined)`. With 46 case arms against 107
 * registered Tauri commands, that made the whole E2E layer structurally unable
 * to fail on the thing it exists to check: a spec exercising a renamed,
 * removed, or mistyped command got `undefined`, the UI treated it as a
 * benign-but-empty result, and the spec passed green.
 *
 * These two specs are the guard on the guard. The first pins the throw itself
 * — without it, a future "just resolve undefined so boot stops complaining"
 * would silently restore the blind spot. The second walks the concrete
 * instance that was live: `compose_preview_proxy` had no case, so
 * `renderPreviewProxy` resolved `true` and the timeline announced "Preview
 * rendered" for a file that was never written, to an output path that was
 * literally `undefined` (the path plugin had no case either).
 */

import { expect, test } from "@playwright/test";

import { openDemoProject } from "./fixtures/mock-backend";

type ProxyRender = { output: string; items: number };

test("the mock rejects a command it has no case for", async ({ page }) => {
  await openDemoProject(page);

  const outcome = await page.evaluate(async () => {
    const internals = (
      window as unknown as {
        __TAURI_INTERNALS__: {
          invoke: (cmd: string, args: unknown) => Promise<unknown>;
        };
      }
    ).__TAURI_INTERNALS__;
    try {
      await internals.invoke("op_this_command_does_not_exist", {});
      return "resolved";
    } catch (e) {
      return (e as Error).message;
    }
  });

  // Not "resolved": a spec can no longer pass by driving a command that the
  // backend does not have.
  expect(outcome).toContain("unhandled command");
  expect(outcome).toContain("op_this_command_does_not_exist");
});

test("render-preview actually reaches compose_preview_proxy with a real output path", async ({
  page,
}) => {
  await openDemoProject(page, { tauri: true });

  const button = page.getByRole("button", { name: /Gjengi forhåndsvisning/i });
  await expect(button).toBeVisible();
  await button.click();

  // The UI's claim…
  await expect(
    page.getByRole("button", { name: /Forhåndsvisning gjengitt/i }),
  ).toBeVisible();

  // …must be backed by a call that carried somewhere to write to. Before the
  // mock threw on unknown commands, `appCacheDir()` resolved `undefined`, the
  // joined path was `undefined`, and the button turned green anyway.
  const renders = await page.evaluate(
    () =>
      (window as unknown as { __mockProxyRenders: ProxyRender[] })
        .__mockProxyRenders,
  );
  expect(renders).toHaveLength(1);
  expect(renders[0].output).toMatch(/^\/.+sundayedit-preview\.mp4$/);
  // …and it flattened a timeline that actually had clips on it.
  expect(renders[0].items).toBeGreaterThan(0);
});
