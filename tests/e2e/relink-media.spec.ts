import { test, expect, type Page } from "@playwright/test";

import { openDemoProject } from "./fixtures/mock-backend";

// Missing-media detection + relink, end-to-end.
//
// The mock backend's `check_media_paths` treats any pooled path under
// `/missing/` as gone and everything else as present (see the fixture); its
// `project_relink` (auto-search) always reports "not found" so these specs
// exercise the renderer's real dialog fallback, which honours
// `window.__mockDialogPath` (set below) exactly like the real native picker
// would return whatever the user picked.

test.beforeEach(async ({ page }) => {
  // Missing-media detection is gated on `isTauri()` (no fs to stat in a plain
  // browser) — opt into the mock's Tauri-host flag so it actually runs.
  await openDemoProject(page, { tauri: true });
});

/** Import media through the real UI, steering the mock file dialog. */
async function importVia(page: Page, path: string) {
  await page.evaluate((p) => {
    (window as unknown as { __mockDialogPath?: string }).__mockDialogPath = p;
  }, path);
  await page.getByRole("button", { name: "Medier" }).click();
  await page.getByRole("button", { name: /importer media/i }).click();
}

test("a missing source file is flagged in the bin and in the app-wide banner", async ({
  page,
}) => {
  await importVia(page, "/missing/gone.mp4");
  await expect(page.getByTestId("relink-media")).toBeVisible();

  // The bin row: warning state + "Finn filen…" action + the last-known path.
  await expect(page.getByTestId("relink-media")).toBeVisible();
  await expect(page.getByText(/Fil mangler/)).toBeVisible();

  // The app-wide banner catches it too, regardless of which dock tool is
  // focused — it counts the one missing file.
  await expect(page.getByTestId("missing-media-banner")).toContainText(
    "1 fil(er) mangler.",
  );
});

test("Finn filen… falls back to the file dialog and clears the missing state", async ({
  page,
}) => {
  await importVia(page, "/missing/gone.mp4");
  const relinkButton = page.getByTestId("relink-media");
  await expect(relinkButton).toBeVisible();

  // Auto-search (project_relink) always misses in the mock — point the
  // dialog fallback at a path the mock's fake filesystem treats as present.
  await page.evaluate(() => {
    (window as unknown as { __mockDialogPath?: string }).__mockDialogPath =
      "/found/gone.mp4";
  });
  await relinkButton.click();

  // op_relink_media committed through the shared undo stack: the row no
  // longer offers "Finn filen…", the banner is gone, and the new path shows.
  await expect(page.getByTestId("relink-media")).toHaveCount(0);
  await expect(page.getByTestId("missing-media-banner")).toHaveCount(0);
  await expect(page.getByTitle("/found/gone.mp4")).toBeVisible();
  await expect(page.getByTestId("relink-status")).toHaveText(
    "Koblet til på nytt.",
  );
});

test("a duration change on the relinked file surfaces the timing warning", async ({
  page,
}) => {
  await importVia(page, "/missing/gone.mp4");
  await expect(page.getByTestId("relink-media")).toBeVisible();

  // The mock's op_relink_media reports a different (shorter) duration for
  // any path containing "short".
  await page.evaluate(() => {
    (window as unknown as { __mockDialogPath?: string }).__mockDialogPath =
      "/found/short-take.mp4";
  });
  await page.getByTestId("relink-media").click();

  await expect(page.getByTestId("relink-status")).toContainText(
    /varighet|forskjøvet/i,
  );
});
