//! Cross-language parity for `TimelineItem::timeline_end_ms`.
//!
//! The Rust method and its TypeScript mirror `previewMap.timelineEndMs`
//! (src/features/timeline/previewMap.ts) must agree bit-for-bit: every UI
//! adjacency/snapping/duration computation (laneLayout.itemSpan, clipDrag
//! clamps, timelineDurationMs) uses the TS value, while validate_timeline,
//! neighbour_bounds, split bounds and the xfade offset use the Rust value.
//! Both sides therefore compute in f64 with truncation toward zero
//! (`Math.trunc`). An earlier f32 implementation diverged by 1 ms around
//! integer quotients (1100 ms at speed 1.1) and above 2^24 ms durations,
//! making Rust reject butt-joined layouts the UI showed as legal
//! (seam-timeline-end-f32-vs-f64).
//!
//! The TS side is replicated exactly: serde_json round-trip of the item (the
//! IPC / project-file wire format), `speed` read back as f64 exactly as
//! `JSON.parse` yields in JS, then f64 divide + trunc.

use sundayedit_lib::model::{TimelineItem, TimelineItemKind, Transform};

fn item(start: i64, in_ms: i64, out_ms: i64, speed: f32) -> TimelineItem {
    TimelineItem {
        id: "i".into(),
        track_id: "t1".into(),
        kind: TimelineItemKind::Av,
        source_media_id: Some("m1".into()),
        in_ms,
        out_ms,
        timeline_start_ms: start,
        speed,
        transform: Transform::default(),
        effects: vec![],
        transition_in: None,
        text: None,
        enabled: true,
        locked: false,
    }
}

/// Exactly what `previewMap.timelineEndMs` computes on the frontend, fed the
/// same JSON the frontend is fed.
fn ts_mirror_timeline_end_ms(it: &TimelineItem) -> i64 {
    let json = serde_json::to_value(it).expect("serialize TimelineItem");
    let speed_f64 = json["speed"].as_f64().expect("speed is a JSON number");
    let speed = speed_f64.max(0.01);
    it.timeline_start_ms + (((it.out_ms - it.in_ms) as f64) / speed).trunc() as i64
}

/// out−in = 1100 ms at speed 1.1: the f32 quotient rounds up to exactly
/// 1000.0 while f64 gives 999.9999999999999 — both lanes must say 999.
#[test]
fn rust_and_ts_agree_on_end_at_speed_1_1() {
    let it = item(0, 0, 1100, 1.1);
    let rust_end = it.timeline_end_ms();
    let ts_end = ts_mirror_timeline_end_ms(&it);
    assert_eq!(
        rust_end, ts_end,
        "seam: Rust timeline_end_ms = {rust_end}, TS previewMap.timelineEndMs mirror = {ts_end}"
    );
}

/// The UI butts a second clip up against the TS-computed end of the first.
/// Rust's validate_timeline must accept the same layout — i.e. Rust's end
/// must not exceed the TS end the UI placed the neighbour at.
#[test]
fn ui_butt_joined_layout_is_not_an_overlap_in_rust() {
    let first = item(0, 0, 1100, 1.1);
    let ui_end = ts_mirror_timeline_end_ms(&first); // where the UI lets clip 2 start
    let rust_end = first.timeline_end_ms();
    assert!(
        rust_end <= ui_end,
        "UI butt-joins next clip at {ui_end} ms (TS f64 end), but Rust validate_timeline \
         sees the first clip ending at {rust_end} ms -> flags a phantom overlap"
    );
}

/// At speed 1.0 a bare `as f32` cast loses integer precision above 2^24 ms
/// (~4h39m). f64 holds integers exactly up to 2^53 — both lanes must agree
/// on very long media.
#[test]
fn rust_and_ts_agree_on_end_for_long_media_at_speed_1() {
    let dur = (1i64 << 24) + 1; // 16_777_217 ms
    let it = item(0, 0, dur, 1.0);
    let rust_end = it.timeline_end_ms();
    let ts_end = ts_mirror_timeline_end_ms(&it);
    assert_eq!(
        rust_end, ts_end,
        "seam: >2^24 ms duration at speed 1.0 — Rust = {rust_end}, TS (f64) = {ts_end}"
    );
}
