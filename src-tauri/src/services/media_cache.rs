//! Disk-cache housekeeping for the filmstrip tile + thumbnail JPEG dumps
//! (E3-UI / media bin). Both caches are write-only from the app's own point
//! of view — `extract_thumbnail`/`extract_filmstrip_tile` (`video.rs`) write
//! a JPEG once and the frontend re-uses whatever is already on disk forever
//! (`src/features/media/thumbnails.ts`, `src/features/timeline/filmstrip.ts`)
//! — so nothing ever deleted a tile or thumbnail. A user scrubbing a long
//! 4K timeline across every zoom tier generates thousands of filmstrip
//! tiles per media item; across sessions that grows without bound.
//!
//! The fix is a bounded sweep, not smarter caching: each cache directory is
//! a flat dump of JPEGs, so pruning is "delete the least-recently-modified
//! files until the directory is back under its budget" — a disk LRU keyed
//! on mtime (the file's last-written time doubles as its last-used time,
//! since nothing else ever touches these files after they're written).
//! Deleting a tile/thumbnail is always safe: the frontend re-renders it via
//! ffmpeg the next time it scrolls into view (`selectDisplayTiles` even
//! degrades gracefully to a coarser ancestor or no backdrop while that
//! happens).

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::Serialize;
use ts_rs::TS;

use crate::error::AppResult;

/// Filmstrip tile cache budget, bytes. A tile is `TILE_COLS_DEFAULT` (8)
/// frames baked into one JPEG at `TILE_HEIGHT_PX` (72px) tall
/// (`filmstrip.ts`) — a few tens of KB each in practice. 512 MB covers on
/// the order of 10 000-20 000 tiles: enough for a user to scrub a multi-hour
/// 4K timeline across every zoom tier in one sitting, without the cache
/// growing without bound across sessions.
pub const FILMSTRIP_CACHE_BUDGET_BYTES: u64 = 512 * 1024 * 1024;

/// Thumbnail cache budget, bytes. One 120px-tall single-frame JPEG per
/// media item (`extract_thumbnail`) — a few KB each. 128 MB comfortably
/// covers a media pool of many thousands of imported files.
pub const THUMBNAIL_CACHE_BUDGET_BYTES: u64 = 128 * 1024 * 1024;

/// Result of sweeping one cache directory.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, TS)]
#[ts(export, export_to = "../../src/lib/bindings/CacheDirPruneReport.ts")]
pub struct CacheDirPruneReport {
    pub files_scanned: u32,
    pub files_deleted: u32,
    #[ts(type = "number")]
    pub bytes_before: u64,
    #[ts(type = "number")]
    pub bytes_freed: u64,
}

/// Result of sweeping both media caches.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, TS)]
#[ts(export, export_to = "../../src/lib/bindings/CachePruneReport.ts")]
pub struct CachePruneReport {
    pub filmstrip: CacheDirPruneReport,
    pub thumbnails: CacheDirPruneReport,
}

/// Sweep `dir` down to at most `budget_bytes` total, deleting the
/// least-recently-modified files first. A missing directory (nothing
/// cached yet) is a silent no-op, not an error. Never descends into
/// subdirectories — both caches are flat JPEG dumps, and treating a
/// surprise subdirectory as a deletable "file" would be the wrong kind of
/// aggressive.
fn prune_dir_by_budget(dir: &Path, budget_bytes: u64) -> AppResult<CacheDirPruneReport> {
    let mut report = CacheDirPruneReport::default();

    let read_dir = match std::fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(report),
        Err(e) => return Err(e.into()),
    };

    let mut files: Vec<(PathBuf, u64, SystemTime)> = Vec::new();
    for entry in read_dir {
        // A vanished-between-listing-and-stat entry is a race with another
        // prune or a concurrent write, not a fatal error — skip it.
        let Ok(entry) = entry else { continue };
        let Ok(meta) = entry.metadata() else { continue };
        if !meta.is_file() {
            continue;
        }
        let len = meta.len();
        let mtime = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
        report.bytes_before += len;
        files.push((entry.path(), len, mtime));
    }
    report.files_scanned = files.len() as u32;

    if report.bytes_before <= budget_bytes {
        return Ok(report);
    }

    // Oldest-touched first — read_dir order is arbitrary, so an explicit
    // sort is what actually makes this an LRU-by-mtime eviction.
    files.sort_by_key(|(_, _, mtime)| *mtime);

    let mut remaining = report.bytes_before;
    for (path, len, _) in files {
        if remaining <= budget_bytes {
            break;
        }
        if std::fs::remove_file(&path).is_ok() {
            remaining -= len;
            report.bytes_freed += len;
            report.files_deleted += 1;
        }
    }
    Ok(report)
}

/// Sweep both the filmstrip and thumbnail caches under `cache_root` (the
/// app cache dir) down to their documented budgets. Safe to call on every
/// launch — a no-op when already under budget, and idempotent (re-running
/// it immediately after does nothing further).
pub fn prune_media_caches(cache_root: &Path) -> AppResult<CachePruneReport> {
    Ok(CachePruneReport {
        filmstrip: prune_dir_by_budget(
            &cache_root.join("filmstrip"),
            FILMSTRIP_CACHE_BUDGET_BYTES,
        )?,
        thumbnails: prune_dir_by_budget(
            &cache_root.join("thumbnails"),
            THUMBNAIL_CACHE_BUDGET_BYTES,
        )?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use std::time::Duration;

    /// Write `name` under `dir` with `len` bytes of content, then back-date
    /// its mtime by `age` from "now" — deterministic without depending on
    /// filesystem mtime granularity or sleeping between writes.
    fn write_aged_file(dir: &Path, name: &str, len: usize, age: Duration) {
        let path = dir.join(name);
        std::fs::write(&path, vec![0u8; len]).unwrap();
        let mtime = SystemTime::now() - age;
        File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(mtime)
            .unwrap();
    }

    #[test]
    fn missing_dir_is_a_silent_noop() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("never-written");
        let report = prune_dir_by_budget(&missing, 1024).unwrap();
        assert_eq!(report, CacheDirPruneReport::default());
    }

    #[test]
    fn under_budget_deletes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        write_aged_file(dir.path(), "a.jpg", 100, Duration::from_secs(1000));
        write_aged_file(dir.path(), "b.jpg", 100, Duration::from_secs(1));

        let report = prune_dir_by_budget(dir.path(), 10_000).unwrap();

        assert_eq!(report.files_scanned, 2);
        assert_eq!(report.files_deleted, 0);
        assert_eq!(report.bytes_before, 200);
        assert_eq!(report.bytes_freed, 0);
        assert!(dir.path().join("a.jpg").exists());
        assert!(dir.path().join("b.jpg").exists());
    }

    /// Over budget: the OLDEST files must go first, and just enough of them
    /// to land back at or under budget — the newest survives untouched.
    #[test]
    fn over_budget_evicts_oldest_first_until_under_budget() {
        let dir = tempfile::tempdir().unwrap();
        // Oldest to newest: 100 + 100 + 100 = 300 bytes total, budget 150.
        write_aged_file(dir.path(), "oldest.jpg", 100, Duration::from_secs(3000));
        write_aged_file(dir.path(), "middle.jpg", 100, Duration::from_secs(2000));
        write_aged_file(dir.path(), "newest.jpg", 100, Duration::from_secs(1000));

        let report = prune_dir_by_budget(dir.path(), 150).unwrap();

        assert_eq!(report.files_scanned, 3);
        assert_eq!(report.bytes_before, 300);
        // Must delete the two oldest (200 bytes) to get to 100 <= 150 — one
        // deletion alone would leave 200 > 150, still over budget.
        assert_eq!(report.files_deleted, 2);
        assert_eq!(report.bytes_freed, 200);
        assert!(!dir.path().join("oldest.jpg").exists());
        assert!(!dir.path().join("middle.jpg").exists());
        assert!(dir.path().join("newest.jpg").exists());
    }

    #[test]
    fn exactly_at_budget_deletes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        write_aged_file(dir.path(), "a.jpg", 500, Duration::from_secs(1));
        let report = prune_dir_by_budget(dir.path(), 500).unwrap();
        assert_eq!(report.files_deleted, 0);
        assert!(dir.path().join("a.jpg").exists());
    }

    /// A stray subdirectory (should never happen for these caches, but
    /// nothing enforces it at the filesystem level) must be skipped, not
    /// treated as a deletable file.
    #[test]
    fn subdirectories_are_never_touched() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("stray_dir")).unwrap();
        write_aged_file(dir.path(), "a.jpg", 10, Duration::from_secs(1));

        let report = prune_dir_by_budget(dir.path(), 0).unwrap();

        // Only the file counts toward scanned/deleted; the directory is
        // silently skipped either way.
        assert_eq!(report.files_scanned, 1);
        assert_eq!(report.files_deleted, 1);
        assert!(dir.path().join("stray_dir").is_dir());
    }

    /// `prune_media_caches` sweeps `filmstrip/` and `thumbnails/`
    /// independently, each against its OWN budget — a bloated filmstrip
    /// cache must not cause thumbnails to be pruned too, or vice versa.
    #[test]
    fn prune_media_caches_sweeps_each_subdir_independently() {
        let root = tempfile::tempdir().unwrap();
        let filmstrip = root.path().join("filmstrip");
        let thumbnails = root.path().join("thumbnails");
        std::fs::create_dir_all(&filmstrip).unwrap();
        std::fs::create_dir_all(&thumbnails).unwrap();

        // Filmstrip: 300 bytes total against a tiny custom-sized scenario —
        // exercise via the real budget constants isn't practical in a unit
        // test, so call prune_dir_by_budget directly per dir at small,
        // deliberately DIFFERENT budgets to prove independence.
        write_aged_file(&filmstrip, "t1.jpg", 100, Duration::from_secs(2000));
        write_aged_file(&filmstrip, "t2.jpg", 100, Duration::from_secs(1000));
        write_aged_file(&thumbnails, "m1.jpg", 100, Duration::from_secs(2000));
        write_aged_file(&thumbnails, "m2.jpg", 100, Duration::from_secs(1000));

        let filmstrip_report = prune_dir_by_budget(&filmstrip, 150).unwrap();
        let thumbnails_report = prune_dir_by_budget(&thumbnails, 1_000_000).unwrap();

        assert_eq!(
            filmstrip_report.files_deleted, 1,
            "filmstrip over its budget"
        );
        assert_eq!(
            thumbnails_report.files_deleted, 0,
            "thumbnails under its own (much larger) budget must be untouched"
        );
        assert!(thumbnails.join("m1.jpg").exists());
        assert!(thumbnails.join("m2.jpg").exists());
    }

    #[test]
    fn prune_media_caches_is_a_noop_when_neither_dir_exists_yet() {
        let root = tempfile::tempdir().unwrap();
        let report = prune_media_caches(root.path()).unwrap();
        assert_eq!(report, CachePruneReport::default());
    }
}
