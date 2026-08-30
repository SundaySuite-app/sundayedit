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

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::error::{AppError, AppResult};
use crate::model::{
    Effect, MediaItem, Project, TextSpec, TimelineItem, TimelineItemKind, Track, TrackKind,
    Transform, Transition,
};
use crate::services::video::{self, VideoMetadata};

// ── finalize ──────────────────────────────────────────────────────────────────

/// Normalise the clamp-only fields, run both invariant checks, and return the
/// project, mapping either failure to `AppError::Invariant`.
///
/// `clamp_playback_params` runs FIRST and for EVERY op, not just the audio
/// ones: a fade is measured against the clip's timeline length, and half the
/// ops in this module (trim, split, ripple, relink) change that length. Doing
/// the clamp here means a 5 s fade on a clip trimmed down to 2 s comes back
/// out at 2 s without `trim_timeline_item` ever having heard of fades — the
/// alternative is the seam bug where each op remembers some of the fields.
fn finalize(mut next: Project) -> AppResult<Project> {
    next.clamp_playback_params();
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

// ── relink ─────────────────────────────────────────────────────────────────────

/// Point an existing `MediaItem` at a file that moved or was renamed.
///
/// The ONE thing that must not change is the media **id** — every
/// `TimelineItem.source_media_id` and every filmstrip/thumbnail cache key
/// hangs off it, so an id-preserving update is what makes this a repair
/// rather than a re-import that orphans the edit.
///
/// This is the only op in the module that touches the filesystem: the new
/// file has to be probed and hashed before we can honestly claim its
/// duration. The state transition itself stays pure in `apply_relink`, which
/// is where the interesting behaviour (and its tests) live.
///
/// Errors when `new_path` is absent or unprobeable — a relink to a file we
/// cannot read would write a lie into the project.
pub fn relink_media(project: &Project, media_id: &str, new_path: &str) -> AppResult<Project> {
    // Fail before spawning ffprobe so the message names the real problem.
    if !std::path::Path::new(new_path).exists() {
        return Err(AppError::VideoMissing(new_path.to_string()));
    }
    // Cheap existence check on the media id too — no point probing a large
    // file only to discover the pool entry was never there.
    find_media(project, media_id)?;

    let meta = video::probe(std::path::Path::new(new_path))?;
    let hash = video::content_hash(std::path::Path::new(new_path))?;
    apply_relink(project, media_id, new_path, &meta, hash)
}

/// Pure half of [`relink_media`]: swap a pool entry's file facts for the
/// already-probed facts of `new_path`, then repair anything the swap
/// invalidated.
///
/// Two consequences a naive field update would get wrong:
///
///  1. **A shorter replacement.** Clips cut against the old duration may now
///     reference source time past the new end, which `validate_timeline`
///     rejects outright. Following the module's clamp-don't-reject rule we
///     pull each affected clip back into `[0, new_duration)` instead of
///     failing the whole relink — a slightly short clip the user can see and
///     re-trim beats an unopenable project. A clip that merely overhangs the
///     end keeps its `in_ms` and loses the overhang; a clip that now starts
///     entirely past the end slides back to the tail, keeping its length
///     where the new file is long enough to hold it. Either way the clip's
///     source span can only shrink or slide, never grow, so its
///     `timeline_end_ms` never advances and no new overlap is created.
///  2. **The primary video.** `Project::video_path` and friends are the
///     legacy scalars the burn-in fast path and the legacy preview read
///     directly, bypassing the pool. If the relinked item IS the primary
///     (its OLD path is what the scalars point at), they have to move with
///     it or the repaired project still opens a dead file.
///
/// Captions are deliberately left alone even when the new file is shorter.
/// They are the flagship data and carry their own timing; silently dropping
/// words the user transcribed would be a far worse loss than a caption that
/// runs past the end of a replacement clip.
pub fn apply_relink(
    project: &Project,
    media_id: &str,
    new_path: &str,
    meta: &VideoMetadata,
    content_hash: String,
) -> AppResult<Project> {
    let idx = project
        .media
        .iter()
        .position(|m| m.id == media_id)
        .ok_or_else(|| AppError::NotFound {
            entity: "media",
            id: media_id.to_string(),
        })?;

    let new_dur = meta.duration_ms;
    if new_dur <= 0 {
        return Err(AppError::Validation(format!(
            "{} reports a duration of {} ms — nothing to relink to",
            new_path, new_dur
        )));
    }

    let old_path = project.media[idx].path.clone();
    let old_hash = project.media[idx].content_hash.clone();
    // The pool entry is primary when the legacy scalars still point at the
    // file it USED to be — compare before we overwrite it.
    let is_primary = old_path == project.video_path;

    let original_filename = std::path::Path::new(new_path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(&project.media[idx].original_filename)
        .to_string();

    let mut next = project.clone();

    {
        let m = &mut next.media[idx];
        m.path = new_path.to_string();
        m.content_hash = content_hash.clone();
        // A relink can legitimately change what the file IS — swapping a
        // camera master for an audio-only bounce. A stale `Video` kind would
        // keep the preview trying to draw frames that aren't there.
        m.kind = meta.kind;
        m.duration_ms = new_dur;
        m.width = meta.width;
        m.height = meta.height;
        m.fps = meta.fps;
        m.has_audio = meta.audio_codec.is_some();
        m.original_filename = original_filename;
        // The extracted WAV is cached under the OLD content hash. Same bytes
        // (a pure move/rename) → the cache is still correct and re-extracting
        // a long talk would be wasteful; different bytes → it is now wrong
        // audio, so drop it and let the next transcribe/waveform re-extract.
        if content_hash != old_hash {
            m.audio_wav_path = None;
        }
    }

    // ── clamp clips whose source range no longer fits ────────────────────────
    for it in next.timeline_items.iter_mut() {
        if it.source_media_id.as_deref() != Some(media_id) {
            continue;
        }
        if it.in_ms >= 0 && it.out_ms <= new_dur && it.in_ms < it.out_ms {
            continue; // already inside the new bounds
        }
        let len = (it.out_ms - it.in_ms).max(1);
        if it.in_ms >= new_dur {
            // Wholly past the new end: slide the window back to the tail,
            // keeping the clip's length when the new file can hold it.
            it.out_ms = new_dur;
            it.in_ms = (new_dur - len).max(0);
        } else {
            // Overhangs the end (or a negative in from a malformed file):
            // keep the start, drop the overhang.
            it.in_ms = it.in_ms.max(0);
            it.out_ms = it.out_ms.clamp(it.in_ms + 1, new_dur);
        }
        // Both branches land `0 <= in < out <= new_dur`: `new_dur >= 1` is
        // guaranteed above, the slide branch keeps `len >= 1`, and the
        // overhang branch only runs while `in_ms + 1 <= new_dur`.
    }

    // ── carry the legacy primary-video scalars ───────────────────────────────
    if is_primary {
        next.video_path = new_path.to_string();
        next.video_content_hash = content_hash;
        next.video_duration_ms = new_dur;
        next.video_width = meta.width;
        next.video_height = meta.height;
        next.video_fps = meta.fps;
        // Same reasoning as the pool entry's cached WAV: keyed by content
        // hash, so it survives a move and is dropped on a content change.
        if old_hash != next.video_content_hash {
            next.audio_wav_path = None;
        }
    }

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
        volume_db: 0.0,
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
    // Reshuffle in index order (the visual order). Take the vec out of the
    // clone instead of cloning the tracks a second time.
    let mut ordered = std::mem::take(&mut next.tracks);
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
        gain_db: 0.0,
        fade_in_ms: 0,
        fade_out_ms: 0,
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
    next.timeline_items[idx] = left; // replace in place — one shift, not three
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

// ── gap engine ──────────────────────────────────────────────────────────────────
//
// A *gap* is empty time on one track: from 0 to the first clip, or between two
// clips. There is no trailing gap — the track simply ends.
//
// **Protection.** A protected gap is one a ripple must not consume: the shift
// stops there and the material downstream keeps its timecode. We do NOT
// introduce a persisted gap entity for this. The marker is the existing,
// already-persisted `TimelineItem.locked` flag: *the gap immediately before a
// locked clip is protected*, because closing or shrinking past it would move a
// clip the user pinned. `Gap.protected` is therefore derived at query time,
// never stored — no new table, no new column, no SCHEMA_VERSION bump, and it
// composes with the lock affordance the UI already has. See docs/DECISIONS.md
// ADR-011 for the rejected alternative (a persisted gap entity).
//
// All four ops clamp rather than reject: an out-of-range `at_ms`, a zero
// duration, or a fully pinned track yields an unchanged project, not an error.

/// A stretch of empty time on one track. `end_ms` is exclusive.
///
/// `protected` means a ripple stops here (the clip that follows is locked).
/// Returned by `detect_gaps` — this is a query result, never persisted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../src/lib/bindings/Gap.ts")]
pub struct Gap {
    #[ts(type = "number")]
    pub start_ms: i64,
    #[ts(type = "number")]
    pub end_ms: i64,
    pub protected: bool,
}

impl Gap {
    /// Length of the gap in milliseconds.
    pub fn duration_ms(&self) -> i64 {
        self.end_ms - self.start_ms
    }
}

/// Every gap on `track_id`, left to right, including a leading gap when the
/// first clip does not start at 0. Pure query — takes no project mutation.
pub fn detect_gaps(project: &Project, track_id: &str) -> AppResult<Vec<Gap>> {
    require_track(project, track_id)?;
    let items = track_items_sorted(project, track_id);

    let mut gaps = Vec::new();
    let mut cursor = 0i64;
    for it in &items {
        if it.timeline_start_ms > cursor {
            gaps.push(Gap {
                start_ms: cursor,
                end_ms: it.timeline_start_ms,
                protected: it.locked,
            });
        }
        // `max` so overlapping clips (legal on Caption/Overlay tracks) don't
        // manufacture a phantom gap behind the cursor.
        cursor = cursor.max(it.timeline_end_ms());
    }
    Ok(gaps)
}

/// Open `duration_ms` of empty time at `at_ms`: every clip that STARTS at or
/// after `at_ms` slides right. A clip straddling `at_ms` keeps its place — use
/// `split_timeline_item` first if you want to cut it open.
///
/// The ripple stops at the first locked clip at or after `at_ms`: the clips in
/// between absorb as much of the shift as the protected gap allows, and the
/// locked clip never moves. With no headroom the op is a no-op.
pub fn insert_gap_with_ripple(
    project: &Project,
    track_id: &str,
    at_ms: i64,
    duration_ms: i64,
) -> AppResult<Project> {
    require_track(project, track_id)?;
    let at = at_ms.max(0);
    let requested = duration_ms.max(0);
    if requested == 0 {
        return finalize(project.clone());
    }

    let items = track_items_sorted(project, track_id);
    let barrier = items
        .iter()
        .position(|it| it.timeline_start_ms >= at && it.locked);

    // Clips that may move: start >= at, and ahead of the barrier.
    let movable: Vec<String> = items
        .iter()
        .enumerate()
        .filter(|(i, it)| it.timeline_start_ms >= at && barrier.is_none_or(|b| *i < b))
        .map(|(_, it)| it.id.clone())
        .collect();
    if movable.is_empty() {
        return finalize(project.clone());
    }

    // Clamp the shift to the headroom in front of the barrier.
    let shift = match barrier {
        None => requested,
        Some(b) => {
            let last_end = items[..b]
                .iter()
                .filter(|it| it.timeline_start_ms >= at)
                .map(|it| it.timeline_end_ms())
                .max()
                .unwrap_or(at);
            requested.min((items[b].timeline_start_ms - last_end).max(0))
        }
    };
    if shift == 0 {
        return finalize(project.clone());
    }

    let mut next = project.clone();
    for it in next.timeline_items.iter_mut() {
        if movable.iter().any(|id| id == &it.id) {
            it.timeline_start_ms += shift;
        }
    }
    finalize(next)
}

/// Close the gap containing `at_ms`: every clip from the gap's end onwards
/// slides left by the gap's length.
///
/// No-op (not an error) when `at_ms` is not inside a gap, or when the gap is
/// protected. The ripple stops at the first locked clip downstream, which then
/// keeps its timecode while the material before it packs up against the gap.
pub fn remove_gap_with_ripple(project: &Project, track_id: &str, at_ms: i64) -> AppResult<Project> {
    require_track(project, track_id)?;
    let at = at_ms.max(0);

    let gaps = detect_gaps(project, track_id)?;
    let Some(gap) = gaps
        .into_iter()
        .find(|g| at >= g.start_ms && at < g.end_ms)
        .filter(|g| !g.protected)
    else {
        return finalize(project.clone());
    };
    let len = gap.duration_ms();

    let items = track_items_sorted(project, track_id);
    let barrier = items
        .iter()
        .position(|it| it.timeline_start_ms >= gap.end_ms && it.locked);
    let movable: Vec<String> = items
        .iter()
        .enumerate()
        .filter(|(i, it)| it.timeline_start_ms >= gap.end_ms && barrier.is_none_or(|b| *i < b))
        .map(|(_, it)| it.id.clone())
        .collect();
    if movable.is_empty() {
        return finalize(project.clone());
    }

    let mut next = project.clone();
    for it in next.timeline_items.iter_mut() {
        if movable.iter().any(|id| id == &it.id) {
            it.timeline_start_ms = (it.timeline_start_ms - len).max(gap.start_ms);
        }
    }
    finalize(next)
}

/// Close every gap on the track, left to right: each clip slides back against
/// its predecessor (the first one to 0).
///
/// Locked clips are anchors — they keep their timecode, and the clips after
/// them pack against the anchor's end rather than sliding past it. So the gap
/// in front of a locked clip survives (and grows, since everything upstream
/// moved left): that is exactly what "protected" buys the user.
///
/// Clips never move right, so a track with no gaps is unchanged.
pub fn pack_track(project: &Project, track_id: &str) -> AppResult<Project> {
    require_track(project, track_id)?;
    let items = track_items_sorted(project, track_id);

    let mut cursor = 0i64;
    let mut moves: Vec<(String, i64)> = Vec::new();
    for it in &items {
        if it.locked {
            cursor = cursor.max(it.timeline_end_ms());
            continue;
        }
        let dur = it.timeline_end_ms() - it.timeline_start_ms;
        // `min` is belt-and-braces: on a well-formed track the cursor is never
        // ahead of the clip, and we must never shove a clip to the right.
        let start = cursor.min(it.timeline_start_ms);
        moves.push((it.id.clone(), start));
        cursor = start + dur;
    }

    let mut next = project.clone();
    for it in next.timeline_items.iter_mut() {
        if let Some((_, start)) = moves.iter().find(|(id, _)| id == &it.id) {
            it.timeline_start_ms = *start;
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

// ── curated effects (E6) ──────────────────────────────────────────────────────

/// Add or update ONE curated colour effect on a clip.
///
/// The effect `kind` is its identity: setting `brightness` twice moves the
/// existing entry instead of stacking a second one, so the inspector's "one row
/// per effect" UI and the stored list cannot drift apart. Params are filtered
/// to the keys the registry declares and clamped to their ranges (house rule:
/// clamp, don't reject), so a slider that overshoots — or a hand-edited project
/// file — lands on a value the export can actually render.
///
/// A non-curated `kind` is REJECTED here rather than clamped: unlike a value,
/// there is no in-range meaning for an effect neither the preview nor the
/// export can produce, and silently storing it would put something in the
/// project file that renders as nothing (`effects::filter_fragment` → `None`).
pub fn set_effect(
    project: &Project,
    item_id: &str,
    kind: &str,
    params: &serde_json::Value,
    enabled: bool,
) -> AppResult<Project> {
    let def = crate::services::effects::definition(kind).ok_or_else(|| {
        AppError::Validation(format!(
            "effect kind `{kind}` is not in the curated registry (preview↔export parity)"
        ))
    })?;

    // Keep ONLY declared params, each clamped to its declared range. A probe
    // Effect carries the caller's bag so `effects::param` does the reading —
    // one implementation of "what does this param mean", shared with export.
    let probe = Effect {
        id: String::new(),
        kind: kind.to_string(),
        params: params.clone(),
        enabled,
    };
    let mut clean = serde_json::Map::new();
    for p in def.params {
        let v = crate::services::effects::param(&probe, p);
        clean.insert(
            p.name.to_string(),
            serde_json::Number::from_f64(v)
                .map(serde_json::Value::Number)
                .unwrap_or(serde_json::Value::Null),
        );
    }
    let params = serde_json::Value::Object(clean);

    let mut next = project.clone();
    let it = find_item_mut(&mut next, item_id)?;
    match it.effects.iter_mut().find(|e| e.kind == kind) {
        Some(existing) => {
            existing.params = params;
            existing.enabled = enabled;
        }
        None => it.effects.push(Effect {
            // Deterministic per (item, kind): the pair is already unique, and a
            // stable id keeps undo/redo snapshots comparable.
            id: format!("fx-{kind}"),
            kind: kind.to_string(),
            params,
            enabled,
        }),
    }
    finalize(next)
}

// ── audio levels (R2) ─────────────────────────────────────────────────────────

/// Set any subset of a clip's audio parameters. `None` leaves a field
/// unchanged — the same "omit to keep" shape `set_track_flags` and
/// `trim_timeline_item` already use, so the inspector can drive one slider
/// without having to resend the other two.
///
/// Everything is CLAMPED rather than rejected, and the clamping is NOT done
/// here: `finalize` runs `Project::clamp_playback_params` on the result, which
/// is the same code the trim/split ops go through. A fade longer than the clip
/// therefore lands at exactly the clip's length whether it got there by this
/// op or by a later trim.
pub fn set_item_audio(
    project: &Project,
    item_id: &str,
    gain_db: Option<f32>,
    fade_in_ms: Option<i64>,
    fade_out_ms: Option<i64>,
) -> AppResult<Project> {
    let mut next = project.clone();
    let it = find_item_mut(&mut next, item_id)?;
    if let Some(v) = gain_db {
        it.gain_db = v;
    }
    if let Some(v) = fade_in_ms {
        it.fade_in_ms = v;
    }
    if let Some(v) = fade_out_ms {
        it.fade_out_ms = v;
    }
    finalize(next)
}

/// Set a track's fader, in dB. Clamped to `[GAIN_DB_MIN, GAIN_DB_MAX]` by
/// `finalize`.
///
/// Deliberately separate from `set_track_flags`: mute/solo are switches whose
/// state you can flip back, the fader is a continuous value the UI drags in
/// bursts (`commit` with a `coalesceKey`), and folding them together would put
/// a slider drag and a mute toggle in the same undo entry.
pub fn set_track_volume(project: &Project, track_id: &str, volume_db: f32) -> AppResult<Project> {
    let mut next = project.clone();
    let track = next
        .tracks
        .iter_mut()
        .find(|t| t.id == track_id)
        .ok_or_else(|| AppError::NotFound {
            entity: "track",
            id: track_id.to_string(),
        })?;
    track.volume_db = volume_db;
    finalize(next)
}

/// Remove a clip's effect of the given kind. Removing one that isn't there is a
/// no-op, not an error (idempotent, like `clear_transition`).
pub fn remove_effect(project: &Project, item_id: &str, kind: &str) -> AppResult<Project> {
    let mut next = project.clone();
    let it = find_item_mut(&mut next, item_id)?;
    it.effects.retain(|e| e.kind != kind);
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
        gain_db: 0.0,
        fade_in_ms: 0,
        fade_out_ms: 0,
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

fn require_track<'a>(project: &'a Project, track_id: &str) -> AppResult<&'a Track> {
    project
        .tracks
        .iter()
        .find(|t| t.id == track_id)
        .ok_or_else(|| AppError::NotFound {
            entity: "track",
            id: track_id.to_string(),
        })
}

/// Every clip on `track_id`, ordered by timeline position. Ties broken by id so
/// the order is deterministic (two clips may share a start on Caption/Overlay
/// tracks, where overlap is legal).
fn track_items_sorted<'a>(project: &'a Project, track_id: &str) -> Vec<&'a TimelineItem> {
    let mut items: Vec<&TimelineItem> = project
        .timeline_items
        .iter()
        .filter(|it| it.track_id == track_id)
        .collect();
    items.sort_by(|a, b| {
        a.timeline_start_ms
            .cmp(&b.timeline_start_ms)
            .then_with(|| a.id.cmp(&b.id))
    });
    items
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

    // ── relink_media / apply_relink ─────────────────────────────────────────
    //
    // The IO half (`relink_media`) only probes + hashes and hands off, so the
    // whole state transition is exercised here through `apply_relink` with a
    // synthetic probe — no ffmpeg, no fixture files.

    fn probe(dur: i64) -> VideoMetadata {
        VideoMetadata {
            duration_ms: dur,
            width: 1280,
            height: 720,
            fps: 25.0,
            video_codec: Some("h264".into()),
            audio_codec: Some("aac".into()),
            audio_channels: Some(2),
            audio_sample_rate: Some(48_000),
            container: Some("mov,mp4".into()),
            kind: MediaKind::Video,
        }
    }

    #[test]
    fn relink_keeps_the_media_id_so_clips_stay_attached() {
        let mut p = base();
        p.timeline_items = vec![item("i1", "v1", Some("m1"), 0, 0, 4000)];
        let r = apply_relink(&p, "m1", "/moved/new.mp4", &probe(5000), "h2".into()).unwrap();

        assert_eq!(r.media[0].id, "m1", "the id is the anchor — never reissued");
        assert_eq!(
            r.timeline_items[0].source_media_id.as_deref(),
            Some("m1"),
            "the clip must still resolve to the pool entry"
        );
        assert_eq!(r.timeline_items[0].in_ms, 0);
        assert_eq!(r.timeline_items[0].out_ms, 4000);
    }

    #[test]
    fn relink_updates_every_file_fact_including_the_basename() {
        let p = base();
        let mut meta = probe(7777);
        meta.audio_codec = None;
        meta.kind = MediaKind::AudioOnly;
        let r = apply_relink(&p, "m1", "/elsewhere/Talen 2026.m4a", &meta, "hNEW".into()).unwrap();

        let m = &r.media[0];
        assert_eq!(m.path, "/elsewhere/Talen 2026.m4a");
        assert_eq!(m.content_hash, "hNEW");
        assert_eq!(m.duration_ms, 7777);
        assert_eq!(m.width, 1280);
        assert_eq!(m.height, 720);
        assert_eq!(m.fps, 25.0);
        assert!(!m.has_audio, "no audio stream in the probe");
        assert_eq!(m.kind, MediaKind::AudioOnly);
        assert_eq!(
            m.original_filename, "Talen 2026.m4a",
            "basename of the new path, spaces and all"
        );
    }

    // ── the interesting case: a SHORTER replacement ─────────────────────────

    #[test]
    fn relink_to_shorter_file_clamps_overhanging_clip() {
        // Clip cut against the 5.000 ms original now overhangs a 3.000 ms
        // replacement. Keep the start, drop the overhang — and stay valid.
        let mut p = base();
        p.timeline_items = vec![item("i1", "v1", Some("m1"), 0, 1000, 5000)];
        let r = apply_relink(&p, "m1", "/short.mp4", &probe(3000), "h2".into()).unwrap();

        assert_eq!(r.timeline_items[0].in_ms, 1000, "start of the cut survives");
        assert_eq!(r.timeline_items[0].out_ms, 3000, "clamped to the new end");
        r.validate_timeline()
            .expect("finalize already ran, but pin it explicitly");
    }

    #[test]
    fn relink_to_shorter_file_slides_a_clip_that_now_starts_past_the_end() {
        // in_ms 4000 is past a 3.000 ms replacement entirely — clamping out
        // alone would leave in >= out. Slide the window back to the tail,
        // keeping the 800 ms length the new file can still hold.
        let mut p = base();
        p.timeline_items = vec![item("i1", "v1", Some("m1"), 0, 4000, 4800)];
        let r = apply_relink(&p, "m1", "/short.mp4", &probe(3000), "h2".into()).unwrap();

        let it = &r.timeline_items[0];
        assert_eq!(it.in_ms, 2200);
        assert_eq!(it.out_ms, 3000);
        assert_eq!(it.out_ms - it.in_ms, 800, "length preserved");
    }

    #[test]
    fn relink_to_much_shorter_file_collapses_to_the_whole_new_file() {
        // The new file is shorter than the clip's own length: there is no
        // window to slide, so the clip becomes the entire new file rather
        // than the relink failing.
        let mut p = base();
        p.timeline_items = vec![item("i1", "v1", Some("m1"), 0, 3000, 5000)];
        let r = apply_relink(&p, "m1", "/tiny.mp4", &probe(400), "h2".into()).unwrap();

        assert_eq!(r.timeline_items[0].in_ms, 0);
        assert_eq!(r.timeline_items[0].out_ms, 400);
    }

    #[test]
    fn relink_to_shorter_file_clamps_every_affected_clip_and_leaves_others_alone() {
        let mut p = base();
        p.media.push(media("m2", 5000));
        p.tracks.push(track("v2", TrackKind::Video, 1));
        p.timeline_items = vec![
            item("i1", "v1", Some("m1"), 0, 0, 1000),       // fits
            item("i2", "v1", Some("m1"), 2000, 1500, 4500), // overhangs
            item("i3", "v2", Some("m2"), 0, 3000, 5000),    // other media — untouched
        ];
        let r = apply_relink(&p, "m1", "/short.mp4", &probe(2000), "h2".into()).unwrap();

        let by = |id: &str| r.timeline_items.iter().find(|i| i.id == id).unwrap();
        assert_eq!((by("i1").in_ms, by("i1").out_ms), (0, 1000));
        assert_eq!((by("i2").in_ms, by("i2").out_ms), (1500, 2000));
        assert_eq!(
            (by("i3").in_ms, by("i3").out_ms),
            (3000, 5000),
            "a clip on a different media must not be touched"
        );
    }

    #[test]
    fn relink_to_shorter_file_never_grows_a_clip_on_the_timeline() {
        // Clamping shrinks source spans, so timeline_end_ms can only move
        // left — a clamp can never manufacture an overlap with a neighbour.
        let mut p = base();
        p.timeline_items = vec![
            item("i1", "v1", Some("m1"), 0, 0, 4000),
            item("i2", "v1", Some("m1"), 4000, 1000, 5000),
        ];
        let before: Vec<i64> = p
            .timeline_items
            .iter()
            .map(|i| i.timeline_end_ms())
            .collect();
        let r = apply_relink(&p, "m1", "/short.mp4", &probe(2500), "h2".into()).unwrap();

        for (it, was) in r.timeline_items.iter().zip(before) {
            assert!(
                it.timeline_end_ms() <= was,
                "clip {} grew from {} to {}",
                it.id,
                was,
                it.timeline_end_ms()
            );
        }
    }

    #[test]
    fn relink_to_longer_file_leaves_clips_untouched() {
        let mut p = base();
        p.timeline_items = vec![item("i1", "v1", Some("m1"), 0, 1000, 5000)];
        let r = apply_relink(&p, "m1", "/long.mp4", &probe(60_000), "h2".into()).unwrap();
        assert_eq!(
            (r.timeline_items[0].in_ms, r.timeline_items[0].out_ms),
            (1000, 5000)
        );
    }

    // ── the legacy primary-video scalars ────────────────────────────────────

    #[test]
    fn relink_of_the_primary_media_carries_the_legacy_scalars() {
        // Without this the burn-in fast path and the legacy preview keep
        // reading the OLD path and the repaired project is still broken.
        let mut p = base();
        p.video_path = p.media[0].path.clone(); // "/v/m1.mp4"
        p.video_content_hash = "h".into();
        p.video_duration_ms = 5000;
        p.audio_wav_path = Some("/cache/h.wav".into());

        let r = apply_relink(&p, "m1", "/moved/m1.mp4", &probe(4000), "hNEW".into()).unwrap();

        assert_eq!(r.video_path, "/moved/m1.mp4");
        assert_eq!(r.video_content_hash, "hNEW");
        assert_eq!(r.video_duration_ms, 4000);
        assert_eq!(r.video_width, 1280);
        assert_eq!(r.video_height, 720);
        assert_eq!(r.video_fps, 25.0);
        assert_eq!(
            r.audio_wav_path, None,
            "the cached WAV is keyed by the old hash — different bytes, drop it"
        );
    }

    #[test]
    fn relink_of_a_non_primary_media_leaves_the_scalars_alone() {
        let mut p = base();
        p.video_path = "/somewhere/primary.mp4".into();
        p.video_content_hash = "hPRIMARY".into();
        p.video_duration_ms = 9999;
        p.audio_wav_path = Some("/cache/hPRIMARY.wav".into());

        let r = apply_relink(&p, "m1", "/moved/other.mp4", &probe(4000), "hNEW".into()).unwrap();

        assert_eq!(r.video_path, "/somewhere/primary.mp4");
        assert_eq!(r.video_content_hash, "hPRIMARY");
        assert_eq!(r.video_duration_ms, 9999);
        assert_eq!(r.audio_wav_path.as_deref(), Some("/cache/hPRIMARY.wav"));
    }

    #[test]
    fn relink_primary_by_pure_move_keeps_the_cached_wav() {
        // Same bytes, new path: the hash-keyed WAV is still correct, and
        // re-extracting an hour-long talk for a rename would be daft.
        let mut p = base();
        p.video_path = p.media[0].path.clone();
        p.video_content_hash = "h".into();
        p.audio_wav_path = Some("/cache/h.wav".into());
        p.media[0].audio_wav_path = Some("/cache/h.wav".into());

        let r = apply_relink(&p, "m1", "/moved/m1.mp4", &probe(5000), "h".into()).unwrap();

        assert_eq!(r.audio_wav_path.as_deref(), Some("/cache/h.wav"));
        assert_eq!(r.media[0].audio_wav_path.as_deref(), Some("/cache/h.wav"));
    }

    #[test]
    fn relink_drops_the_pool_wav_when_the_content_changed() {
        let mut p = base();
        p.media[0].audio_wav_path = Some("/cache/h.wav".into());
        let r = apply_relink(&p, "m1", "/other.mp4", &probe(5000), "hNEW".into()).unwrap();
        assert_eq!(r.media[0].audio_wav_path, None);
    }

    #[test]
    fn relink_leaves_captions_alone_even_when_the_file_shrinks() {
        // Captions are the flagship data; a shorter replacement must never
        // silently destroy transcribed words.
        let mut p = base();
        p.captions = vec![crate::model::Caption {
            id: "c1".into(),
            start_ms: 3000,
            end_ms: 4000, // well past the 1.000 ms replacement
            words: vec![crate::model::Word {
                text: "nåde".into(),
                start_ms: 3000,
                end_ms: 4000,
                confidence: 91.0,
                edited: false,
                locked: false,
                polished: false,
                alternates: vec![],
            }],
            speaker_id: None,
            style_id: None,
            notes: None,
            ai_generated: true,
            last_edited_at: 0,
            track_id: None,
        }];
        p.timeline_items = vec![item("i1", "v1", Some("m1"), 0, 0, 5000)];
        let r = apply_relink(&p, "m1", "/short.mp4", &probe(1000), "h2".into()).unwrap();

        assert_eq!(r.captions.len(), 1);
        assert_eq!(r.captions[0].words.len(), 1);
        assert_eq!((r.captions[0].start_ms, r.captions[0].end_ms), (3000, 4000));
        assert_eq!(r.captions[0].words[0].confidence, 91.0);
    }

    #[test]
    fn relink_unknown_media_is_not_found() {
        let p = base();
        let err = apply_relink(&p, "nope", "/x.mp4", &probe(1000), "h".into()).unwrap_err();
        assert_eq!(err.code(), "not_found");
    }

    #[test]
    fn relink_to_zero_duration_file_is_rejected() {
        let p = base();
        let err = apply_relink(&p, "m1", "/empty.mp4", &probe(0), "h".into()).unwrap_err();
        assert_eq!(err.code(), "validation");
    }

    #[test]
    fn relink_media_errors_when_the_path_does_not_exist() {
        // The IO wrapper's own guard — it must not reach ffprobe.
        let p = base();
        let err = relink_media(&p, "m1", "/definitely/not/here/nope.mp4").unwrap_err();
        assert_eq!(err.code(), "video_missing");
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

    // ── set_item_audio / set_track_volume (R2) ──────────────────────────────

    fn with_audio_item() -> Project {
        let mut p = base();
        // 4 s clip so fade clamping has room to be interesting.
        p.timeline_items = vec![item("i1", "v1", Some("m1"), 0, 0, 4000)];
        p
    }

    #[test]
    fn set_item_audio_sets_all_three() {
        let p = with_audio_item();
        let r = set_item_audio(&p, "i1", Some(-6.0), Some(500), Some(750)).unwrap();
        let it = &r.timeline_items[0];
        assert_eq!(it.gain_db, -6.0);
        assert_eq!(it.fade_in_ms, 500);
        assert_eq!(it.fade_out_ms, 750);
    }

    /// `None` means "leave alone" — the inspector drives one slider at a time.
    #[test]
    fn set_item_audio_leaves_omitted_fields_untouched() {
        let p = with_audio_item();
        let a = set_item_audio(&p, "i1", Some(-3.0), Some(200), Some(300)).unwrap();

        let b = set_item_audio(&a, "i1", Some(-9.0), None, None).unwrap();
        assert_eq!(b.timeline_items[0].gain_db, -9.0);
        assert_eq!(b.timeline_items[0].fade_in_ms, 200, "fade-in untouched");
        assert_eq!(b.timeline_items[0].fade_out_ms, 300, "fade-out untouched");

        let c = set_item_audio(&b, "i1", None, Some(10), None).unwrap();
        assert_eq!(c.timeline_items[0].gain_db, -9.0, "gain untouched");
        assert_eq!(c.timeline_items[0].fade_in_ms, 10);

        let d = set_item_audio(&c, "i1", None, None, None).unwrap();
        assert_eq!(
            d.timeline_items[0], c.timeline_items[0],
            "all-None is a no-op"
        );
    }

    #[test]
    fn set_item_audio_clamps_gain_both_ways() {
        let p = with_audio_item();
        assert_eq!(
            set_item_audio(&p, "i1", Some(500.0), None, None)
                .unwrap()
                .timeline_items[0]
                .gain_db,
            crate::model::GAIN_DB_MAX
        );
        assert_eq!(
            set_item_audio(&p, "i1", Some(-500.0), None, None)
                .unwrap()
                .timeline_items[0]
                .gain_db,
            crate::model::GAIN_DB_MIN
        );
        assert_eq!(
            set_item_audio(&p, "i1", Some(f32::NAN), None, None)
                .unwrap()
                .timeline_items[0]
                .gain_db,
            0.0,
            "NaN has no in-range meaning — unity is the only safe reading"
        );
    }

    #[test]
    fn set_item_audio_clamps_fades_to_the_clip_length() {
        let p = with_audio_item();
        let r = set_item_audio(&p, "i1", None, Some(99_000), Some(-40)).unwrap();
        assert_eq!(
            r.timeline_items[0].fade_in_ms, 4000,
            "capped at clip length"
        );
        assert_eq!(r.timeline_items[0].fade_out_ms, 0, "negative means none");
    }

    /// The seam this whole `finalize`-normalises design exists for: a fade set
    /// on a long clip must shrink when a LATER, unrelated op shortens it. If
    /// `trim_timeline_item` had to remember fades, it eventually wouldn't.
    #[test]
    fn trimming_a_clip_shrinks_a_fade_that_no_longer_fits() {
        let p = with_audio_item();
        let faded = set_item_audio(&p, "i1", None, Some(3000), Some(3000)).unwrap();
        assert_eq!(faded.timeline_items[0].fade_in_ms, 3000);

        let trimmed = trim_timeline_item(&faded, "i1", None, Some(1000), None).unwrap();
        assert_eq!(trimmed.timeline_items[0].timeline_len_ms(), 1000);
        assert_eq!(
            trimmed.timeline_items[0].fade_in_ms, 1000,
            "the fade must not survive longer than the clip it fades"
        );
        assert_eq!(trimmed.timeline_items[0].fade_out_ms, 1000);
    }

    /// A split hands BOTH halves a shorter length; neither may keep an
    /// oversized fade.
    #[test]
    fn splitting_a_faded_clip_clamps_both_halves() {
        let p = with_audio_item();
        let faded = set_item_audio(&p, "i1", Some(-4.0), Some(3500), Some(3500)).unwrap();
        let split = split_timeline_item(&faded, "i1", 1000, "i2".into()).unwrap();
        assert_eq!(split.timeline_items.len(), 2);
        for it in &split.timeline_items {
            let len = it.timeline_len_ms();
            assert!(
                it.fade_in_ms <= len,
                "fade_in {} > len {len}",
                it.fade_in_ms
            );
            assert!(
                it.fade_out_ms <= len,
                "fade_out {} > len {len}",
                it.fade_out_ms
            );
            assert_eq!(it.gain_db, -4.0, "gain survives the split on both halves");
        }
    }

    #[test]
    fn set_item_audio_missing_item_is_not_found() {
        let p = with_audio_item();
        let err = set_item_audio(&p, "nope", Some(0.0), None, None).unwrap_err();
        assert_eq!(err.code(), "not_found");
    }

    #[test]
    fn set_track_volume_sets_and_clamps() {
        let p = with_audio_item();
        assert_eq!(
            set_track_volume(&p, "v1", -8.0).unwrap().tracks[0].volume_db,
            -8.0
        );
        assert_eq!(
            set_track_volume(&p, "v1", 999.0).unwrap().tracks[0].volume_db,
            crate::model::GAIN_DB_MAX
        );
        assert_eq!(
            set_track_volume(&p, "v1", -999.0).unwrap().tracks[0].volume_db,
            crate::model::GAIN_DB_MIN
        );
        assert_eq!(
            set_track_volume(&p, "v1", f32::NAN).unwrap().tracks[0].volume_db,
            0.0
        );
    }

    /// The fader is a level, not a switch: it must not disturb mute/solo, and
    /// mute/solo must not disturb it.
    #[test]
    fn track_volume_and_track_flags_are_independent() {
        let p = with_audio_item();
        let v = set_track_volume(&p, "v1", -12.0).unwrap();
        let m = set_track_flags(&v, "v1", None, None, Some(true), None).unwrap();
        assert_eq!(
            m.tracks[0].volume_db, -12.0,
            "muting keeps the fader position"
        );
        assert!(m.tracks[0].muted);

        let v2 = set_track_volume(&m, "v1", -2.0).unwrap();
        assert!(v2.tracks[0].muted, "moving the fader does not unmute");
        assert_eq!(v2.tracks[0].volume_db, -2.0);
    }

    #[test]
    fn set_track_volume_missing_track_is_not_found() {
        let p = with_audio_item();
        let err = set_track_volume(&p, "nope", 0.0).unwrap_err();
        assert_eq!(err.code(), "not_found");
    }

    // ── set_effect / remove_effect (E6) ─────────────────────────────────────
    fn with_one_item() -> Project {
        let mut p = base();
        p.timeline_items = vec![item("i1", "v1", Some("m1"), 0, 0, 1000)];
        p
    }

    #[test]
    fn set_effect_adds_a_curated_effect() {
        let p = with_one_item();
        let r = set_effect(
            &p,
            "i1",
            "brightness",
            &serde_json::json!({ "amount": 0.3 }),
            true,
        )
        .unwrap();
        let fx = &r.timeline_items[0].effects;
        assert_eq!(fx.len(), 1);
        assert_eq!(fx[0].kind, "brightness");
        assert_eq!(fx[0].id, "fx-brightness");
        assert!(fx[0].enabled);
        assert_eq!(fx[0].params["amount"].as_f64().unwrap(), 0.3);
    }

    #[test]
    fn set_effect_updates_in_place_instead_of_stacking() {
        let p = with_one_item();
        let r = set_effect(
            &p,
            "i1",
            "contrast",
            &serde_json::json!({ "amount": 1.2 }),
            true,
        )
        .unwrap();
        let r = set_effect(
            &r,
            "i1",
            "contrast",
            &serde_json::json!({ "amount": 1.8 }),
            false,
        )
        .unwrap();
        let fx = &r.timeline_items[0].effects;
        assert_eq!(fx.len(), 1, "one entry per kind");
        assert_eq!(fx[0].params["amount"].as_f64().unwrap(), 1.8);
        assert!(!fx[0].enabled);
    }

    #[test]
    fn set_effect_clamps_and_drops_undeclared_params() {
        let p = with_one_item();
        let r = set_effect(
            &p,
            "i1",
            "saturation",
            &serde_json::json!({ "amount": 99.0, "radius": 4, "note": "hi" }),
            true,
        )
        .unwrap();
        let params = &r.timeline_items[0].effects[0].params;
        assert_eq!(params["amount"].as_f64().unwrap(), 3.0, "clamped to max");
        assert!(params.get("radius").is_none(), "undeclared key dropped");
        assert!(params.get("note").is_none(), "undeclared key dropped");
    }

    #[test]
    fn set_effect_fills_in_the_neutral_default_for_a_missing_param() {
        let p = with_one_item();
        let r = set_effect(&p, "i1", "contrast", &serde_json::json!({}), true).unwrap();
        assert_eq!(
            r.timeline_items[0].effects[0].params["amount"]
                .as_f64()
                .unwrap(),
            1.0
        );
    }

    #[test]
    fn set_effect_stores_no_params_for_a_parameterless_effect() {
        let p = with_one_item();
        let r = set_effect(&p, "i1", "grayscale", &serde_json::json!({}), true).unwrap();
        assert_eq!(
            r.timeline_items[0].effects[0].params,
            serde_json::json!({}),
            "grayscale declares no params"
        );
    }

    #[test]
    fn set_effect_rejects_a_non_curated_kind() {
        let p = with_one_item();
        let err = set_effect(&p, "i1", "bloom", &serde_json::json!({}), true).unwrap_err();
        assert_eq!(err.code(), "validation");
        // …and nothing was written.
        assert!(p.timeline_items[0].effects.is_empty());
    }

    #[test]
    fn set_effect_missing_item_is_not_found() {
        let p = with_one_item();
        let err = set_effect(&p, "nope", "grayscale", &serde_json::json!({}), true).unwrap_err();
        assert_eq!(err.code(), "not_found");
    }

    #[test]
    fn remove_effect_drops_only_that_kind() {
        let p = with_one_item();
        let p = set_effect(&p, "i1", "grayscale", &serde_json::json!({}), true).unwrap();
        let p = set_effect(
            &p,
            "i1",
            "brightness",
            &serde_json::json!({ "amount": 0.5 }),
            true,
        )
        .unwrap();
        let r = remove_effect(&p, "i1", "grayscale").unwrap();
        let kinds: Vec<&str> = r.timeline_items[0]
            .effects
            .iter()
            .map(|e| e.kind.as_str())
            .collect();
        assert_eq!(kinds, vec!["brightness"]);
    }

    #[test]
    fn remove_effect_is_idempotent() {
        let p = with_one_item();
        let r = remove_effect(&p, "i1", "grayscale").unwrap();
        assert!(r.timeline_items[0].effects.is_empty());
    }

    #[test]
    fn effect_ops_leave_the_input_project_untouched() {
        let p = with_one_item();
        let _ = set_effect(
            &p,
            "i1",
            "brightness",
            &serde_json::json!({ "amount": 0.5 }),
            true,
        )
        .unwrap();
        assert!(
            p.timeline_items[0].effects.is_empty(),
            "ops are pure (undo depends on it)"
        );
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

    // ── gap engine ──────────────────────────────────────────────────────────

    /// A track with three 1 s clips and two 1 s gaps:
    ///   [1000..2000) a  ·gap·  [3000..4000) b  ·gap·  [5000..6000) c
    /// plus a leading gap [0..1000).
    fn gappy() -> Project {
        let mut p = base();
        p.media = vec![media("m1", 10_000)];
        p.timeline_items = vec![
            item("a", "v1", Some("m1"), 1000, 0, 1000),
            item("b", "v1", Some("m1"), 3000, 0, 1000),
            item("c", "v1", Some("m1"), 5000, 0, 1000),
        ];
        p
    }

    fn lock(p: &mut Project, id: &str) {
        p.timeline_items
            .iter_mut()
            .find(|it| it.id == id)
            .unwrap()
            .locked = true;
    }

    fn start_of(p: &Project, id: &str) -> i64 {
        p.timeline_items
            .iter()
            .find(|it| it.id == id)
            .unwrap()
            .timeline_start_ms
    }

    // detect_gaps
    #[test]
    fn detect_gaps_finds_leading_and_interior_gaps() {
        let g = detect_gaps(&gappy(), "v1").unwrap();
        assert_eq!(
            g,
            vec![
                Gap {
                    start_ms: 0,
                    end_ms: 1000,
                    protected: false
                },
                Gap {
                    start_ms: 2000,
                    end_ms: 3000,
                    protected: false
                },
                Gap {
                    start_ms: 4000,
                    end_ms: 5000,
                    protected: false
                },
            ]
        );
    }

    #[test]
    fn detect_gaps_reports_no_trailing_gap() {
        let g = detect_gaps(&gappy(), "v1").unwrap();
        assert!(
            g.iter().all(|x| x.start_ms < 5000),
            "a track just ends — no trailing gap"
        );
    }

    #[test]
    fn detect_gaps_empty_track_has_no_gaps() {
        let p = base();
        assert!(detect_gaps(&p, "v1").unwrap().is_empty());
    }

    #[test]
    fn detect_gaps_gapless_track_has_no_gaps() {
        let mut p = base();
        p.timeline_items = vec![
            item("a", "v1", Some("m1"), 0, 0, 1000),
            item("b", "v1", Some("m1"), 1000, 0, 1000),
        ];
        assert!(detect_gaps(&p, "v1").unwrap().is_empty());
    }

    #[test]
    fn detect_gaps_unknown_track_is_not_found() {
        let err = detect_gaps(&gappy(), "nope").unwrap_err();
        assert_eq!(err.code(), "not_found");
    }

    #[test]
    fn detect_gaps_ignores_other_tracks() {
        let mut p = gappy();
        p.tracks.push(track("v2", TrackKind::Video, 1));
        p.timeline_items
            .push(item("d", "v2", Some("m1"), 0, 0, 9000));
        assert_eq!(detect_gaps(&p, "v1").unwrap().len(), 3);
        assert!(detect_gaps(&p, "v2").unwrap().is_empty());
    }

    #[test]
    fn detect_gaps_overlapping_items_do_not_manufacture_gaps() {
        // Overlap is legal on Overlay tracks; a short clip inside a long one
        // must not look like a gap when the cursor walks past it.
        let mut p = base();
        p.tracks.push(track("ov", TrackKind::Overlay, 1));
        p.timeline_items = vec![
            item("long", "ov", Some("m1"), 0, 0, 5000),
            item("short", "ov", Some("m1"), 1000, 0, 500),
        ];
        assert!(detect_gaps(&p, "ov").unwrap().is_empty());
    }

    #[test]
    fn detect_gaps_marks_the_gap_before_a_locked_clip_protected() {
        let mut p = gappy();
        lock(&mut p, "b");
        let g = detect_gaps(&p, "v1").unwrap();
        assert_eq!(g[1].start_ms, 2000);
        assert!(g[1].protected, "gap in front of locked 'b' is protected");
        assert!(!g[0].protected);
        assert!(!g[2].protected);
    }

    #[test]
    fn gap_duration_is_end_minus_start() {
        let g = detect_gaps(&gappy(), "v1").unwrap();
        assert_eq!(g[0].duration_ms(), 1000);
    }

    // insert_gap_with_ripple
    #[test]
    fn insert_gap_shifts_clips_starting_at_or_after() {
        let r = insert_gap_with_ripple(&gappy(), "v1", 3000, 500).unwrap();
        assert_eq!(start_of(&r, "a"), 1000, "before the point — unmoved");
        assert_eq!(start_of(&r, "b"), 3500);
        assert_eq!(start_of(&r, "c"), 5500);
    }

    #[test]
    fn insert_gap_leaves_a_straddling_clip_in_place() {
        // 3500 falls inside b [3000..4000): b starts before it, so b stays.
        let r = insert_gap_with_ripple(&gappy(), "v1", 3500, 500).unwrap();
        assert_eq!(start_of(&r, "b"), 3000);
        assert_eq!(start_of(&r, "c"), 5500);
    }

    #[test]
    fn insert_gap_at_zero_shifts_the_whole_track() {
        let r = insert_gap_with_ripple(&gappy(), "v1", 0, 250).unwrap();
        assert_eq!(start_of(&r, "a"), 1250);
        assert_eq!(start_of(&r, "b"), 3250);
        assert_eq!(start_of(&r, "c"), 5250);
    }

    #[test]
    fn insert_gap_zero_duration_is_a_no_op() {
        let p = gappy();
        let r = insert_gap_with_ripple(&p, "v1", 3000, 0).unwrap();
        assert_eq!(r.timeline_items, p.timeline_items);
    }

    #[test]
    fn insert_gap_clamps_negative_inputs() {
        let p = gappy();
        // Negative duration clamps to 0 → no-op; negative at clamps to 0.
        let r = insert_gap_with_ripple(&p, "v1", -9999, -5).unwrap();
        assert_eq!(r.timeline_items, p.timeline_items);
        let r = insert_gap_with_ripple(&p, "v1", -9999, 100).unwrap();
        assert_eq!(start_of(&r, "a"), 1100);
    }

    #[test]
    fn insert_gap_past_the_end_moves_nothing() {
        let p = gappy();
        let r = insert_gap_with_ripple(&p, "v1", 99_000, 1000).unwrap();
        assert_eq!(r.timeline_items, p.timeline_items);
    }

    #[test]
    fn insert_gap_ripple_stops_at_a_protected_gap() {
        let mut p = gappy();
        lock(&mut p, "c"); // gap [4000..5000) in front of c is protected
        let r = insert_gap_with_ripple(&p, "v1", 3000, 400).unwrap();
        assert_eq!(start_of(&r, "b"), 3400, "b absorbs the shift");
        assert_eq!(start_of(&r, "c"), 5000, "locked c keeps its timecode");
    }

    #[test]
    fn insert_gap_clamps_the_shift_to_the_protected_headroom() {
        let mut p = gappy();
        lock(&mut p, "c");
        // Headroom between b's end (4000) and c's start (5000) is 1000 ms.
        let r = insert_gap_with_ripple(&p, "v1", 3000, 5_000).unwrap();
        assert_eq!(start_of(&r, "b"), 4000, "b slides only as far as it fits");
        assert_eq!(start_of(&r, "c"), 5000);
    }

    #[test]
    fn insert_gap_with_no_headroom_is_a_no_op() {
        let mut p = base();
        p.timeline_items = vec![
            item("a", "v1", Some("m1"), 0, 0, 1000),
            item("b", "v1", Some("m1"), 1000, 0, 1000),
        ];
        lock(&mut p, "b");
        let before = p.timeline_items.clone();
        let r = insert_gap_with_ripple(&p, "v1", 1000, 500).unwrap();
        assert_eq!(r.timeline_items, before);
    }

    #[test]
    fn insert_gap_at_a_locked_clip_moves_nothing() {
        let mut p = gappy();
        lock(&mut p, "b");
        let before = p.timeline_items.clone();
        let r = insert_gap_with_ripple(&p, "v1", 3000, 500).unwrap();
        assert_eq!(r.timeline_items, before, "b is the barrier itself");
    }

    #[test]
    fn insert_gap_downstream_of_the_barrier_stays_put() {
        let mut p = gappy();
        lock(&mut p, "b");
        let r = insert_gap_with_ripple(&p, "v1", 1000, 300).unwrap();
        assert_eq!(start_of(&r, "a"), 1300);
        assert_eq!(start_of(&r, "b"), 3000);
        assert_eq!(start_of(&r, "c"), 5000, "ripple never jumped the barrier");
    }

    #[test]
    fn insert_gap_unknown_track_is_not_found() {
        let err = insert_gap_with_ripple(&gappy(), "nope", 0, 100).unwrap_err();
        assert_eq!(err.code(), "not_found");
    }

    // remove_gap_with_ripple
    #[test]
    fn remove_gap_closes_the_gap_and_ripples() {
        let r = remove_gap_with_ripple(&gappy(), "v1", 2500).unwrap();
        assert_eq!(start_of(&r, "a"), 1000);
        assert_eq!(start_of(&r, "b"), 2000, "b closed up against a");
        assert_eq!(start_of(&r, "c"), 4000);
        // The gap it targeted is gone; the other two survive.
        let g = detect_gaps(&r, "v1").unwrap();
        assert_eq!(g.len(), 2);
        assert!(!g.iter().any(|x| x.start_ms == 2000));
    }

    #[test]
    fn remove_gap_closes_the_leading_gap() {
        let r = remove_gap_with_ripple(&gappy(), "v1", 0).unwrap();
        assert_eq!(start_of(&r, "a"), 0);
        assert_eq!(start_of(&r, "b"), 2000);
        assert_eq!(start_of(&r, "c"), 4000);
    }

    #[test]
    fn remove_gap_is_inclusive_of_start_exclusive_of_end() {
        // 2000 is the gap's first ms → hits. 3000 is the next clip → no-op.
        let hit = remove_gap_with_ripple(&gappy(), "v1", 2000).unwrap();
        assert_eq!(start_of(&hit, "b"), 2000);
        let p = gappy();
        let miss = remove_gap_with_ripple(&p, "v1", 3000).unwrap();
        assert_eq!(miss.timeline_items, p.timeline_items);
    }

    #[test]
    fn remove_gap_outside_any_gap_is_a_no_op() {
        let p = gappy();
        for at in [1500i64, 3500, 5500, 99_000] {
            let r = remove_gap_with_ripple(&p, "v1", at).unwrap();
            assert_eq!(r.timeline_items, p.timeline_items, "at={at}");
        }
    }

    #[test]
    fn remove_gap_clamps_negative_at() {
        let r = remove_gap_with_ripple(&gappy(), "v1", -500).unwrap();
        assert_eq!(start_of(&r, "a"), 0, "clamped into the leading gap");
    }

    #[test]
    fn remove_gap_refuses_a_protected_gap() {
        let mut p = gappy();
        lock(&mut p, "b");
        let before = p.timeline_items.clone();
        let r = remove_gap_with_ripple(&p, "v1", 2500).unwrap();
        assert_eq!(r.timeline_items, before);
    }

    #[test]
    fn remove_gap_ripple_stops_at_a_downstream_locked_clip() {
        let mut p = gappy();
        lock(&mut p, "c");
        let r = remove_gap_with_ripple(&p, "v1", 2500).unwrap();
        assert_eq!(start_of(&r, "b"), 2000, "b moved");
        assert_eq!(start_of(&r, "c"), 5000, "locked c did not");
    }

    #[test]
    fn remove_gap_on_an_empty_track_is_a_no_op() {
        let p = base();
        let r = remove_gap_with_ripple(&p, "v1", 1000).unwrap();
        assert!(r.timeline_items.is_empty());
    }

    #[test]
    fn remove_gap_unknown_track_is_not_found() {
        let err = remove_gap_with_ripple(&gappy(), "nope", 0).unwrap_err();
        assert_eq!(err.code(), "not_found");
    }

    // pack_track
    #[test]
    fn pack_track_closes_every_gap() {
        let r = pack_track(&gappy(), "v1").unwrap();
        assert_eq!(start_of(&r, "a"), 0);
        assert_eq!(start_of(&r, "b"), 1000);
        assert_eq!(start_of(&r, "c"), 2000);
        assert!(detect_gaps(&r, "v1").unwrap().is_empty());
    }

    #[test]
    fn pack_track_is_idempotent() {
        let once = pack_track(&gappy(), "v1").unwrap();
        let twice = pack_track(&once, "v1").unwrap();
        assert_eq!(once.timeline_items, twice.timeline_items);
    }

    #[test]
    fn pack_track_leaves_a_gapless_track_alone() {
        let mut p = base();
        p.timeline_items = vec![
            item("a", "v1", Some("m1"), 0, 0, 1000),
            item("b", "v1", Some("m1"), 1000, 0, 1000),
        ];
        let r = pack_track(&p, "v1").unwrap();
        assert_eq!(r.timeline_items, p.timeline_items);
    }

    #[test]
    fn pack_track_never_moves_a_clip_to_the_right() {
        let r = pack_track(&gappy(), "v1").unwrap();
        for id in ["a", "b", "c"] {
            assert!(
                start_of(&r, id) <= start_of(&gappy(), id),
                "{id} moved right"
            );
        }
    }

    #[test]
    fn pack_track_anchors_locked_clips_and_preserves_their_gap() {
        let mut p = gappy();
        lock(&mut p, "c");
        let r = pack_track(&p, "v1").unwrap();
        assert_eq!(start_of(&r, "a"), 0);
        assert_eq!(start_of(&r, "b"), 1000);
        assert_eq!(start_of(&r, "c"), 5000, "anchor holds its timecode");
        // The protected gap survives — grown, because the material upstream
        // packed left away from it.
        let g = detect_gaps(&r, "v1").unwrap();
        assert_eq!(g.len(), 1);
        assert_eq!((g[0].start_ms, g[0].end_ms), (2000, 5000));
        assert!(g[0].protected);
    }

    #[test]
    fn pack_track_packs_after_a_locked_anchor_too() {
        let mut p = gappy();
        lock(&mut p, "b");
        let r = pack_track(&p, "v1").unwrap();
        assert_eq!(start_of(&r, "a"), 0);
        assert_eq!(start_of(&r, "b"), 3000, "anchor holds");
        assert_eq!(start_of(&r, "c"), 4000, "c packs against the anchor's end");
    }

    #[test]
    fn pack_track_first_clip_locked_pins_the_leading_gap() {
        let mut p = gappy();
        lock(&mut p, "a");
        let r = pack_track(&p, "v1").unwrap();
        assert_eq!(start_of(&r, "a"), 1000);
        assert_eq!(start_of(&r, "b"), 2000);
        assert_eq!(start_of(&r, "c"), 3000);
        let g = detect_gaps(&r, "v1").unwrap();
        assert_eq!(g.len(), 1);
        assert_eq!((g[0].start_ms, g[0].end_ms), (0, 1000));
    }

    #[test]
    fn pack_track_all_locked_is_a_no_op() {
        let mut p = gappy();
        for id in ["a", "b", "c"] {
            lock(&mut p, id);
        }
        let before = p.timeline_items.clone();
        let r = pack_track(&p, "v1").unwrap();
        assert_eq!(r.timeline_items, before);
    }

    #[test]
    fn pack_track_ignores_other_tracks() {
        let mut p = gappy();
        p.tracks.push(track("v2", TrackKind::Video, 1));
        p.timeline_items
            .push(item("d", "v2", Some("m1"), 7000, 0, 1000));
        let r = pack_track(&p, "v1").unwrap();
        assert_eq!(start_of(&r, "d"), 7000);
    }

    #[test]
    fn pack_track_respects_clip_speed() {
        let mut p = base();
        let mut fast = item("a", "v1", Some("m1"), 1000, 0, 2000);
        fast.speed = 2.0; // 2000 ms of source → 1000 ms on the timeline
        p.timeline_items = vec![fast, item("b", "v1", Some("m1"), 5000, 0, 1000)];
        let r = pack_track(&p, "v1").unwrap();
        assert_eq!(start_of(&r, "a"), 0);
        assert_eq!(start_of(&r, "b"), 1000);
    }

    #[test]
    fn pack_track_empty_track_is_fine() {
        let p = base();
        let r = pack_track(&p, "v1").unwrap();
        assert!(r.timeline_items.is_empty());
    }

    #[test]
    fn pack_track_unknown_track_is_not_found() {
        let err = pack_track(&gappy(), "nope").unwrap_err();
        assert_eq!(err.code(), "not_found");
    }

    // Cross-op invariant: every gap op leaves a valid timeline.
    #[test]
    fn gap_ops_all_validate_the_result() {
        let p = gappy();
        for r in [
            insert_gap_with_ripple(&p, "v1", 3000, 750).unwrap(),
            remove_gap_with_ripple(&p, "v1", 2500).unwrap(),
            pack_track(&p, "v1").unwrap(),
        ] {
            r.validate_timeline().expect("timeline invariants hold");
            r.validate().expect("project invariants hold");
        }
    }

    #[test]
    fn insert_then_remove_the_same_gap_round_trips() {
        let p = gappy();
        let inserted = insert_gap_with_ripple(&p, "v1", 3000, 750).unwrap();
        // The gap in front of b is now 1750 ms; removing it puts b back.
        let restored = remove_gap_with_ripple(&inserted, "v1", 2500).unwrap();
        assert_eq!(start_of(&restored, "b"), 2000);
        assert_eq!(start_of(&restored, "c"), 4000);
    }
}
