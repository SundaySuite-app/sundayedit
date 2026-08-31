import { test, expect, type Page } from "@playwright/test";

import { openDemoProject } from "./fixtures/mock-backend";

// NLE multi-track timeline, end-to-end.
//
// Two layers, mirroring timeline-operations.spec.ts:
//
//  1. Real UI where a plain click drives it — the media bin (open the dock,
//     import a clip, add a track) and the sample clip box rendered on the video
//     lane. These exercise React → ipc.timeline.* → invoke → useProjectStore →
//     the lanes re-render, which is the whole point of the E2E layer.
//
//  2. The clip drags (place / trim / split / move / ripple / transition /
//     transform / text) have no toolbar trigger — they're pointer-capture edge
//     drags + HTML5 dnd, unclickable in `vite preview` exactly like the caption
//     move/resize ops. We guard their IPC contract directly: the command name +
//     camelCase args ipc.timeline sends, plus the semantic round-trip (split
//     yields two items, ripple slides later clips left, transitions attach…).
//     The op maths mirrors services/operations.rs and is asserted there.

test.beforeEach(async ({ page }) => {
  await openDemoProject(page);
});

// ── real-UI: media bin ──────────────────────────────────────────────────────

test("media bin opens from the dock and imports a clip through the UI", async ({
  page,
}) => {
  // The dock defaults to Context; the "Medier" nav icon swaps in the MediaBin.
  await page.getByRole("button", { name: "Medier" }).click();
  await expect(
    page.getByRole("button", { name: /importer media/i }),
  ).toBeVisible();

  // Import → mock dialog returns a path → op_import_media appends the media →
  // the store re-renders → a new row (basename "broll.mp4") shows in the bin.
  await page.getByRole("button", { name: /importer media/i }).click();
  await expect(page.getByText("broll.mp4")).toBeVisible();
});

test("media bin adds a track and the lane appears in the timeline gutter", async ({
  page,
}) => {
  await page.getByRole("button", { name: "Medier" }).click();
  // "Videospor" (video track) button → op_add_track appends a track named
  // "Videospor"; its header renders in the timeline's left gutter.
  await page.getByRole("button", { name: "Videospor" }).click();
  // The new track's header renders in the timeline gutter (inside <main>), a
  // distinct node from the bin's "Videospor" add-track button in the dock.
  await expect(page.getByRole("main").getByText("Videospor")).toBeVisible();
});

// ── real-UI: the sample clip renders as a box on the video lane ──────────────

test("the sample project renders its placed clip as a box on the video lane", async ({
  page,
}) => {
  // SAMPLE_PROJECT places media m1 (sermon.mp4) on the video track as clip ti1;
  // the ClipBox carries the source filename as its title. (The bin row uses the
  // full path as its title, so this matches only the timeline clip.)
  await expect(page.getByTitle("sermon.mp4")).toBeVisible();
});

// ── real-UI: clip selection → inspector → transition ─────────────────────────

test("selecting a clip opens the inspector and a transition can be set", async ({
  page,
}) => {
  // Clicking the placed clip box selects it → the inspector floats in over the
  // workspace (App renders it from the lifted selectedClipId). The 224px docked
  // timeline overlaps the clip's centre with the help footer, so dispatch the
  // click straight at the box (same onClick React handles) rather than hit-test.
  await page.getByTitle("sermon.mp4").dispatchEvent("click");
  const inspector = page.getByTestId("clip-inspector");
  await expect(inspector).toBeVisible();

  // The duration field only exists once a transition is attached — absent first.
  await expect(inspector.getByText("Varighet (ms)")).toHaveCount(0);

  // Pick a crossfade lead-in (real xfade name "dissolve" — the picker only
  // offers names ffmpeg accepts) → op_set_transition commits through the store
  // → the inspector re-reads the item and reveals the duration field.
  await inspector.getByRole("combobox").selectOption("dissolve");
  await expect(inspector.getByRole("combobox")).toHaveValue("dissolve");
  await expect(inspector.getByText("Varighet (ms)")).toBeVisible();
});

// ── real-UI: split via the inspector button ──────────────────────────────────

test("the inspector's split button blades the selected clip in two", async ({
  page,
}) => {
  // Select the clip FIRST — selection parks the playhead at the clip start —
  // then nudge the playhead off the edge (ArrowRight = one frame ≈ 33 ms,
  // inside the clip's 0..18000 span) so the split button arms. The timeline's
  // keyboard surface must hold focus for its handler to receive the key.
  await page.getByTitle("sermon.mp4").dispatchEvent("click");
  const inspector = page.getByTestId("clip-inspector");
  await expect(inspector).toBeVisible();
  await page.getByTestId("timeline").focus();
  await page.keyboard.press("ArrowRight");

  // op_split_timeline_item → two clips of the same source render on the lane.
  const split = inspector.getByTestId("inspector-split");
  await expect(split).toBeEnabled();
  await split.click();
  await expect(page.getByTitle("sermon.mp4")).toHaveCount(2);
});

// ── real-UI: Delete ripple-deletes the selected clip ─────────────────────────

test("the Delete key ripple-deletes the selected clip", async ({ page }) => {
  await page.getByTestId("timeline").focus();
  await page.getByTitle("sermon.mp4").dispatchEvent("click");
  await page.keyboard.press("Delete");
  // op_ripple_delete_item removed the only clip — its box is gone.
  await expect(page.getByTitle("sermon.mp4")).toHaveCount(0);
});

// ── real-UI: the inspector's delete button ripple-deletes too (E3-UI) ────────

test("the inspector's delete button ripple-deletes the selected clip and closes the panel", async ({
  page,
}) => {
  await page.getByTitle("sermon.mp4").dispatchEvent("click");
  const inspector = page.getByTestId("clip-inspector");
  await expect(inspector).toBeVisible();

  await inspector.getByTestId("inspector-delete").click();
  // op_ripple_delete_item removed the only clip — its box AND the inspector
  // (nothing left to inspect) are gone.
  await expect(page.getByTitle("sermon.mp4")).toHaveCount(0);
  await expect(inspector).toBeHidden();
});

// ── real-UI: the track header's close-gaps button (E3-UI) ────────────────────

test("the close-gaps track button is offered on a video track but not on captions", async ({
  page,
}) => {
  await expect(
    page.getByTestId("track-header-tv").getByTestId("pack-track-tv"),
  ).toBeVisible();
  // Gaps live in TimelineItems; the caption track's content is a separate
  // `captions` array, so there is nothing for pack_track to act on there.
  await expect(
    page.getByTestId("track-header-tc").getByTestId("pack-track-tc"),
  ).toHaveCount(0);
});

test("clicking close-gaps commits op_pack_track without disturbing an already-packed track", async ({
  page,
}) => {
  // The demo project's one clip already sits at 0 on tv — packing is a no-op,
  // but the click must still round-trip cleanly through the shared undo stack
  // (no alert, the clip box survives) rather than erroring or vanishing state.
  await page
    .getByTestId("track-header-tv")
    .getByTestId("pack-track-tv")
    .dispatchEvent("click");
  await expect(page.getByTitle("sermon.mp4")).toBeVisible();
  await expect(page.getByRole("alert")).toHaveCount(0);
});

// ── real-UI: remove-track rejection surfaces in the timeline ─────────────────

test("removing a non-empty track surfaces the backend rejection", async ({
  page,
}) => {
  // The Video track (id "tv") still carries clip ti1 → op_remove_track rejects
  // with a validation message the timeline shows as an alert strip. The gutter
  // header sits partially clipped in the 224px docked timeline, so dispatch
  // the click like the clip-box tests do.
  await page
    .getByTestId("track-header-tv")
    .getByTestId("remove-track")
    .dispatchEvent("click");
  await expect(page.getByRole("alert")).toContainText("is not empty");
});

// ── real-UI: remove-media rejection surfaces in the bin ──────────────────────

test("removing media still referenced by a clip surfaces the rejection", async ({
  page,
}) => {
  await page.getByRole("button", { name: "Medier" }).click();
  // m1 backs clip ti1 → op_remove_media rejects; the bin's error banner shows.
  await page.getByTestId("remove-media").click();
  await expect(page.getByRole("alert")).toContainText("still referenced");
});

// ── real-UI: R2 audio controls ────────────────────────────────────────────────

test("the inspector's gain slider commits through the store, with a reset once it drifts from 0 dB", async ({
  page,
}) => {
  await page.getByTitle("sermon.mp4").dispatchEvent("click");
  const inspector = page.getByTestId("clip-inspector");
  const gain = inspector.getByTestId("inspector-gain");
  await expect(gain).toHaveValue("0");
  await expect(inspector.getByTestId("inspector-gain-reset")).toHaveCount(0);

  // Range inputs aren't `.fill()`-able; arrow keys move by `step` (0.5 dB).
  await gain.focus();
  await page.keyboard.press("ArrowLeft");
  await expect(gain).toHaveValue("-0.5");
  // op_set_item_audio landed — the clip box survives, nothing rejected.
  await expect(page.getByTitle("sermon.mp4")).toBeVisible();
  await expect(page.getByRole("alert")).toHaveCount(0);
  await expect(inspector.getByTestId("inspector-gain-reset")).toBeVisible();

  // The reset affordance commits exactly 0 dB and then hides itself.
  await inspector.getByTestId("inspector-gain-reset").click();
  await expect(gain).toHaveValue("0");
  await expect(inspector.getByTestId("inspector-gain-reset")).toHaveCount(0);
});

test("fade in/out commit through the inspector's number fields", async ({
  page,
}) => {
  await page.getByTitle("sermon.mp4").dispatchEvent("click");
  const inspector = page.getByTestId("clip-inspector");
  // Norwegian labels — `openDemoProject` boots the editor in "no".
  const fadeIn = inspector.getByLabel("Inntoning (ms)");
  const fadeOut = inspector.getByLabel("Uttoning (ms)");

  await fadeIn.fill("500");
  await fadeIn.blur();
  await expect(fadeIn).toHaveValue("500");

  await fadeOut.fill("300");
  await fadeOut.blur();
  await expect(fadeOut).toHaveValue("300");

  // Both round-tripped through op_set_item_audio without a rejection.
  await expect(page.getByRole("alert")).toHaveCount(0);
});

test("the track header's fader adjusts the video track's volume and resets on double-click", async ({
  page,
}) => {
  const volume = page.getByTestId("track-volume-tv");
  await expect(volume).toHaveValue("0");

  await volume.focus();
  await page.keyboard.press("ArrowRight");
  await expect(volume).toHaveValue("0.5");
  await expect(page.getByRole("alert")).toHaveCount(0);

  // The docked timeline's header area overlaps this row (same reason other
  // tests in this file `dispatchEvent("click")` the clip box instead of a
  // real click) — dispatch the dblclick directly rather than fighting it.
  await volume.dispatchEvent("dblclick");
  await expect(volume).toHaveValue("0");
});

// ── IPC-contract drivers (clip ops with no clickable trigger) ────────────────

test("add-timeline-item op places a clip on a track", async ({ page }) => {
  const next = await invoke<DemoProject>(page, "op_add_timeline_item", {
    project: demoProject(),
    trackId: "tv",
    sourceMediaId: "m1",
    inMs: 0,
    outMs: 5000,
    timelineStartMs: 20_000,
    kind: "av",
  });
  // The original clip plus the new one; the new one lands on the target track.
  expect(next.timeline_items).toHaveLength(2);
  const added = next.timeline_items[1];
  expect(added.track_id).toBe("tv");
  expect(added.source_media_id).toBe("m1");
  expect(added.timeline_start_ms).toBe(20_000);
  expect(added.speed).toBe(1);
});

test("split-timeline-item op cuts one clip into two at the split point", async ({
  page,
}) => {
  // ti1 spans timeline 0..18000 (in 0, out 18000, speed 1). Splitting at 6000
  // maps back to source cut 6000: left keeps [0,6000], right takes [6000,18000].
  const next = await invoke<DemoProject>(page, "op_split_timeline_item", {
    project: demoProject(),
    itemId: "ti1",
    atTimelineMs: 6000,
  });
  expect(next.timeline_items).toHaveLength(2);
  const [left, right] = next.timeline_items;
  expect(left.out_ms).toBe(6000);
  expect(right.in_ms).toBe(6000);
  expect(right.timeline_start_ms).toBe(6000);
  expect(right.id).not.toBe(left.id);
});

test("split-timeline-item op rejects a cut outside the clip", async ({
  page,
}) => {
  const error = await invokeError(page, "op_split_timeline_item", {
    project: demoProject(),
    itemId: "ti1",
    atTimelineMs: 99_999,
  });
  expect(error.code).toBe("validation");
});

test("trim-timeline-item op moves only the edges it's given", async ({
  page,
}) => {
  const next = await invoke<DemoProject>(page, "op_trim_timeline_item", {
    project: demoProject(),
    itemId: "ti1",
    newInMs: 1000,
    newOutMs: null,
    newTimelineStartMs: 1000,
  });
  const it = next.timeline_items[0];
  expect(it.in_ms).toBe(1000);
  expect(it.timeline_start_ms).toBe(1000);
  expect(it.out_ms).toBe(18_000); // untouched — null edge left alone
});

test("move-timeline-item op relocates a clip across tracks and time", async ({
  page,
}) => {
  const next = await invoke<DemoProject>(page, "op_move_timeline_item", {
    project: demoProject(),
    itemId: "ti1",
    newTrackId: "tv2",
    newTimelineStartMs: 4000,
  });
  const it = next.timeline_items[0];
  expect(it.track_id).toBe("tv2");
  expect(it.timeline_start_ms).toBe(4000);
});

test("ripple-delete op removes a clip and slides later clips left", async ({
  page,
}) => {
  // Two clips on tv: ti1 [0,18000] and ti2 starting at 20000. Ripple-deleting
  // ti1 (span 18000) removes it and pulls ti2 left by the gap.
  const project = demoProject();
  project.timeline_items.push({
    id: "ti2",
    track_id: "tv",
    kind: "av",
    source_media_id: "m1",
    in_ms: 0,
    out_ms: 4000,
    timeline_start_ms: 20_000,
    speed: 1,
    transform: identityTransform(),
    effects: [],
    transition_in: null,
    text: null,
    enabled: true,
    locked: false,
  });
  const next = await invoke<DemoProject>(page, "op_ripple_delete_item", {
    project,
    itemId: "ti1",
  });
  expect(next.timeline_items).toHaveLength(1);
  expect(next.timeline_items[0].id).toBe("ti2");
  expect(next.timeline_items[0].timeline_start_ms).toBe(2000); // 20000 − 18000
});

test("pack-track op closes every gap, left to right", async ({ page }) => {
  // ti1 [0,18000], then a 2000ms gap, then ti2 [20000,24000].
  const project = demoProject();
  project.timeline_items.push({
    id: "ti2",
    track_id: "tv",
    kind: "av",
    source_media_id: "m1",
    in_ms: 0,
    out_ms: 4000,
    timeline_start_ms: 20_000,
    speed: 1,
    transform: identityTransform(),
    effects: [],
    transition_in: null,
    text: null,
    enabled: true,
    locked: false,
  });
  const next = await invoke<DemoProject>(page, "op_pack_track", {
    project,
    trackId: "tv",
  });
  expect(
    next.timeline_items.find((it) => it.id === "ti1")!.timeline_start_ms,
  ).toBe(0);
  expect(
    next.timeline_items.find((it) => it.id === "ti2")!.timeline_start_ms,
  ).toBe(18_000); // packed straight up against ti1's end
});

test("pack-track op anchors a locked clip and leaves the gap in front of it alone", async ({
  page,
}) => {
  const project = demoProject();
  project.timeline_items[0].locked = true; // ti1 is the anchor
  project.timeline_items.push({
    id: "ti2",
    track_id: "tv",
    kind: "av",
    source_media_id: "m1",
    in_ms: 0,
    out_ms: 4000,
    timeline_start_ms: 20_000,
    speed: 1,
    transform: identityTransform(),
    effects: [],
    transition_in: null,
    text: null,
    enabled: true,
    locked: false,
  });
  const next = await invoke<DemoProject>(page, "op_pack_track", {
    project,
    trackId: "tv",
  });
  expect(
    next.timeline_items.find((it) => it.id === "ti1")!.timeline_start_ms,
  ).toBe(0); // locked — untouched
  expect(
    next.timeline_items.find((it) => it.id === "ti2")!.timeline_start_ms,
  ).toBe(18_000); // packs against the anchor's end, same as the unlocked case
});

test("set-transition then clear-transition round-trips a clip's lead-in", async ({
  page,
}) => {
  const withTransition = await invoke<DemoProject>(page, "op_set_transition", {
    project: demoProject(),
    itemId: "ti1",
    kind: "crossfade",
    durationMs: 500,
  });
  expect(withTransition.timeline_items[0].transition_in).toEqual({
    kind: "crossfade",
    duration_ms: 500,
  });
  const cleared = await invoke<DemoProject>(page, "op_clear_transition", {
    project: withTransition,
    itemId: "ti1",
  });
  expect(cleared.timeline_items[0].transition_in).toBeNull();
});

test("set-transform op replaces a clip's geometry", async ({ page }) => {
  const transform = {
    x: 10,
    y: 20,
    scale: 1.5,
    rotation_deg: 0,
    opacity: 0.8,
    crop: null,
  };
  const next = await invoke<DemoProject>(page, "op_set_transform", {
    project: demoProject(),
    itemId: "ti1",
    transform,
  });
  expect(next.timeline_items[0].transform).toEqual(transform);
});

// ── audio (R2) ───────────────────────────────────────────────────────────────
// The command names + camelCase args + clamp semantics `ipc.timeline.setItemAudio`
// / `setTrackVolume` send — mirrors `services::timeline_ops::set_item_audio` /
// `set_track_volume` (clamp to [-60, 12], NaN → unity, a fade clamped to the
// clip's OWN timeline length).

test("set-item-audio op sets gain and both fades, clamping out-of-range input", async ({
  page,
}) => {
  const next = await invoke<DemoProject>(page, "op_set_item_audio", {
    project: demoProject(),
    itemId: "ti1",
    gainDb: -6,
    fadeInMs: 500,
    // The demo clip is 18 000 ms long — a fade longer than the clip clamps
    // to the clip's own length, not the raw requested value.
    fadeOutMs: 99_000,
  });
  const item = next.timeline_items[0];
  expect(item.gain_db).toBe(-6);
  expect(item.fade_in_ms).toBe(500);
  expect(item.fade_out_ms).toBe(18_000);
});

test("set-item-audio op leaves omitted (null) fields untouched", async ({
  page,
}) => {
  const first = await invoke<DemoProject>(page, "op_set_item_audio", {
    project: demoProject(),
    itemId: "ti1",
    gainDb: -3,
    fadeInMs: 200,
    fadeOutMs: null,
  });
  const second = await invoke<DemoProject>(page, "op_set_item_audio", {
    project: first,
    itemId: "ti1",
    gainDb: null,
    fadeInMs: null,
    fadeOutMs: 300,
  });
  const item = second.timeline_items[0];
  expect(item.gain_db).toBe(-3); // untouched by the second call
  expect(item.fade_in_ms).toBe(200); // untouched by the second call
  expect(item.fade_out_ms).toBe(300);
});

test("set-track-volume op sets the fader, clamped to [-60, 12]", async ({
  page,
}) => {
  const next = await invoke<DemoProject>(page, "op_set_track_volume", {
    project: demoProject(),
    trackId: "tv",
    volumeDb: 3,
  });
  expect(next.tracks.find((t) => t.id === "tv")?.volume_db).toBe(3);

  const clamped = await invoke<DemoProject>(page, "op_set_track_volume", {
    project: demoProject(),
    trackId: "tv",
    volumeDb: 1000,
  });
  expect(clamped.tracks.find((t) => t.id === "tv")?.volume_db).toBe(12);
});

// ── curated effects (E6) ─────────────────────────────────────────────────────
// The registry is the preview↔export parity contract; these pin the IPC half
// of it — the command names + camelCase args ipc.timeline sends, and the
// one-entry-per-kind / clamp / reject-non-curated semantics the backend has.

test("set-effect op adds a curated effect with clamped params", async ({
  page,
}) => {
  const next = await invoke<DemoProject>(page, "op_set_effect", {
    project: demoProject(),
    itemId: "ti1",
    kind: "brightness",
    params: { amount: 9 },
    enabled: true,
  });
  expect(next.timeline_items[0].effects).toEqual([
    {
      id: "fx-brightness",
      kind: "brightness",
      params: { amount: 1 },
      enabled: true,
    },
  ]);
});

test("set-effect op updates in place instead of stacking a second entry", async ({
  page,
}) => {
  const once = await invoke<DemoProject>(page, "op_set_effect", {
    project: demoProject(),
    itemId: "ti1",
    kind: "contrast",
    params: { amount: 1.2 },
    enabled: true,
  });
  const twice = await invoke<DemoProject>(page, "op_set_effect", {
    project: once,
    itemId: "ti1",
    kind: "contrast",
    params: { amount: 1.8 },
    enabled: true,
  });
  expect(twice.timeline_items[0].effects).toHaveLength(1);
  expect(twice.timeline_items[0].effects[0].params).toEqual({ amount: 1.8 });
});

test("set-effect op rejects an effect the export cannot render", async ({
  page,
}) => {
  await expect(
    invoke<DemoProject>(page, "op_set_effect", {
      project: demoProject(),
      itemId: "ti1",
      kind: "bloom",
      params: {},
      enabled: true,
    }),
  ).rejects.toThrow(/curated/i);
});

test("remove-effect op drops only that kind", async ({ page }) => {
  const gray = await invoke<DemoProject>(page, "op_set_effect", {
    project: demoProject(),
    itemId: "ti1",
    kind: "grayscale",
    params: {},
    enabled: true,
  });
  const both = await invoke<DemoProject>(page, "op_set_effect", {
    project: gray,
    itemId: "ti1",
    kind: "saturation",
    params: { amount: 1.5 },
    enabled: true,
  });
  const next = await invoke<DemoProject>(page, "op_remove_effect", {
    project: both,
    itemId: "ti1",
    kind: "grayscale",
  });
  expect(next.timeline_items[0].effects.map((e) => e.kind)).toEqual([
    "saturation",
  ]);
});

test("add-text-item op places a standalone text clip (no source media)", async ({
  page,
}) => {
  const next = await invoke<DemoProject>(page, "op_add_text_item", {
    project: demoProject(),
    trackId: "to",
    timelineStartMs: 2000,
    durationMs: 3000,
    text: "Lower third",
  });
  const added = next.timeline_items[next.timeline_items.length - 1];
  expect(added.kind).toBe("text");
  expect(added.source_media_id).toBeNull();
  expect(added.text).toEqual({ text: "Lower third", style_id: null });
  expect(added.track_id).toBe("to");
});

test("set-item-text op replaces a text overlay's spec in place", async ({
  page,
}) => {
  const added = await invoke<DemoProject>(page, "op_add_text_item", {
    project: demoProject(),
    trackId: "to",
    timelineStartMs: 2000,
    durationMs: 3000,
    text: "Lower third",
  });
  const id = added.timeline_items[added.timeline_items.length - 1].id;
  const next = await invoke<DemoProject>(page, "op_set_item_text", {
    project: added,
    itemId: id,
    text: "Velkommen",
    styleId: "preset:tiktok_bold",
  });
  const edited = next.timeline_items.find((i) => i.id === id);
  expect(edited?.text).toEqual({
    text: "Velkommen",
    style_id: "preset:tiktok_bold",
  });
  // Nothing else about the clip moved.
  expect(edited?.timeline_start_ms).toBe(2000);
  expect(next.timeline_items).toHaveLength(added.timeline_items.length);
});

// ── media + track lifecycle ───────────────────────────────────────────────────

test("remove-media op is rejected while a clip still references it", async ({
  page,
}) => {
  const error = await invokeError(page, "op_remove_media", {
    project: demoProject(), // m1 is referenced by ti1
    mediaId: "m1",
  });
  expect(error.code).toBe("validation");
});

test("set-track-flags op toggles only the flags it's handed", async ({
  page,
}) => {
  const next = await invoke<DemoProject>(page, "op_set_track_flags", {
    project: demoProject(),
    trackId: "tv",
    enabled: null,
    locked: true,
    muted: null,
    solo: null,
  });
  const tv = next.tracks.find((t) => t.id === "tv")!;
  expect(tv.locked).toBe(true);
  expect(tv.enabled).toBe(true); // null flag left untouched
});

test("reorder-track op restacks and renumbers densely", async ({ page }) => {
  const next = await invoke<DemoProject>(page, "op_reorder_track", {
    project: demoProject(),
    trackId: "tc",
    newIndex: 0,
  });
  const byId = new Map(next.tracks.map((t) => [t.id, t.index]));
  expect(byId.get("tc")).toBe(0);
  // Indices stay a dense 0..n-1 permutation after the move.
  expect([...next.tracks.map((t) => t.index)].sort()).toEqual([0, 1]);
});

// ── probe + compose ───────────────────────────────────────────────────────────

test("video-probe op returns media metadata for a path", async ({ page }) => {
  const meta = await invoke<{ width: number; height: number; kind: string }>(
    page,
    "video_probe",
    { path: "/demo/clip.mp4" },
  );
  expect(meta.width).toBe(1920);
  expect(meta.height).toBe(1080);
  expect(meta.kind).toBe("video");
});

test("compose-render op emits progress ticks then resolves", async ({
  page,
}) => {
  const result = await page.evaluate(async () => {
    const ticks: number[] = [];
    const onProgress = (e: Event) => {
      ticks.push((e as CustomEvent).detail.fraction as number);
    };
    window.addEventListener("compose-render-progress", onProgress);
    const w = window as unknown as {
      __TAURI_INTERNALS__: {
        invoke: (c: string, a: Record<string, unknown>) => Promise<unknown>;
      };
    };
    await w.__TAURI_INTERNALS__.invoke("compose_render", {
      project: { name: "demo", language: "no", captions: [], speakers: [] },
      output: "/tmp/out.mp4",
      settings: {},
    });
    window.removeEventListener("compose-render-progress", onProgress);
    return ticks;
  });
  // A couple of progress ticks, ending on the completion tick.
  expect(result.length).toBeGreaterThanOrEqual(2);
  expect(result[result.length - 1]).toBe(1);
});

test("compose-cancel op resolves without error", async ({ page }) => {
  const ok = await page.evaluate(async () => {
    const w = window as unknown as {
      __TAURI_INTERNALS__: {
        invoke: (c: string, a: Record<string, unknown>) => Promise<unknown>;
      };
    };
    await w.__TAURI_INTERNALS__.invoke("compose_cancel", {});
    return true;
  });
  expect(ok).toBe(true);
});

// ── inline demo project + IPC helpers ────────────────────────────────────────
// `vite preview` serves the built bundle, so sampleProject.ts isn't importable
// in-page. We rebuild the NLE subset these ops need (mirroring SAMPLE_PROJECT:
// one video track + one caption track + one placed clip + its source media) and
// call the backend exactly as ipc.timeline.* does — same names + camelCase args.

type DemoTransform = {
  x: number;
  y: number;
  scale: number;
  rotation_deg: number;
  opacity: number;
  crop: null;
};
type DemoItem = {
  id: string;
  track_id: string;
  kind: string;
  source_media_id: string | null;
  in_ms: number;
  out_ms: number;
  timeline_start_ms: number;
  speed: number;
  gain_db: number;
  fade_in_ms: number;
  fade_out_ms: number;
  transform: DemoTransform;
  effects: unknown[];
  transition_in: { kind: string; duration_ms: number } | null;
  text: { text: string; style_id: string | null } | null;
  enabled: boolean;
  locked: boolean;
};
type DemoTrack = {
  id: string;
  kind: string;
  name: string;
  index: number;
  enabled: boolean;
  locked: boolean;
  muted: boolean;
  solo: boolean;
  volume_db: number;
};
type DemoProject = {
  name: string;
  language: string;
  captions: unknown[];
  speakers: unknown[];
  media: { id: string; original_filename: string }[];
  tracks: DemoTrack[];
  timeline_items: DemoItem[];
};

function identityTransform(): DemoTransform {
  return { x: 0, y: 0, scale: 1, rotation_deg: 0, opacity: 1, crop: null };
}

function track(
  id: string,
  kind: string,
  name: string,
  index: number,
): DemoTrack {
  return {
    id,
    kind,
    name,
    index,
    enabled: true,
    locked: false,
    muted: false,
    solo: false,
    volume_db: 0,
  };
}

function demoProject(): DemoProject {
  return {
    name: "demo",
    language: "no",
    captions: [],
    speakers: [],
    media: [{ id: "m1", original_filename: "sermon.mp4" }],
    tracks: [
      track("tv", "video", "Video", 0),
      track("tc", "caption", "Captions", 1),
    ],
    timeline_items: [
      {
        id: "ti1",
        track_id: "tv",
        kind: "av",
        source_media_id: "m1",
        in_ms: 0,
        out_ms: 18_000,
        timeline_start_ms: 0,
        speed: 1,
        gain_db: 0,
        fade_in_ms: 0,
        fade_out_ms: 0,
        transform: identityTransform(),
        effects: [],
        transition_in: null,
        text: null,
        enabled: true,
        locked: false,
      },
    ],
  };
}

// Resolve path: call the mock backend exactly as ipc.ts's `call` does.
async function invoke<T>(
  page: Page,
  cmd: string,
  args: Record<string, unknown>,
): Promise<T> {
  return page.evaluate(
    ({ cmd, args }) => {
      const w = window as unknown as {
        __TAURI_INTERNALS__: {
          invoke: (c: string, a: Record<string, unknown>) => Promise<unknown>;
        };
      };
      return w.__TAURI_INTERNALS__.invoke(cmd, args) as Promise<unknown>;
    },
    { cmd, args },
  ) as Promise<T>;
}

// Reject path: capture the structured `{ code, message }` the op throws.
async function invokeError(
  page: Page,
  cmd: string,
  args: Record<string, unknown>,
): Promise<{ code: string; message: string }> {
  return page.evaluate(
    async ({ cmd, args }) => {
      const w = window as unknown as {
        __TAURI_INTERNALS__: {
          invoke: (c: string, a: Record<string, unknown>) => Promise<unknown>;
        };
      };
      try {
        await w.__TAURI_INTERNALS__.invoke(cmd, args);
        return { code: "none", message: "expected the op to reject" };
      } catch (e) {
        return e as { code: string; message: string };
      }
    },
    { cmd, args },
  );
}
