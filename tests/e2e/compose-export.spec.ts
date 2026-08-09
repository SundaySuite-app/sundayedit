import { test, expect } from "@playwright/test";

import { openDemoProject } from "./fixtures/mock-backend";

// Multi-track compose export, end-to-end through the real UI.
//
// The compose surface is guarded behind `isTauri()` (it needs a native save
// dialog + the ffmpeg compose engine), so this spec opens the demo project with
// the Tauri host flag set. The mock backend returns a deterministic output path
// for the save picker and drives `compose_render` — emitting a couple of
// progress ticks then resolving — so the whole button → progress → done flow is
// exercised without a real ffmpeg.

test.beforeEach(async ({ page }) => {
  await openDemoProject(page, { tauri: true });
});

test("compose export runs the render and reports progress then success", async ({
  page,
}) => {
  // Open the export modal from the topbar.
  await page.getByRole("button", { name: "Eksport" }).click();

  // The compose action lives in the left column (NLE mode + Tauri host).
  const compose = page.getByRole("button", {
    name: /eksporter komponert video/i,
  });
  await expect(compose).toBeVisible();
  await compose.click();

  // The progress overlay appears (save dialog resolved the output path), then
  // the render resolves and the success line shows the saved path.
  await expect(page.getByTestId("compose-progress")).toBeVisible();
  await expect(page.getByTestId("compose-done")).toBeVisible();
  await expect(page.getByTestId("compose-done")).toContainText("/demo/out.mp4");

  // Dismiss the overlay (scope to it — the export modal also has a "Lukk").
  await page
    .getByTestId("compose-progress")
    .getByRole("button", { name: "Lukk" })
    .click();
  await expect(page.getByTestId("compose-progress")).toHaveCount(0);
});
