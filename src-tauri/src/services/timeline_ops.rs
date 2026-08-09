//! Pure NLE timeline operations.
//!
//! Every public function takes a `&Project` and returns either a new
//! `Project` on success or an `AppError`. The input is never mutated —
//! same discipline as `services::operations` (caption ops), so undo is
//! trivial (keep the previous `Project`) and tests are easy.
//!
//! Inputs are CLAMPED rather than hard-rejected wherever an out-of-range
//! value has a sensible in-range meaning (mirror `move_caption` /
//! `resize_caption`): a drag past a neighbour stops at the gap, an in/out
//! past the media bounds snaps to the media, a negative start snaps to 0.
//!
//! Every op finishes by running `Project::validate_timeline()` and then
//! `Project::validate()` so a malformed result is surfaced as an
//! `Invariant` error instead of corrupting state.

use crate::error::{AppError, AppResult};
use crate::model::{
    MediaItem, Project, TextSpec, TimelineItem, TimelineItemKind, Track, TrackKind, Transform,
    Transition,
};

// ── finalize ──────────────────────────────────────────────────────────────────

/// Run both invariant checks and return the project, mapping either failure
/// to `AppError::Invariant`.
fn finalize(next: Project) -> AppResult<Project> {
    next.validate_timeline().map_err(AppError::Invariant)?;
    next.validate().map_err(AppError::Invariant)?;
    Ok(next)
}

// ── media pool ─────────────────────────────────────────────────────────────────

/// Append an imported media item to the pool. The IO (probe + hash) happens
/// in the command wrapper — this is the pure state transition.
pub fn add_media(project: &Project, media: MediaItem) -> AppResult<Project> {
    let mut next = project.clone();
    next.media.push(media);
    finalize(next)
}

/// Remove a media item from the pool. Rejected if any timeline item still
/// references it (you'd orphan the clip).
pub fn remove_media(project: &Project, media_id: &str) -> AppResult<Project> {
    let idx = project
        .media
        .iter()
        .position(|m| m.id == media_id)
        .ok_or_else(|| AppError::NotFound {
            entity: "media",
            id: media_id.to_string(),
        })?;

    if project
        .timeline_items
        .iter()
        .any(|it| it.source_media_id.as_deref() == Some(media_id))
    {
        return Err(AppError::Validation(format!(
            "media {} is still used by one or more timeline items — remove them first",
            media_id
        )));
    }

    let mut next = project.clone();
    next.media.remove(idx);
    finalize(next)
}

// ── tracks ─────────────────────────────────────────────────────────────────────

/// Add a track, assigning it the next stacking index (max + 1).
pub fn add_track(
    project: &Project,
    id: String,
    kind: TrackKind,
    name: String,
) -> AppResult<Project> {
    let index = project
        .tracks
        .iter()
        .map(|t| t.index)
        .max()
        .map(|m| m + 1)
        .unwrap_or(0);
    let mut next = project.clone();
    next.tracks.push(Track {
        id,
        kind,
        name,
        index,
        enabled: true,
        locked: false,
        muted: false,
        solo: false,
    });
    finalize(next)
}

/// Remove a track. Rejected if it still has timeline items or captions on it.
pub fn remove_track(project: &Project, track_id: &str) -> AppResult<Project> {
    let idx = project
        .tracks
        .iter()
        .position(|t| t.id == track_id)
        .ok_or_else(|| AppError::NotFound {
            entity: "track",
            id: track_id.to_string(),
        })?;

    let has_items = project
        .timeline_items
        .iter()
        .any(|it| it.track_id == track_id)
        || project
            .captions
            .iter()
            .any(|c| c.track_id.as_deref() == Some(track_id));
    if has_items {
        return Err(AppError::Validation(format!(
            "track {} still has items — remove them before deleting the track",
            track_id
        )));
    }

    let mut next = project.clone();
    next.tracks.remove(idx);
    finalize(next)
}

/// Move a track to a new stacking position and renumber every track's index
/// to a dense `0..n`. `new_index` is clamped into range.
pub fn reorder_track(project: &Project, track_id: &str, new_index: i32) -> AppResult<Project> {
    if !project.tracks.iter().any(|t| t.id == track_id) {
        return Err(AppError::NotFound {
            entity: "track",
            id: track_id.to_string(),
        });
    }

    let mut next = project.clone();
    // Work on an index-ordered copy so the visual order is what we reshuffle.
    let mut ordered = next.tracks.clone();
    ordered.sort_by_key(|t| t.index);
    let cur_pos = ordered.iter().position(|t| t.id == track_id).unwrap();
    let moved = ordered.remove(cur_pos);
    let target = new_index.max(0) as usize;
    let target = target.min(ordered.len());
    ordered.insert(target, moved);
    for (i, t) in ordered.iter_mut().enumerate() {
        t.index = i as i32;
    }
    next.tracks = ordered;
    finalize(next)
}

/// Toggle any subset of a track's boolean flags. `None` leaves a flag as-is.
pub fn set_track_flags(
    project: &Project,
    track_id: &str,
    enabled: Option<bool>,
    locked: Option<bool>,
    muted: Option<bool>,
    solo: Option<bool>,
) -> AppResult<Project> {
    let mut next = project.clone();
    let track = next
        .tracks
        .iter_mut()
        .find(|t| t.id == track_id)
        .ok_or_else(|| AppError::NotFound {
            entity: "track",
            id: track_id.to_string(),
        })?;
    if let Some(v) = enabled {
        track.enabled = v;
    }
    if let Some(v) = locked {
        track.locked = v;
    }
    if let Some(v) = muted {
        track.muted = v;
    }
    if let Some(v) = solo {
        track.solo = v;
    }
    finalize(next)
}

// ── timeline items ─────────────────────────────────────────────────────────────

/// Place a new clip on a track. `in_ms`/`out_ms` are clamped to the source
/// media's duration (when the item references media); `timeline_start_ms`
/// clamps to `>= 0`. Builds an identity transform, no effects, speed 1.0.
#[allow(clippy::too_many_arguments)]
pub fn add_timeline_item(
    project: &Project,
    id: String,
    track_id: &str,
    source_media_id: Option<String>,
    in_ms: i64,
    out_ms: i64,
    timeline_start_ms: i64,
    kind: TimelineItemKind,
) -> AppResult<Project> {
    let track = project
        .tracks
        .iter()
        .find(|t| t.id == track_id)
        .ok_or_else(|| AppError::NotFound {
            entity: "track",
            id: track_id.to_string(),
        })?;

    // Clamp in/out. With media the bounds are the media duration; without it
    // (text/graphic) we only need `in < out` and `in >= 0`.
    let (in_ms, mut out_ms) = if let Some(mid) = &source_media_id {
        let media = find_media(project, mid)?;
        let dur = media.duration_ms;
        let i = in_ms.clamp(0, dur);
        let o = out_ms.clamp(0, dur);
        (i, o)
    } else {
        (in_ms.max(0), out_ms.max(0))
    };
    if in_ms >= out_ms {
        return Err(AppError::Validation(
            "timeline item has no positive duration after clamping".to_string(),
        ));
    }

    let mut start = timeline_start_ms.max(0);

    // Lane placement (Video/Audio disallow overlap): a drop whose full length
    // would touch an existing clip is CLAMPED into the gap under the pointer,
    // falling back to the end of the track when there is no gap — mirroring
    // `move_timeline_item`'s shift policy instead of letting `finalize` reject
    // the op (which the UI's drop handler swallows as a silent no-op).
    // New items always have speed 1.0, so source ms == timeline ms.
    if matches!(track.kind, TrackKind::Video | TrackKind::Audio) {
        let others: Vec<&TimelineItem> = project
            .timeline_items
            .iter()
            .filter(|it| it.track_id == track_id)
            .collect();
        let track_end = others
            .iter()
            .map(|o| o.timeline_end_ms())
            .max()
            .unwrap_or(0)
            .max(0);
        let prev_end = others
            .iter()
            .filter(|o| o.timeline_start_ms <= start)
            .map(|o| o.timeline_end_ms())
            .max()
            .unwrap_or(0);
        if start < prev_end {
            // The pointer sits inside an existing clip — no gap here.
            start = track_end;
        }
        let next_start = others
            .iter()
            .filter(|o| o.timeline_start_ms > start)
            .map(|o| o.timeline_start_ms)
            .min();
        if let Some(ns) = next_start {
            let gap = ns - start;
            if gap >= 1 {
                // Trim the clip's tail so it fits the gap under the pointer.
                out_ms = out_ms.min(in_ms + gap);
            } else {
                start = track_end;
            }
        }
    }

    let item = TimelineItem {
        id,
        track_id: track_id.to_string(),
        kind,
        source_media_id,
        in_ms,
        out_ms,
        timeline_start_ms: start,
        speed: 1.0,
        transform: Transform::default(),
        effects: vec![],
        transition_in: None,
        text: None,
        enabled: true,
        locked: false,
    };

    let mut next = project.clone();
    next.timeline_items.push(item);
    finalize(next)
}

/// Split a clip in two at `at_timeline_ms` on the timeline. The split point is
/// mapped back into the source (respecting `speed`); the left piece keeps the
/// original id + leading transition, the right piece gets `new_id`. The split
/// must fall strictly inside the clip.
pub fn split_timeline_item(
    project: &Project,
    item_id: &str,
    at_timeline_ms: i64,
    new_id: String,
) -> AppResult<Project> {
    let (idx, original) = find_item(project, item_id)?;
    let start = original.timeline_start_ms;
    let end = original.timeline_end_ms();
    if at_timeline_ms <= start || at_timeline_ms >= end {
        return Err(AppError::Validation(format!(
            "split point {} is outside the clip's timeline range ({}, {})",
            at_timeline_ms, start, end
        )));
    }

    // Map the timeline split point back into the source media. floor() (not
    // round) so the LEFT piece's truncating `timeline_end_ms` can never land
    // past the cut — with speed < 1 a half-up source ms amplifies to 1/speed
    // timeline ms, and a left piece ending after `at_timeline_ms` overlaps the
    // right piece and fails validation on a perfectly valid interior split.
    let speed = original.speed.max(0.01) as f64;
    let src_split = original.in_ms + (((at_timeline_ms - start) as f64) * speed).floor() as i64;
    let src_split = src_split.clamp(original.in_ms + 1, original.out_ms - 1);

    let mut left = original.clone();
    left.out_ms = src_split;
    // Defense-in-depth against f64 rounding and the clamp above: the left
    // piece must end at or before the cut.
    while left.out_ms > left.in_ms + 1 && left.timeline_end_ms() > at_timeline_ms {
        left.out_ms -= 1;
    }

    let mut right = original.clone();
    right.id = new_id;
    right.in_ms = src_split;
    right.timeline_start_ms = at_timeline_ms;
    right.transition_in = None; // the cut is not a transition
                                // Flooring `src_split` hands the boundary source ms to the RIGHT piece,
                                // which can push its derived end past the original clip end (and into a
                                // butt-joined neighbour). Trim its tail so the split never grows the span.
    let original_end = original.timeline_end_ms();
    while right.out_ms > right.in_ms + 1 && right.timeline_end_ms() > original_end {
        right.out_ms -= 1;
    }

    let mut next = project.clone();
    next.timeline_items.remove(idx);
    next.timeline_items.insert(idx, left);
    next.timeline_items.insert(idx + 1, right);
    finalize(next)
}

/// Edge-drag trim: adjust any of `in_ms` / `out_ms` / `timeline_start_ms`.
/// Each is clamped to the source media bounds and to same-track neighbours so
/// the clip can neither reveal content it doesn't have nor overlap a sibling.
///
/// The MOVING edge is the one that gives way:
///   - right-edge drag (`new_out_ms` alone) clamps `out_ms` so the clip ends
///     at the next neighbour's start — `timeline_start_ms` never moves;
///   - left-edge drag (`new_in_ms` + `new_timeline_start_ms`, the coupled
///     pair `clipDrag.ts` sends) clamps ONE effective delta against every
///     bound before applying it to BOTH fields, so a clamped edge can neither
///     slip source content nor grow the clip's duration.
pub fn trim_timeline_item(
    project: &Project,
    item_id: &str,
    new_in_ms: Option<i64>,
    new_out_ms: Option<i64>,
    new_timeline_start_ms: Option<i64>,
) -> AppResult<Project> {
    let (idx, original) = find_item(project, item_id)?;
    let media_dur = match &original.source_media_id {
        Some(mid) => find_media(project, mid)?.duration_ms,
        None => i64::MAX,
    };
    let speed = original.speed.max(0.01) as f64;

    // Neighbour bounds on the same track (Video/Audio only care about overlap).
    let (prev_end, next_start) = neighbour_bounds(project, original);

    // ── Coupled left-edge trim: in + start move by ONE delta ────────────────
    // The frontend pre-couples only the zero bounds; the neighbour bound is
    // known solely here, so the pair must be re-coupled after clamping —
    // otherwise an overshoot into the previous clip stops `start` at
    // `prev_end` while `in_ms` keeps the full reduction (content slip).
    if let (Some(_), Some(req_start), None) = (new_in_ms, new_timeline_start_ms, new_out_ms) {
        let requested = req_start - original.timeline_start_ms;
        let min_from_prev = prev_end - original.timeline_start_ms;
        let min_from_zero_start = -original.timeline_start_ms;
        let min_from_zero_in = (-(original.in_ms as f64) / speed).ceil() as i64;
        let max_from_out = (((original.out_ms - 1 - original.in_ms) as f64) / speed).floor() as i64;
        let delta = requested
            .max(min_from_prev)
            .max(min_from_zero_start)
            .max(min_from_zero_in)
            .min(max_from_out);

        let mut next = project.clone();
        let it = &mut next.timeline_items[idx];
        it.timeline_start_ms = original.timeline_start_ms + delta;
        it.in_ms = (original.in_ms + ((delta as f64) * speed).round() as i64)
            .clamp(0, original.out_ms - 1);
        return finalize(next);
    }

    let mut in_ms = new_in_ms.unwrap_or(original.in_ms).clamp(0, media_dur);
    let mut out_ms = new_out_ms.unwrap_or(original.out_ms).clamp(0, media_dur);
    // Keep in < out; whichever edge moved gives way to the other.
    if in_ms >= out_ms {
        if new_in_ms.is_some() {
            in_ms = (out_ms - 1).max(0);
        } else {
            out_ms = (in_ms + 1).min(media_dur);
        }
    }

    // ── Right-edge trim: only `out_ms` moves, `start` stays put ─────────────
    if new_out_ms.is_some() && new_timeline_start_ms.is_none() {
        let start = original.timeline_start_ms;
        if let Some(ns) = next_start {
            // Largest source span that still ends at (or before) the next
            // neighbour's start. floor() so the truncating `timeline_end_ms`
            // can never land past `ns`; the guard loop absorbs any residual
            // f64 rounding at extreme speeds.
            let max_dur = (ns - start).max(1);
            let max_out = in_ms + ((max_dur as f64) * speed).floor() as i64;
            out_ms = out_ms.min(max_out).max(in_ms + 1).min(media_dur);
            while out_ms > in_ms + 1 && start + (((out_ms - in_ms) as f64) / speed) as i64 > ns {
                out_ms -= 1;
            }
        }
        let mut next = project.clone();
        let it = &mut next.timeline_items[idx];
        it.in_ms = in_ms;
        it.out_ms = out_ms;
        return finalize(next);
    }

    // ── Start move (and legacy single-field combinations) ───────────────────
    let dur = (((out_ms - in_ms) as f64) / speed) as i64;
    let mut start = new_timeline_start_ms.unwrap_or(original.timeline_start_ms);
    let hi = next_start.map(|ns| ns - dur);
    start = start.max(prev_end);
    if let Some(hi) = hi {
        if hi >= prev_end {
            start = start.min(hi);
        } else {
            start = prev_end;
        }
    }
    start = start.max(0);

    let mut next = project.clone();
    let it = &mut next.timeline_items[idx];
    it.in_ms = in_ms;
    it.out_ms = out_ms;
    it.timeline_start_ms = start;
    finalize(next)
}

/// Move a clip along time and/or across tracks. `timeline_start_ms` is clamped
/// to `>= 0`; on Video/Audio target tracks the clip is shifted to the end of
/// the track if the requested spot would overlap an existing clip.
pub fn move_timeline_item(
    project: &Project,
    item_id: &str,
    new_track_id: &str,
    new_timeline_start_ms: i64,
) -> AppResult<Project> {
    let (idx, original) = find_item(project, item_id)?;
    let target = project
        .tracks
        .iter()
        .find(|t| t.id == new_track_id)
        .ok_or_else(|| AppError::NotFound {
            entity: "track",
            id: new_track_id.to_string(),
        })?;
    let is_lane = matches!(target.kind, TrackKind::Video | TrackKind::Audio);

    let dur = original.timeline_end_ms() - original.timeline_start_ms;
    let mut start = new_timeline_start_ms.max(0);

    if is_lane {
        // Other clips on the target track (exclude self).
        let mut others: Vec<&TimelineItem> = project
            .timeline_items
            .iter()
            .filter(|it| it.track_id == new_track_id && it.id != item_id)
            .collect();
        others.sort_by_key(|it| it.timeline_start_ms);
        let overlaps = |s: i64| {
            others
                .iter()
                .any(|o| s < o.timeline_end_ms() && o.timeline_start_ms < s + dur)
        };
        if overlaps(start) {
            // Shift to the end of the track — always a valid, gap-free spot.
            start = others
                .iter()
                .map(|o| o.timeline_end_ms())
                .max()
                .unwrap_or(0)
                .max(0);
        }
    }

    let mut next = project.clone();
    let it = &mut next.timeline_items[idx];
    it.track_id = new_track_id.to_string();
    it.timeline_start_ms = start;
    finalize(next)
}

/// Delete a clip and close the gap: every later clip on the same track slides
/// left by the deleted clip's timeline duration.
pub fn ripple_delete_item(project: &Project, item_id: &str) -> AppResult<Project> {
    let (idx, original) = find_item(project, item_id)?;
    let track_id = original.track_id.clone();
    let gap = original.timeline_end_ms() - original.timeline_start_ms;
    let removed_start = original.timeline_start_ms;
    let removed_end = original.timeline_end_ms();

    let mut next = project.clone();
    next.timeline_items.remove(idx);
    for it in next.timeline_items.iter_mut() {
        if it.track_id == track_id && it.timeline_start_ms >= removed_end {
            it.timeline_start_ms = (it.timeline_start_ms - gap).max(removed_start);
        }
    }
    finalize(next)
}

// ── transitions / transform ─────────────────────────────────────────────────────

/// Set (or replace) the leading-edge transition on a clip. The duration is
/// clamped to `>= 0` and to the clip's timeline length.
pub fn set_transition(
    project: &Project,
    item_id: &str,
    kind: String,
    duration_ms: i64,
) -> AppResult<Project> {
    let mut next = project.clone();
    let it = find_item_mut(&mut next, item_id)?;
    let max = it.timeline_end_ms() - it.timeline_start_ms;
    let duration_ms = duration_ms.clamp(0, max.max(0));
    it.transition_in = Some(Transition { kind, duration_ms });
    finalize(next)
}

/// Remove a clip's leading transition.
pub fn clear_transition(project: &Project, item_id: &str) -> AppResult<Project> {
    let mut next = project.clone();
    let it = find_item_mut(&mut next, item_id)?;
    it.transition_in = None;
    finalize(next)
}

/// Replace a clip's geometric transform. `opacity` clamps to `[0,1]`, `scale`
/// to `>= 0` — everything else is passed through.
pub fn set_transform(
    project: &Project,
    item_id: &str,
    mut transform: Transform,
) -> AppResult<Project> {
    transform.opacity = transform.opacity.clamp(0.0, 1.0);
    transform.scale = transform.scale.max(0.0);
    let mut next = project.clone();
    let it = find_item_mut(&mut next, item_id)?;
    it.transform = transform;
    finalize(next)
}

/// Add a standalone text overlay clip (no source media). `duration_ms` clamps
/// to `>= 1`, `timeline_start_ms` to `>= 0`.
pub fn add_text_item(
    project: &Project,
    id: String,
    track_id: &str,
    timeline_start_ms: i64,
    duration_ms: i64,
    text: String,
) -> AppResult<Project> {
    if !project.tracks.iter().any(|t| t.id == track_id) {
        return Err(AppError::NotFound {
            entity: "track",
            id: track_id.to_string(),
        });
    }
    let duration_ms = duration_ms.max(1);
    let item = TimelineItem {
        id,
        track_id: track_id.to_string(),
        kind: TimelineItemKind::Text,
        source_media_id: None,
        in_ms: 0,
        out_ms: duration_ms,
        timeline_start_ms: timeline_start_ms.max(0),
        speed: 1.0,
        transform: Transform::default(),
        effects: vec![],
        transition_in: None,
        text: Some(TextSpec {
            text,
            style_id: None,
        }),
        enabled: true,
        locked: false,
    };
    let mut next = project.clone();
    next.timeline_items.push(item);
    finalize(next)
}

// ── helpers ─────────────────────────────────────────────────────────────────────

fn find_item<'a>(project: &'a Project, id: &str) -> AppResult<(usize, &'a TimelineItem)> {
    project
        .timeline_items
        .iter()
        .enumerate()
        .find(|(_, it)| it.id == id)
        .ok_or_else(|| AppError::NotFound {
            entity: "timeline_item",
            id: id.to_string(),
        })
}

fn find_item_mut<'a>(project: &'a mut Project, id: &str) -> AppResult<&'a mut TimelineItem> {
    project
        .timeline_items
        .iter_mut()
        .find(|it| it.id == id)
        .ok_or_else(|| AppError::NotFound {
            entity: "timeline_item",
            id: id.to_string(),
        })
}

fn find_media<'a>(project: &'a Project, id: &str) -> AppResult<&'a MediaItem> {
    project
        .media
        .iter()
        .find(|m| m.id == id)
        .ok_or_else(|| AppError::NotFound {
            entity: "media",
            id: id.to_string(),
        })
}

/// The `[prev_end, next_start)` window a clip may occupy on its own track,
/// derived from its current neighbours (sorted by timeline start). `next_start`
/// is `None` when the clip is last. Non-lane tracks (Caption/Overlay) allow
/// overlap, so they report an unbounded window.
fn neighbour_bounds(project: &Project, item: &TimelineItem) -> (i64, Option<i64>) {
    let track = project.tracks.iter().find(|t| t.id == item.track_id);
    let is_lane = track
        .map(|t| matches!(t.kind, TrackKind::Video | TrackKind::Audio))
        .unwrap_or(false);
    if !is_lane {
        return (0, None);
    }
    let mut others: Vec<&TimelineItem> = project
        .timeline_items
        .iter()
        .filter(|it| it.track_id == item.track_id && it.id != item.id)
        .collect();
    others.sort_by_key(|it| it.timeline_start_ms);
    let prev_end = others
        .iter()
        .filter(|o| o.timeline_start_ms <= item.timeline_start_ms)
        .map(|o| o.timeline_end_ms())
        .max()
        .unwrap_or(0);
    let next_start = others
        .iter()
        .filter(|o| o.timeline_start_ms > item.timeline_start_ms)
        .map(|o| o.timeline_start_ms)
        .min();
    (prev_end, next_start)
}

// ────────────────────────────────────────────────────────────────────────────
// Tests
// ────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ExportConfig, ProjectMeta, Style};
    use crate::services::video::MediaKind;

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
            transform: Transform::default(),
            effects: vec![],
            transition_in: None,
            text: None,
            enabled: true,
            locked: false,
        }
    }

    fn base() -> Project {
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
            media: vec![media("m1", 5000)],
            tracks: vec![track("v1", TrackKind::Video, 0)],
            timeline_items: vec![],
            created_at: 0,
            updated_at: 0,
        }
    }

    // ── add_media / remove_media ────────────────────────────────────────────
    #[test]
    fn add_media_appends() {
        let p = base();
        let r = add_media(&p, media("m2", 2000)).unwrap();
        assert_eq!(r.media.len(), 2);
        assert_eq!(r.media[1].id, "m2");
    }

    #[test]
    fn remove_media_rejects_when_referenced() {
        let mut p = base();
        p.timeline_items = vec![item("i1", "v1", Some("m1"), 0, 0, 1000)];
        let err = remove_media(&p, "m1").unwrap_err();
        assert_eq!(err.code(), "validation");
    }

    #[test]
    fn remove_media_ok_when_unused() {
        let p = base();
        let r = remove_media(&p, "m1").unwrap();
        assert!(r.media.is_empty());
    }

    #[test]
    fn remove_media_missing_is_not_found() {
        let p = base();
        let err = remove_media(&p, "nope").unwrap_err();
        assert_eq!(err.code(), "not_found");
    }

    // ── add_track / remove_track / reorder / flags ──────────────────────────
    #[test]
    fn add_track_assigns_next_index() {
        let p = base();
        let r = add_track(&p, "a1".into(), TrackKind::Audio, "Audio".into()).unwrap();
        assert_eq!(r.tracks.len(), 2);
        assert_eq!(r.tracks[1].index, 1); // v1 was index 0
    }

    #[test]
    fn remove_track_rejects_when_it_has_items() {
        let mut p = base();
        p.timeline_items = vec![item("i1", "v1", Some("m1"), 0, 0, 1000)];
        let err = remove_track(&p, "v1").unwrap_err();
        assert_eq!(err.code(), "validation");
    }

    #[test]
    fn remove_track_ok_when_empty() {
        let mut p = base();
        p.tracks.push(track("cap", TrackKind::Caption, 1));
        let r = remove_track(&p, "cap").unwrap();
        assert_eq!(r.tracks.len(), 1);
    }

    #[test]
    fn remove_track_missing_is_not_found() {
        let p = base();
        let err = remove_track(&p, "nope").unwrap_err();
        assert_eq!(err.code(), "not_found");
    }

    #[test]
    fn reorder_track_renumbers_dense() {
        let mut p = base();
        p.tracks.push(track("t2", TrackKind::Audio, 1));
        p.tracks.push(track("t3", TrackKind::Overlay, 2));
        // Move t3 (index 2) to the front.
        let r = reorder_track(&p, "t3", 0).unwrap();
        let mut ordered = r.tracks.clone();
        ordered.sort_by_key(|t| t.index);
        assert_eq!(ordered[0].id, "t3");
        assert_eq!(ordered[0].index, 0);
        assert_eq!(ordered[1].index, 1);
        assert_eq!(ordered[2].index, 2);
    }

    #[test]
    fn reorder_track_clamps_out_of_range_index() {
        let mut p = base();
        p.tracks.push(track("t2", TrackKind::Audio, 1));
        let r = reorder_track(&p, "v1", 99).unwrap();
        let mut ordered = r.tracks.clone();
        ordered.sort_by_key(|t| t.index);
        assert_eq!(ordered.last().unwrap().id, "v1"); // clamped to the end
    }

    #[test]
    fn reorder_track_missing_is_not_found() {
        let p = base();
        let err = reorder_track(&p, "nope", 0).unwrap_err();
        assert_eq!(err.code(), "not_found");
    }

    #[test]
    fn set_track_flags_applies_only_provided() {
        let p = base();
        let r = set_track_flags(&p, "v1", None, Some(true), Some(true), None).unwrap();
        assert!(r.tracks[0].enabled); // unchanged (was true, `None` left it)
        assert!(r.tracks[0].locked);
        assert!(r.tracks[0].muted);
        assert!(!r.tracks[0].solo);
    }

    #[test]
    fn set_track_flags_missing_is_not_found() {
        let p = base();
        let err = set_track_flags(&p, "nope", Some(true), None, None, None).unwrap_err();
        assert_eq!(err.code(), "not_found");
    }

    // ── add_timeline_item ───────────────────────────────────────────────────
    #[test]
    fn add_timeline_item_valid() {
        let p = base();
        let r = add_timeline_item(
            &p,
            "i1".into(),
            "v1",
            Some("m1".into()),
            0,
            2000,
            0,
            TimelineItemKind::Av,
        )
        .unwrap();
        assert_eq!(r.timeline_items.len(), 1);
        let it = &r.timeline_items[0];
        assert_eq!((it.in_ms, it.out_ms), (0, 2000));
        assert_eq!(it.speed, 1.0);
        assert_eq!(it.transform, Transform::default());
    }

    #[test]
    fn add_timeline_item_clamps_out_to_media_duration() {
        let p = base();
        // media m1 is 5000ms; asking out=9000 clamps to 5000.
        let r = add_timeline_item(
            &p,
            "i1".into(),
            "v1",
            Some("m1".into()),
            0,
            9000,
            0,
            TimelineItemKind::Av,
        )
        .unwrap();
        assert_eq!(r.timeline_items[0].out_ms, 5000);
    }

    #[test]
    fn add_timeline_item_clamps_negative_start() {
        let p = base();
        let r = add_timeline_item(
            &p,
            "i1".into(),
            "v1",
            Some("m1".into()),
            0,
            2000,
            -500,
            TimelineItemKind::Av,
        )
        .unwrap();
        assert_eq!(r.timeline_items[0].timeline_start_ms, 0);
    }

    #[test]
    fn add_timeline_item_unknown_track_rejected() {
        let p = base();
        let err = add_timeline_item(
            &p,
            "i1".into(),
            "nope",
            Some("m1".into()),
            0,
            2000,
            0,
            TimelineItemKind::Av,
        )
        .unwrap_err();
        assert_eq!(err.code(), "not_found");
    }

    #[test]
    fn add_timeline_item_unknown_media_rejected() {
        let p = base();
        let err = add_timeline_item(
            &p,
            "i1".into(),
            "v1",
            Some("nope".into()),
            0,
            2000,
            0,
            TimelineItemKind::Av,
        )
        .unwrap_err();
        assert_eq!(err.code(), "not_found");
    }

    #[test]
    fn add_timeline_item_zero_duration_rejected() {
        let p = base();
        let err = add_timeline_item(
            &p,
            "i1".into(),
            "v1",
            Some("m1".into()),
            1000,
            1000,
            0,
            TimelineItemKind::Av,
        )
        .unwrap_err();
        assert_eq!(err.code(), "validation");
    }

    // ── split_timeline_item ─────────────────────────────────────────────────
    #[test]
    fn split_timeline_item_splits_at_mapped_source() {
        let mut p = base();
        p.timeline_items = vec![item("i1", "v1", Some("m1"), 0, 0, 2000)];
        // Split at timeline 800 → source 800 (speed 1).
        let r = split_timeline_item(&p, "i1", 800, "i1b".into()).unwrap();
        assert_eq!(r.timeline_items.len(), 2);
        let left = &r.timeline_items[0];
        let right = &r.timeline_items[1];
        assert_eq!(left.id, "i1");
        assert_eq!((left.in_ms, left.out_ms), (0, 800));
        assert_eq!(right.id, "i1b");
        assert_eq!((right.in_ms, right.out_ms), (800, 2000));
        assert_eq!(right.timeline_start_ms, 800);
    }

    #[test]
    fn split_timeline_item_respects_speed() {
        let mut p = base();
        let mut it = item("i1", "v1", Some("m1"), 0, 0, 2000);
        it.speed = 2.0; // 2000ms source plays in 1000ms timeline
        p.timeline_items = vec![it];
        // Split at timeline 500 → source 0 + 500*2 = 1000.
        let r = split_timeline_item(&p, "i1", 500, "i1b".into()).unwrap();
        assert_eq!(r.timeline_items[0].out_ms, 1000);
        assert_eq!(r.timeline_items[1].in_ms, 1000);
    }

    #[test]
    fn split_timeline_item_at_boundary_rejected() {
        let mut p = base();
        p.timeline_items = vec![item("i1", "v1", Some("m1"), 0, 0, 2000)];
        let err = split_timeline_item(&p, "i1", 0, "x".into()).unwrap_err();
        assert_eq!(err.code(), "validation");
        let err = split_timeline_item(&p, "i1", 2000, "x".into()).unwrap_err();
        assert_eq!(err.code(), "validation");
    }

    #[test]
    fn split_timeline_item_missing_is_not_found() {
        let p = base();
        let err = split_timeline_item(&p, "nope", 500, "x".into()).unwrap_err();
        assert_eq!(err.code(), "not_found");
    }

    // ── trim_timeline_item ──────────────────────────────────────────────────
    #[test]
    fn trim_timeline_item_adjusts_out_edge() {
        let mut p = base();
        p.timeline_items = vec![item("i1", "v1", Some("m1"), 0, 0, 2000)];
        let r = trim_timeline_item(&p, "i1", None, Some(1500), None).unwrap();
        assert_eq!(r.timeline_items[0].out_ms, 1500);
    }

    #[test]
    fn trim_timeline_item_clamps_out_to_media() {
        let mut p = base();
        p.timeline_items = vec![item("i1", "v1", Some("m1"), 0, 0, 2000)];
        let r = trim_timeline_item(&p, "i1", None, Some(9000), None).unwrap();
        assert_eq!(r.timeline_items[0].out_ms, 5000); // media dur
    }

    #[test]
    fn trim_timeline_item_clamps_start_to_prev_neighbour() {
        let mut p = base();
        p.timeline_items = vec![
            item("i1", "v1", Some("m1"), 0, 0, 1000),
            item("i2", "v1", Some("m1"), 1000, 1000, 2000),
        ];
        // Try to drag i2's start back to 200 — clamps to i1's end (1000).
        let r = trim_timeline_item(&p, "i2", None, None, Some(200)).unwrap();
        assert_eq!(r.timeline_items[1].timeline_start_ms, 1000);
        r.validate_timeline().unwrap();
    }

    #[test]
    fn trim_timeline_item_keeps_in_less_than_out() {
        let mut p = base();
        p.timeline_items = vec![item("i1", "v1", Some("m1"), 0, 0, 2000)];
        // in past out → clamped so in < out.
        let r = trim_timeline_item(&p, "i1", Some(3000), None, None).unwrap();
        let it = &r.timeline_items[0];
        assert!(it.in_ms < it.out_ms);
    }

    #[test]
    fn trim_timeline_item_missing_is_not_found() {
        let p = base();
        let err = trim_timeline_item(&p, "nope", Some(0), None, None).unwrap_err();
        assert_eq!(err.code(), "not_found");
    }

    // ── move_timeline_item ──────────────────────────────────────────────────
    #[test]
    fn move_timeline_item_along_time() {
        let mut p = base();
        p.timeline_items = vec![item("i1", "v1", Some("m1"), 0, 0, 1000)];
        let r = move_timeline_item(&p, "i1", "v1", 3000).unwrap();
        assert_eq!(r.timeline_items[0].timeline_start_ms, 3000);
    }

    #[test]
    fn move_timeline_item_across_tracks() {
        let mut p = base();
        p.tracks.push(track("v2", TrackKind::Video, 1));
        p.timeline_items = vec![item("i1", "v1", Some("m1"), 0, 0, 1000)];
        let r = move_timeline_item(&p, "i1", "v2", 0).unwrap();
        assert_eq!(r.timeline_items[0].track_id, "v2");
    }

    #[test]
    fn move_timeline_item_clamps_negative_start() {
        let mut p = base();
        p.timeline_items = vec![item("i1", "v1", Some("m1"), 0, 0, 1000)];
        let r = move_timeline_item(&p, "i1", "v1", -500).unwrap();
        assert_eq!(r.timeline_items[0].timeline_start_ms, 0);
    }

    #[test]
    fn move_timeline_item_shifts_off_overlap_on_lane() {
        let mut p = base();
        p.timeline_items = vec![
            item("i1", "v1", Some("m1"), 0, 0, 2000),
            item("i2", "v1", Some("m1"), 2000, 0, 1000),
        ];
        // Ask to drop i2 at 500 — would overlap i1 (ends 2000); shift to end (2000).
        let r = move_timeline_item(&p, "i2", "v1", 500).unwrap();
        assert_eq!(r.timeline_items[1].timeline_start_ms, 2000);
        r.validate_timeline().unwrap();
    }

    #[test]
    fn move_timeline_item_unknown_track_rejected() {
        let mut p = base();
        p.timeline_items = vec![item("i1", "v1", Some("m1"), 0, 0, 1000)];
        let err = move_timeline_item(&p, "i1", "nope", 0).unwrap_err();
        assert_eq!(err.code(), "not_found");
    }

    // ── ripple_delete_item ──────────────────────────────────────────────────
    #[test]
    fn ripple_delete_closes_gap() {
        let mut p = base();
        p.timeline_items = vec![
            item("i1", "v1", Some("m1"), 0, 0, 1000),
            item("i2", "v1", Some("m1"), 1000, 0, 1000),
            item("i3", "v1", Some("m1"), 2000, 0, 1000),
        ];
        // Delete i2 (dur 1000); i3 slides left to 1000.
        let r = ripple_delete_item(&p, "i2").unwrap();
        assert_eq!(r.timeline_items.len(), 2);
        let i3 = r.timeline_items.iter().find(|it| it.id == "i3").unwrap();
        assert_eq!(i3.timeline_start_ms, 1000);
        r.validate_timeline().unwrap();
    }

    #[test]
    fn ripple_delete_missing_is_not_found() {
        let p = base();
        let err = ripple_delete_item(&p, "nope").unwrap_err();
        assert_eq!(err.code(), "not_found");
    }

    // ── transitions ─────────────────────────────────────────────────────────
    #[test]
    fn set_transition_clamps_duration_to_clip_length() {
        let mut p = base();
        p.timeline_items = vec![item("i1", "v1", Some("m1"), 0, 0, 1000)];
        let r = set_transition(&p, "i1", "crossfade".into(), 9000).unwrap();
        let t = r.timeline_items[0].transition_in.as_ref().unwrap();
        assert_eq!(t.kind, "crossfade");
        assert_eq!(t.duration_ms, 1000); // clamped to clip length
    }

    #[test]
    fn clear_transition_removes_it() {
        let mut p = base();
        let mut it = item("i1", "v1", Some("m1"), 0, 0, 1000);
        it.transition_in = Some(Transition {
            kind: "crossfade".into(),
            duration_ms: 200,
        });
        p.timeline_items = vec![it];
        let r = clear_transition(&p, "i1").unwrap();
        assert!(r.timeline_items[0].transition_in.is_none());
    }

    #[test]
    fn set_transition_missing_is_not_found() {
        let p = base();
        let err = set_transition(&p, "nope", "crossfade".into(), 100).unwrap_err();
        assert_eq!(err.code(), "not_found");
    }

    // ── set_transform ───────────────────────────────────────────────────────
    #[test]
    fn set_transform_clamps_opacity_and_scale() {
        let mut p = base();
        p.timeline_items = vec![item("i1", "v1", Some("m1"), 0, 0, 1000)];
        let t = Transform {
            x: 0.1,
            y: 0.2,
            scale: -3.0,
            rotation_deg: 45.0,
            opacity: 5.0,
            crop: None,
        };
        let r = set_transform(&p, "i1", t).unwrap();
        let got = &r.timeline_items[0].transform;
        assert_eq!(got.opacity, 1.0);
        assert_eq!(got.scale, 0.0);
        assert_eq!(got.rotation_deg, 45.0);
    }

    #[test]
    fn set_transform_missing_is_not_found() {
        let p = base();
        let err = set_transform(&p, "nope", Transform::default()).unwrap_err();
        assert_eq!(err.code(), "not_found");
    }

    // ── add_text_item ───────────────────────────────────────────────────────
    #[test]
    fn add_text_item_builds_text_clip() {
        let mut p = base();
        p.tracks.push(track("ov", TrackKind::Overlay, 1));
        let r = add_text_item(&p, "t1".into(), "ov", 500, 3000, "Hello".into()).unwrap();
        let it = r.timeline_items.iter().find(|i| i.id == "t1").unwrap();
        assert_eq!(it.kind, TimelineItemKind::Text);
        assert!(it.source_media_id.is_none());
        assert_eq!((it.in_ms, it.out_ms), (0, 3000));
        assert_eq!(it.timeline_start_ms, 500);
        assert_eq!(it.text.as_ref().unwrap().text, "Hello");
    }

    #[test]
    fn add_text_item_clamps_duration_and_start() {
        let mut p = base();
        p.tracks.push(track("ov", TrackKind::Overlay, 1));
        let r = add_text_item(&p, "t1".into(), "ov", -100, 0, "Hi".into()).unwrap();
        let it = &r.timeline_items.iter().find(|i| i.id == "t1").unwrap();
        assert_eq!(it.timeline_start_ms, 0);
        assert_eq!(it.out_ms, 1); // duration clamped to >= 1
    }

    #[test]
    fn add_text_item_unknown_track_rejected() {
        let p = base();
        let err = add_text_item(&p, "t1".into(), "nope", 0, 1000, "Hi".into()).unwrap_err();
        assert_eq!(err.code(), "not_found");
    }
}
