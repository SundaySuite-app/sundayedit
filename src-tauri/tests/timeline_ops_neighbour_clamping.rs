//! Neighbour-aware clamping in the timeline ops.
//!
//! `services/timeline_ops` documents that inputs are CLAMPED rather than
//! hard-rejected wherever an out-of-range value has a sensible in-range
//! meaning. These tests pin that contract at the places where it interacts
//! with same-track neighbours — the exact wire values the frontend
//! (`clipDrag.ts` / `Timeline.tsx onLaneDrop`) sends, whose errors the UI
//! swallows silently:
//!
//!   - a right-edge trim into the next clip clamps `out_ms`, never slides or
//!     rejects (ops-trim-end-into-neighbour);
//!   - a coupled left-edge trim past the previous clip clamps ONE delta for
//!     both `in_ms` and `timeline_start_ms`, so a stopped edge cannot slip
//!     source content or grow the clip (ops-trim-start-content-slip);
//!   - a media drop whose full length would touch an existing clip is clamped
//!     into the gap under the pointer instead of rejected
//!     (ops-add-item-overlap-silent-reject);
//!   - splitting a speed<1 clip at an interior point succeeds — the source
//!     mapping floors so the truncating `timeline_end_ms` of the left piece
//!     never crosses the cut (ops-split-slowmo-rounding-invariant).

use sundayedit_lib::model::{
    ExportConfig, MediaItem, Project, ProjectMeta, Style, TimelineItem, TimelineItemKind, Track,
    TrackKind, Transform,
};
use sundayedit_lib::services::timeline_ops::{
    add_timeline_item, move_timeline_item, split_timeline_item, trim_timeline_item,
};
use sundayedit_lib::services::video::MediaKind;

fn media(id: &str, dur: i64) -> MediaItem {
    MediaItem {
        id: id.into(),
        path: format!("/v/{}.mp4", id),
        content_hash: "h".into(),
        kind: MediaKind::Video,
        duration_ms: dur,
        width: 1920,
        height: 1080,
        fps: 30.0,
        has_audio: true,
        audio_wav_path: None,
        original_filename: format!("{}.mp4", id),
        added_at: 0,
    }
}

fn track(id: &str, kind: TrackKind, index: i32) -> Track {
    Track {
        id: id.into(),
        kind,
        name: id.into(),
        index,
        enabled: true,
        locked: false,
        muted: false,
        solo: false,
        volume_db: 0.0,
    }
}

fn item(
    id: &str,
    track_id: &str,
    media_id: Option<&str>,
    start: i64,
    in_ms: i64,
    out_ms: i64,
) -> TimelineItem {
    TimelineItem {
        id: id.into(),
        track_id: track_id.into(),
        kind: TimelineItemKind::Av,
        source_media_id: media_id.map(|s| s.to_string()),
        in_ms,
        out_ms,
        timeline_start_ms: start,
        speed: 1.0,
        gain_db: 0.0,
        fade_in_ms: 0,
        fade_out_ms: 0,
        transform: Transform::default(),
        effects: vec![],
        transition_in: None,
        text: None,
        enabled: true,
        locked: false,
    }
}

fn base_with(media_items: Vec<MediaItem>) -> Project {
    Project {
        id: "p".into(),
        name: "n".into(),
        video_path: "/v.mp4".into(),
        video_content_hash: "h".into(),
        video_duration_ms: 10_000,
        video_width: 1920,
        video_height: 1080,
        video_fps: 30.0,
        audio_wav_path: None,
        language: "no".into(),
        default_style: Style::broadcast_news(),
        context_description: None,
        captions: vec![],
        speakers: vec![],
        glossary: vec![],
        clips: vec![],
        talk_summary: None,
        export_config: ExportConfig::default(),
        project_meta: ProjectMeta::default(),
        media: media_items,
        tracks: vec![track("v1", TrackKind::Video, 0)],
        timeline_items: vec![],
        created_at: 0,
        updated_at: 0,
    }
}

fn base() -> Project {
    base_with(vec![media("m1", 5000)])
}

// ── Right-edge trim vs the next neighbour ───────────────────────────────────

/// Post-split state: i1 [0..1000], i2 butt-joined at 1000. Dragging i1's right
/// edge to 1500 must clamp `out_ms` so i1 still ends at i2's start — not
/// return `AppError::Invariant`.
#[test]
fn trim_out_edge_into_next_neighbour_clamps_out_ms() {
    let mut p = base();
    p.timeline_items = vec![
        item("i1", "v1", Some("m1"), 0, 0, 1000),
        item("i2", "v1", Some("m1"), 1000, 1000, 2000),
    ];

    let r = trim_timeline_item(&p, "i1", None, Some(1500), None);
    let next =
        r.unwrap_or_else(|e| panic!("right-edge trim into neighbour must clamp, not error: {e}"));

    let i1 = &next.timeline_items[0];
    assert_eq!(i1.timeline_start_ms, 0, "left edge must not move");
    assert_eq!(
        i1.timeline_end_ms(),
        1000,
        "clip must end exactly at next neighbour's start"
    );
    next.validate_timeline().unwrap();
}

/// Gap variant: i1 [1000..1500], i2 at 2000. Dragging i1's right edge past
/// the gap must clamp `out_ms` so i1 ends at 2000 — not teleport the clip's
/// LEFT edge earlier.
#[test]
fn trim_out_edge_with_gap_does_not_slide_left_edge() {
    let mut p = base();
    p.timeline_items = vec![
        item("i1", "v1", Some("m1"), 1000, 0, 500),
        item("i2", "v1", Some("m1"), 2000, 1000, 2000),
    ];

    let next = trim_timeline_item(&p, "i1", None, Some(1300), None).unwrap();

    let i1 = &next.timeline_items[0];
    assert_eq!(
        i1.timeline_start_ms, 1000,
        "left edge must not move on a right-edge drag"
    );
    assert!(
        i1.timeline_end_ms() <= 2000,
        "clip must not overlap next neighbour"
    );
}

// ── Coupled left-edge trim vs the previous neighbour ────────────────────────

/// Track v1: A ends at 1000; B at [timeline 1000.., in 500, out 1500]. The
/// user drags B's left edge 200 ms left; `clipDragToOp` commits the coupled
/// pair `newInMs = 300`, `newTimelineStartMs = 800`. The backend clamps start
/// back to prev_end = 1000 (effective delta 0) — so the coupled `in_ms` must
/// stay 500 too, or the visible source content slips and the clip grows.
#[test]
fn left_trim_overshoot_into_prev_neighbour_must_not_slip_source_content() {
    let mut p = base();
    p.timeline_items = vec![
        item("a", "v1", Some("m1"), 0, 0, 1000),
        item("b", "v1", Some("m1"), 1000, 500, 1500),
    ];

    let r = trim_timeline_item(&p, "b", Some(300), None, Some(800)).unwrap();

    let b = r
        .timeline_items
        .iter()
        .find(|it| it.id == "b")
        .expect("b still present");

    assert_eq!(b.timeline_start_ms, 1000, "start clamps to prev_end");
    assert_eq!(
        b.in_ms,
        500,
        "start clamped to prev_end (edge moved 0 ms) but in_ms kept the full \
         requested reduction — source content slipped {} ms earlier and the \
         clip end grew to {}",
        500 - b.in_ms,
        b.timeline_end_ms(),
    );
    assert_eq!(
        b.timeline_end_ms(),
        2000,
        "clip duration must not grow from a fully-clamped left-edge trim"
    );
}

/// Same overshoot with a third clip C butted right after B: the fully-clamped
/// drag is a legal no-op and must not fail validation (an uncoupled in_ms
/// would grow B onto C and revert the drag silently).
#[test]
fn left_trim_overshoot_with_next_neighbour_must_not_fail_validation() {
    let mut p = base();
    p.timeline_items = vec![
        item("a", "v1", Some("m1"), 0, 0, 1000),
        item("b", "v1", Some("m1"), 1000, 500, 1500),
        item("c", "v1", Some("m1"), 2000, 1500, 2500),
    ];

    let r = trim_timeline_item(&p, "b", Some(300), None, Some(800));
    let r = match r {
        Ok(r) => r,
        Err(e) => panic!(
            "fully-clamped left-edge trim (effective delta 0) must succeed, \
             got error: {e:?}"
        ),
    };

    let b = r.timeline_items.iter().find(|it| it.id == "b").unwrap();
    assert_eq!(
        (b.timeline_start_ms, b.in_ms, b.timeline_end_ms()),
        (1000, 500, 2000)
    );
}

// ── Bin drop (add_timeline_item) vs an occupied lane ────────────────────────

/// The exact `onLaneDrop` scenario: an existing 60 s clip at [60_000..120_000]
/// on the video lane, and a full-length 5-minute drop at timeline 0 (an empty
/// 60 s gap under the pointer). `add_timeline_item` must place the clip —
/// clamped into the gap or shifted like `move_timeline_item` — instead of
/// rejecting the whole op with Invariant (which the UI swallows).
#[test]
fn add_full_length_media_into_gap_before_existing_clip_places_instead_of_rejecting() {
    let mut p = base_with(vec![media("m_short", 60_000), media("m_long", 300_000)]);
    p.timeline_items = vec![item("i1", "v1", Some("m_short"), 60_000, 0, 60_000)];

    // What Timeline.tsx onLaneDrop sends: full media length (in 0,
    // out media.duration_ms) at the pointer position (timeline 0).
    let result = add_timeline_item(
        &p,
        "i2".into(),
        "v1",
        Some("m_long".into()),
        0,
        300_000,
        0,
        TimelineItemKind::Av,
    );

    let next = result.unwrap_or_else(|e| {
        panic!(
            "dropping a 300 s media item at timeline 0 (a 60 s empty gap) was \
             hard-rejected instead of clamped/shifted — Timeline.tsx onLaneDrop \
             swallows this error, so the drop silently does nothing: {e:?}"
        )
    });

    let added = next
        .timeline_items
        .iter()
        .find(|it| it.id == "i2")
        .expect("dropped clip present on the timeline");
    assert_eq!(added.track_id, "v1");
    next.validate_timeline().expect("resulting timeline valid");
}

/// Contrast: `move_timeline_item` faced with the identical overlap shifts the
/// clip to the end of the track — add and move share the lane policy.
#[test]
fn move_handles_the_same_overlap_by_shifting() {
    let mut p = base_with(vec![media("m_short", 60_000), media("m_long", 300_000)]);
    p.timeline_items = vec![
        item("i1", "v1", Some("m_short"), 60_000, 0, 60_000),
        item("i2", "v1", Some("m_long"), 120_000, 0, 300_000),
    ];

    // Ask to move the long clip to timeline 0 — same overlap as the drop.
    let next = move_timeline_item(&p, "i2", "v1", 0)
        .expect("move_timeline_item shifts instead of rejecting");
    let moved = next.timeline_items.iter().find(|it| it.id == "i2").unwrap();
    assert_eq!(moved.timeline_start_ms, 120_000);
}

// ── Split of a slow-motion clip ─────────────────────────────────────────────

/// Slow-motion clip (speed 0.25): in 0, out 500 → timeline span [0..2000].
/// Splitting strictly inside at 1002 must succeed and produce two pieces that
/// tile the span — a round()ed source mapping put the left piece's truncated
/// end at 1004 > 1002 and failed validation.
#[test]
fn split_slowmo_clip_interior_point_succeeds() {
    let mut p = base();
    let mut it = item("i1", "v1", Some("m1"), 0, 0, 500);
    it.speed = 0.25;
    assert_eq!(
        it.timeline_end_ms(),
        2000,
        "precondition: span is [0..2000]"
    );
    p.timeline_items = vec![it];

    let r = split_timeline_item(&p, "i1", 1002, "i1-right".into());
    let next =
        r.unwrap_or_else(|e| panic!("valid interior split of a speed<1 clip must not error: {e}"));

    assert_eq!(next.timeline_items.len(), 2);
    let left = &next.timeline_items[0];
    let right = &next.timeline_items[1];
    assert_eq!(
        right.timeline_start_ms, 1002,
        "right piece starts at the cut"
    );
    assert!(
        left.timeline_end_ms() <= right.timeline_start_ms,
        "left piece must not overlap the right piece: left ends {} but right starts {}",
        left.timeline_end_ms(),
        right.timeline_start_ms
    );
    next.validate_timeline().unwrap();
}

/// The floored source mapping hands the boundary source ms to the right
/// piece — its derived end must still not grow past the original clip end,
/// or the split would overlap a butt-joined neighbour.
#[test]
fn split_slowmo_clip_next_to_butted_neighbour_succeeds() {
    let mut p = base();
    let mut it = item("i1", "v1", Some("m1"), 0, 0, 500);
    it.speed = 0.25; // ends at 2000
    p.timeline_items = vec![it, item("i2", "v1", Some("m1"), 2000, 1000, 2000)];

    let next = split_timeline_item(&p, "i1", 1002, "i1-right".into())
        .unwrap_or_else(|e| panic!("split beside a butted neighbour must not error: {e}"));
    let right = next
        .timeline_items
        .iter()
        .find(|it| it.id == "i1-right")
        .unwrap();
    assert!(
        right.timeline_end_ms() <= 2000,
        "right piece must not grow past the original clip end (got {})",
        right.timeline_end_ms()
    );
    next.validate_timeline().unwrap();
}
