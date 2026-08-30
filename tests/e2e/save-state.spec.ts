/**
 * The saved / unsaved indicator (R1 trust round).
 *
 * SundayEdit had no dirty tracking at all: nothing in the shell told the user
 * whether the last hour of caption corrections existed anywhere but in memory,
 * and nothing warned before it was thrown away. The indicator is the visible
 * half of that fix (autosave and the close guard are the Tauri-only halves,
 * unit-tested in `src/lib/autosave.test.tsx`).
 *
 * Driven here through the real editing path — a word edit round-trips through
 * `ipc.ops.editWord` and commits to the store — so the badge is proven to
 * react to an actual edit, not to a synthetic store poke.
 */

import { expect, test } from "@playwright/test";

import { openDemoProject } from "./fixtures/mock-backend";

test("the topbar reports saved, then unsaved after a real edit", async ({
  page,
}) => {
  await openDemoProject(page);

  const state = page.getByTestId("save-state");
  // A freshly opened project matches what is on disk.
  await expect(state).toHaveAttribute("data-dirty", "0");
  await expect(state).toHaveText(/Lagret/);

  // Correct a low-confidence word — the flagship interaction.
  await page.getByRole("button", { name: "kerigma" }).click();
  const input = page.locator("input:focus");
  await input.fill("kerygma");
  await input.press("Enter");
  await expect(page.getByRole("button", { name: "kerygma" })).toBeVisible();

  await expect(state).toHaveAttribute("data-dirty", "1");
  await expect(state).toHaveText(/Ulagrede endringer/);
});

test("undoing back to the opened state reports saved again", async ({
  page,
}) => {
  await openDemoProject(page);
  const state = page.getByTestId("save-state");

  await page.getByRole("button", { name: "kerigma" }).click();
  const input = page.locator("input:focus");
  await input.fill("kerygma");
  await input.press("Enter");
  await expect(state).toHaveAttribute("data-dirty", "1");

  // Snapshots are immutable, so undoing to the opened snapshot is byte-identity
  // with the file — the badge must stop nagging rather than latch on forever.
  await page.locator("body").click();
  await page.keyboard.press("ControlOrMeta+z");
  await expect(page.getByRole("button", { name: "kerigma" })).toBeVisible();
  await expect(state).toHaveAttribute("data-dirty", "0");
});
