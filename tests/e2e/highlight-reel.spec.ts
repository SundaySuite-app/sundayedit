import { test, expect } from "@playwright/test";

import { openDemoProject } from "./fixtures/mock-backend";

// Sermon Highlight Reel Studio, end-to-end through the real UI:
// storyboard → fan-out plan → batch render → per-item progress → done.
//
// The studio is guarded behind `isTauri()` (it needs a native folder picker and
// ffmpeg), so this spec opens the demo project with the Tauri host flag. The
// mock backend answers all four `reel_*` commands: a keyless storyboard (two
// clips, `used_ai:false`), the same clip × preset fan-out Rust produces, and a
// batch render that streams `reel-render-progress` before resolving.
//
// The locale is Norwegian — that is what `openDemoProject` pins.

test.beforeEach(async ({ page }) => {
  await openDemoProject(page, { tauri: true });
  // Each simulated encode is slow enough that the in-flight per-item state is
  // reliably observable, and short enough for a test. Read by the mock when the
  // batch starts, so setting it after load is fine.
  await page.evaluate(() => {
    (
      window as unknown as { __mockReelItemDelayMs: number }
    ).__mockReelItemDelayMs = 700;
  });
});

test("highlight reel goes storyboard → plan → render → progress → done", async ({
  page,
}) => {
  // Open the studio from the left rail.
  await page.getByRole("button", { name: "Høydepunkter" }).click();
  await expect(page.getByTestId("reel-panel")).toBeVisible();

  // 1. Storyboard. No API key typed, so the backend answers with the keyless
  //    pause heuristic — and the panel must SAY so rather than implying AI.
  await page.getByRole("button", { name: "Foreslå storyboard" }).click();
  await expect(page.getByTestId("reel-mode")).toContainText("Ingen API-nøkkel");
  await expect(page.getByTestId("reel-clip")).toHaveCount(2);

  // 2. Output folder — the mock's dialog returns whatever we point it at.
  await page.evaluate(() => {
    (window as unknown as { __mockDialogPath?: string }).__mockDialogPath =
      "/demo/reels";
  });
  await page.getByRole("button", { name: /Ingen mappe valgt/ }).click();
  await expect(page.getByTestId("reel-outdir")).toHaveText("/demo/reels");

  // 3. The fan-out: 2 clips × the one portrait preset the catalog offers. The
  //    files listed here are the files the render will write — same command.
  await expect(page.getByTestId("reel-plan")).toContainText("2 fil(er)");

  // 4. Render the batch.
  await page.getByTestId("reel-render-all").click();
  await expect(page.getByTestId("reel-progress")).toBeVisible();

  // Progress actually streams from the emitted events: some item reaches the
  // "rendering" state while the batch is in flight.
  await expect(
    page.locator('[data-testid="reel-item"][data-item-state="rendering"]'),
  ).toHaveCount(1);
  await expect(page.getByTestId("reel-progress-count")).toContainText("av 2");

  // 5. Done — both files reported, both rows marked finished.
  await expect(page.getByTestId("reel-done")).toBeVisible();
  await expect(page.getByTestId("reel-done")).toContainText("/demo/reels");
  await expect(
    page.locator('[data-testid="reel-item"][data-item-state="done"]'),
  ).toHaveCount(2);
  await expect(page.getByTestId("reel-failed")).toHaveCount(0);

  // Dismiss the overlay.
  await page
    .getByTestId("reel-progress")
    .getByRole("button", { name: "Lukk" })
    .click();
  await expect(page.getByTestId("reel-progress")).toHaveCount(0);
});
