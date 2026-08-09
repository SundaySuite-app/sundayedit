//! Stress guard for the NLE timeline hot path.
//!
//! `validate_timeline()` runs inside `finalize()` after EVERY timeline op —
//! on every drag tick, trim and drop the frontend commits. This suite pins
//! its cost on a deliberately hostile project (5.000 clips across 8 lanes,
//! mirroring `operations_stress.rs`'s 5.000-caption guard) so an accidental
//! O(n²) regression — a nested scan per item, a repeated sort in a loop —
//! shows up as a hard test failure instead of a sluggish editor.
//!
//! The 200 ms budget is generous on purpose: these run as DEBUG builds in
//! CI, ~10-30× slower than the release binary the user gets. Today's debug
//! numbers are single-digit milliseconds; 200 ms therefore means "an order
//! of magnitude blowup happened", not "the machine was busy".

use std::time::Instant;

use sundayedit_lib::model::{
    ExportConfig, MediaItem, Project, ProjectMeta, Style, TimelineItem, TimelineItemKind, Track,
    TrackKind, Transform,
};
use sundayedit_lib::services::timeline_ops::{move_timeline_item, split_timeline_item};
use sundayedit_lib::services::video::MediaKind;

const TRACKS: usize = 8;
const ITEMS: usize = 5_000;
const CLIP_MS: i64 = 1_000;

fn media(id: &str, dur: i64) -> MediaItem {
    MediaItem {
        id: id.into(),
        path: format!("/v/{id}.mp4"),
        content_hash: "h".into(),
        kind: MediaKind::Video,
        duration_ms: dur,
        width: 1920,
        height: 1080,
        fps: 30.0,
        has_audio: true,
        audio_wav_path: None,
        original_filename: format!("{id}.mp4"),
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
    }
}

fn item(id: &str, track_id: &str, media_id: &str, start: i64) -> TimelineItem {
    TimelineItem {
        id: id.into(),
        track_id: track_id.into(),
        kind: TimelineItemKind::Av,
        source_media_id: Some(media_id.to_string()),
        in_ms: 0,
        out_ms: CLIP_MS,
        timeline_start_ms: start,
        speed: 1.0,
        transform: Transform::default(),
        effects: vec![],
        transition_in: None,
        text: None,
        enabled: true,
        locked: false,
    }
}

/// 5.000 butt-joined clips spread round-robin over 4 video + 4 audio lanes.
/// Butt joins (zero gap) are the tightest legal layout, so the non-overlap
/// sweep has no slack to hide behind.
fn huge_project() -> Project {
    let mut tracks = Vec::with_capacity(TRACKS);
    for t in 0..TRACKS {
        let kind = if t % 2 == 0 {
            TrackKind::Video
        } else {
            TrackKind::Audio
        };
        tracks.push(track(&format!("t{t}"), kind, t as i32));
    }

    let mut items = Vec::with_capacity(ITEMS);
    let mut cursor = [0i64; TRACKS];
    for i in 0..ITEMS {
        let t = i % TRACKS;
        items.push(item(&format!("i{i}"), &format!("t{t}"), "m1", cursor[t]));
        cursor[t] += CLIP_MS;
    }

    Project {
        id: "p".into(),
        name: "stress".into(),
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
        media: vec![media("m1", CLIP_MS)],
        tracks,
        timeline_items: items,
        created_at: 0,
        updated_at: 0,
    }
}

#[test]
fn validate_and_move_stay_fast_on_5000_items() {
    let p = huge_project();
    assert_eq!(p.timeline_items.len(), ITEMS);

    // validate_timeline alone — the per-commit invariant sweep.
    let t0 = Instant::now();
    p.validate_timeline().expect("stress project must be valid");
    let validate_elapsed = t0.elapsed();

    // One full op: whole-project clone + lane overlap check + finalize
    // (which re-runs validate_timeline + validate). This is exactly what a
    // single drag-drop commit costs. Move the last clip of lane t0 to the
    // end of lane t1 — a legal, gap-free spot.
    let last_on_t0 = format!("i{}", ITEMS - TRACKS); // last round-robin hit on t0
    let t1_end = (ITEMS / TRACKS) as i64 * CLIP_MS;
    let t0 = Instant::now();
    let moved = move_timeline_item(&p, &last_on_t0, "t1", t1_end).expect("move must succeed");
    let move_elapsed = t0.elapsed();
    assert_eq!(
        moved
            .timeline_items
            .iter()
            .find(|it| it.id == last_on_t0)
            .unwrap()
            .track_id,
        "t1"
    );

    let total = validate_elapsed + move_elapsed;
    println!(
        "validate_timeline: {validate_elapsed:?}, move_timeline_item: {move_elapsed:?}, total: {total:?}"
    );
    assert!(
        total.as_millis() < 200,
        "timeline hot path blew its budget: validate {validate_elapsed:?} + move {move_elapsed:?} \
         >= 200ms on {ITEMS} items / {TRACKS} tracks — an O(n²) regression crept in"
    );
}

#[test]
fn split_on_5000_items_stays_fast_and_valid() {
    let p = huge_project();
    // Split a mid-timeline clip on lane t0.
    let mid = format!("i{}", (ITEMS / 2 / TRACKS) * TRACKS); // some clip on t0
    let start = p
        .timeline_items
        .iter()
        .find(|it| it.id == mid)
        .unwrap()
        .timeline_start_ms;
    let t0 = Instant::now();
    let r = split_timeline_item(&p, &mid, start + CLIP_MS / 2, "fresh".into())
        .expect("interior split must succeed");
    let elapsed = t0.elapsed();
    assert_eq!(r.timeline_items.len(), ITEMS + 1);
    println!("split_timeline_item on {ITEMS} items: {elapsed:?}");
    assert!(
        elapsed.as_millis() < 200,
        "split took {elapsed:?} on {ITEMS} items — budget is 200ms"
    );
}
