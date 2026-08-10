//! Fixed-grid tile addressing for timeline caches (E3).
//!
//! The problem this solves: naively, a filmstrip (or a waveform) is rendered
//! for "whatever range is currently visible at the current zoom". Every scroll
//! and every zoom step then invalidates the whole cache, because the ranges
//! never line up twice.
//!
//! Instead we address cached artefacts on an **absolute grid per zoom tier**:
//!
//!   * a tier `t` has a fixed tile span `TILE_BASE_SPAN_MS >> t`,
//!   * tile `i` at tier `t` always covers `[i*span, (i+1)*span)` — anchored at
//!     timeline zero, never at the viewport,
//!   * spans halve exactly between tiers, so tile `i` at tier `t` is the union
//!     of tiles `2i` and `2i+1` at tier `t+1`.
//!
//! Two consequences the UI gets for free:
//!   1. **Scrolling reuses tiles** — the grid does not move with the viewport,
//!      so panning only asks for the tiles that entered the view.
//!   2. **Zooming reuses the centre** — the boundaries nest, so a zoom step
//!      keeps every already-rendered tile addressable (a tier-`t` tile stays a
//!      valid, if coarse, stand-in for its two children while they render).
//!
//! This module is deliberately media-agnostic: `services::video` uses it for
//! filmstrip tiles today, and the waveform cache can adopt the exact same
//! scheme later without a second grid to reason about. See docs/DECISIONS.md
//! ADR-012.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Span of one tile at the coarsest tier (tier 0), in milliseconds.
///
/// 64 s is chosen so every tier down to `TILE_MAX_TIER` halves *exactly* —
/// no integer-division rounding, which is what makes the nesting property
/// (`parent = child(2i) ∪ child(2i+1)`) hold at every tier.
pub const TILE_BASE_SPAN_MS: i64 = 64_000;

/// Finest addressable tier: `64_000 >> 8` = 250 ms per tile.
pub const TILE_MAX_TIER: u32 = 8;

/// Frames rendered side by side into one filmstrip tile.
pub const TILE_COLS_DEFAULT: u32 = 8;

/// Height in pixels of a filmstrip tile (width follows the source aspect).
pub const TILE_HEIGHT_PX: u32 = 72;

/// Clamp a caller-supplied tier into `0..=TILE_MAX_TIER` — clamp, don't
/// reject, same discipline as the timeline ops.
pub fn clamp_tier(zoom_tier: u32) -> u32 {
    zoom_tier.min(TILE_MAX_TIER)
}

/// How much timeline one tile covers at `zoom_tier`, in milliseconds.
/// Monotonically halves as the tier rises (tier 0 = coarsest / most zoomed out).
pub fn tile_span_ms(zoom_tier: u32) -> i64 {
    TILE_BASE_SPAN_MS >> clamp_tier(zoom_tier)
}

/// Stable cache key for one tile. Opaque to callers — treat it as an id, not
/// as parseable structure. Safe as a filename component (no separators).
pub fn tile_key(zoom_tier: u32, index: i64) -> String {
    format!("z{}i{}", clamp_tier(zoom_tier), index.max(0))
}

/// Cache filename for a tile belonging to one media item. `media_key` is
/// expected to be the media's content hash (path-stable identity), so a moved
/// file keeps its rendered tiles.
pub fn tile_file_name(media_key: &str, zoom_tier: u32, index: i64) -> String {
    format!("{}_{}.jpg", media_key, tile_key(zoom_tier, index))
}

/// Index of the tile containing `ms` at `zoom_tier`. Negative times clamp to
/// tile 0 — the timeline has no negative side.
pub fn tile_index_at(ms: i64, zoom_tier: u32) -> i64 {
    ms.max(0) / tile_span_ms(zoom_tier)
}

/// The `[start, end)` millisecond range tile `index` covers at `zoom_tier`.
pub fn tile_range_ms(zoom_tier: u32, index: i64) -> (i64, i64) {
    let span = tile_span_ms(zoom_tier);
    let start = index.max(0) * span;
    (start, start + span)
}

/// One addressed tile — everything the frontend needs to look it up in its
/// cache and, on a miss, ask the backend to render it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../src/lib/bindings/TileRef.ts")]
pub struct TileRef {
    pub tier: u32,
    #[ts(type = "number")]
    pub index: i64,
    #[ts(type = "number")]
    pub start_ms: i64,
    #[ts(type = "number")]
    pub end_ms: i64,
    pub key: String,
}

/// Every tile at `zoom_tier` that overlaps `[start_ms, end_ms)`, in order.
///
/// Clamps rather than rejects: a negative start snaps to 0, and an end at or
/// before the start still yields the single tile containing the start (asking
/// for an empty range means "the tile I'm on").
pub fn tiles_covering(start_ms: i64, end_ms: i64, zoom_tier: u32) -> Vec<TileRef> {
    let tier = clamp_tier(zoom_tier);
    let start = start_ms.max(0);
    let end = end_ms.max(start + 1);
    let first = tile_index_at(start, tier);
    // `end` is exclusive: a range ending exactly on a boundary must not pull
    // in the next tile.
    let last = tile_index_at(end - 1, tier);
    (first..=last)
        .map(|index| {
            let (s, e) = tile_range_ms(tier, index);
            TileRef {
                tier,
                index,
                start_ms: s,
                end_ms: e,
                key: tile_key(tier, index),
            }
        })
        .collect()
}

/// The two tiles at `zoom_tier + 1` that together cover tile `index` at
/// `zoom_tier`. Returns `None` at `TILE_MAX_TIER` (no finer grid exists).
pub fn child_tiles(zoom_tier: u32, index: i64) -> Option<(i64, i64)> {
    if clamp_tier(zoom_tier) >= TILE_MAX_TIER {
        return None;
    }
    let i = index.max(0);
    Some((i * 2, i * 2 + 1))
}

/// The tile at `zoom_tier - 1` that contains tile `index` at `zoom_tier` —
/// the coarse stand-in to show while a finer tile is still rendering.
/// Returns `None` at tier 0.
pub fn parent_tile(zoom_tier: u32, index: i64) -> Option<i64> {
    if clamp_tier(zoom_tier) == 0 {
        return None;
    }
    Some(index.max(0) / 2)
}

// ────────────────────────────────────────────────────────────────────────────
// Tests
// ────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── tier / span math ────────────────────────────────────────────────────
    #[test]
    fn tier_zero_is_the_base_span() {
        assert_eq!(tile_span_ms(0), TILE_BASE_SPAN_MS);
    }

    #[test]
    fn spans_halve_exactly_every_tier() {
        for t in 0..TILE_MAX_TIER {
            assert_eq!(
                tile_span_ms(t + 1) * 2,
                tile_span_ms(t),
                "tier {} must be exactly half of tier {}",
                t + 1,
                t
            );
        }
    }

    #[test]
    fn finest_tier_is_250ms() {
        assert_eq!(tile_span_ms(TILE_MAX_TIER), 250);
    }

    #[test]
    fn tier_clamps_instead_of_panicking() {
        assert_eq!(clamp_tier(999), TILE_MAX_TIER);
        assert_eq!(tile_span_ms(999), tile_span_ms(TILE_MAX_TIER));
    }

    // ── indexing ────────────────────────────────────────────────────────────
    #[test]
    fn index_is_floor_of_time_over_span() {
        assert_eq!(tile_index_at(0, 0), 0);
        assert_eq!(tile_index_at(63_999, 0), 0);
        assert_eq!(tile_index_at(64_000, 0), 1);
        assert_eq!(tile_index_at(64_001, 0), 1);
    }

    #[test]
    fn negative_time_clamps_to_tile_zero() {
        assert_eq!(tile_index_at(-5_000, 4), 0);
    }

    #[test]
    fn tile_range_round_trips_with_index() {
        for tier in 0..=TILE_MAX_TIER {
            for index in [0i64, 1, 7, 4096] {
                let (s, e) = tile_range_ms(tier, index);
                assert_eq!(tile_index_at(s, tier), index);
                assert_eq!(tile_index_at(e - 1, tier), index);
                assert_eq!(e - s, tile_span_ms(tier));
            }
        }
    }

    // ── keys ────────────────────────────────────────────────────────────────
    #[test]
    fn keys_are_stable_and_distinct() {
        assert_eq!(tile_key(3, 12), "z3i12");
        assert_eq!(tile_key(3, 12), tile_key(3, 12));
        assert_ne!(tile_key(3, 12), tile_key(4, 12));
        assert_ne!(tile_key(3, 12), tile_key(3, 13));
    }

    #[test]
    fn key_clamps_tier_and_index() {
        assert_eq!(tile_key(99, -4), tile_key(TILE_MAX_TIER, 0));
    }

    #[test]
    fn file_name_is_media_scoped() {
        assert_eq!(tile_file_name("abc123", 2, 5), "abc123_z2i5.jpg");
        assert_ne!(tile_file_name("abc123", 2, 5), tile_file_name("def456", 2, 5));
    }

    // ── covering ────────────────────────────────────────────────────────────
    #[test]
    fn covering_returns_tiles_in_order_and_spans_the_range() {
        let tiles = tiles_covering(0, 200_000, 0);
        assert_eq!(tiles.len(), 4); // 0..64k, 64k..128k, 128k..192k, 192k..256k
        assert_eq!(tiles[0].index, 0);
        assert_eq!(tiles[3].index, 3);
        assert!(tiles[0].start_ms <= 0);
        assert!(tiles[3].end_ms >= 200_000);
        for w in tiles.windows(2) {
            assert_eq!(w[0].end_ms, w[1].start_ms, "tiles must abut, no holes");
        }
    }

    #[test]
    fn covering_end_on_a_boundary_does_not_pull_the_next_tile() {
        let tiles = tiles_covering(0, 64_000, 0);
        assert_eq!(tiles.len(), 1);
        assert_eq!(tiles[0].index, 0);
    }

    #[test]
    fn covering_empty_range_yields_the_containing_tile() {
        let tiles = tiles_covering(70_000, 70_000, 0);
        assert_eq!(tiles.len(), 1);
        assert_eq!(tiles[0].index, 1);
    }

    #[test]
    fn covering_clamps_negative_start() {
        let tiles = tiles_covering(-10_000, 1_000, 0);
        assert_eq!(tiles.len(), 1);
        assert_eq!(tiles[0].start_ms, 0);
    }

    // ── the reuse properties this whole module exists for ───────────────────
    #[test]
    fn scrolling_reuses_tiles_because_the_grid_is_absolute() {
        // Two overlapping viewports at the same zoom must share tile keys.
        let a = tiles_covering(10_000, 90_000, 0);
        let b = tiles_covering(50_000, 130_000, 0);
        let keys_a: Vec<&str> = a.iter().map(|t| t.key.as_str()).collect();
        let shared = b.iter().filter(|t| keys_a.contains(&t.key.as_str())).count();
        assert!(shared >= 2, "panning must reuse tiles, got {shared}");
    }

    #[test]
    fn tiles_nest_exactly_across_a_zoom_step() {
        for tier in 0..TILE_MAX_TIER {
            for index in [0i64, 1, 9, 1000] {
                let (ps, pe) = tile_range_ms(tier, index);
                let (a, b) = child_tiles(tier, index).unwrap();
                let (as_, ae) = tile_range_ms(tier + 1, a);
                let (bs, be) = tile_range_ms(tier + 1, b);
                assert_eq!(as_, ps, "first child starts where the parent starts");
                assert_eq!(ae, bs, "children abut");
                assert_eq!(be, pe, "second child ends where the parent ends");
            }
        }
    }

    #[test]
    fn parent_and_child_are_inverse() {
        for tier in 1..=TILE_MAX_TIER {
            for index in [0i64, 1, 2, 3, 77] {
                let p = parent_tile(tier, index).unwrap();
                let (a, b) = child_tiles(tier - 1, p).unwrap();
                assert!(index == a || index == b, "{index} must be a child of {p}");
            }
        }
    }

    #[test]
    fn no_parent_at_tier_zero_no_children_at_max_tier() {
        assert_eq!(parent_tile(0, 5), None);
        assert_eq!(child_tiles(TILE_MAX_TIER, 5), None);
    }

    #[test]
    fn zooming_in_keeps_the_centre_addressable() {
        // A tile that covers the playhead at tier t must have children that
        // still cover the playhead at tier t+1 — that is what lets the UI show
        // the coarse tile while the fine ones render.
        let playhead = 137_500;
        for tier in 0..TILE_MAX_TIER {
            let coarse = tile_index_at(playhead, tier);
            let fine = tile_index_at(playhead, tier + 1);
            assert_eq!(parent_tile(tier + 1, fine), Some(coarse));
        }
    }
}
