//! `.sundayedit` project file format — Phase 1.1.
//!
//! A `.sundayedit` file is a SQLite database. We chose SQLite over a flat
//! JSON file for three reasons:
//!   1. Atomic, crash-safe writes (WAL) — a power-cut mid-save can't
//!      corrupt a project.
//!   2. A `meta` table carries a schema version, so v2 can migrate v1
//!      files forward.
//!   3. The `caption` table is normalized (one row per caption), leaving
//!      room to query/stream captions for very large projects without
//!      parsing a multi-MB JSON blob. (For v1 the renderer still loads
//!      the whole project into memory; the structure is forward-looking.)
//!
//! Scalar Project fields + style/speakers/glossary live in a single
//! `project` row (the collections as JSON — they're small). Captions get
//! their own table; each caption's `words` are JSON (words are an
//! implementation detail of a caption, always loaded together).

use std::path::Path;

use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::{Row, SqlitePool};
use std::str::FromStr;

use crate::error::{AppError, AppResult};
use crate::model::{
    Caption, Clip, ExportConfig, GlossaryTerm, MediaItem, Project, ProjectMeta, Speaker, Style,
    TimelineItem, Track, Word,
};

const SCHEMA_VERSION: i64 = 4;

/// Open (creating if missing) a `.sundayedit` SQLite file and ensure schema.
async fn open_pool(path: &Path) -> AppResult<SqlitePool> {
    let url = format!("sqlite:{}", path.to_string_lossy());
    let opts = SqliteConnectOptions::from_str(&url)?
        .create_if_missing(true)
        .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(opts)
        .await?;
    ensure_schema(&pool).await?;
    Ok(pool)
}

async fn ensure_schema(pool: &SqlitePool) -> AppResult<()> {
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS meta (
            key TEXT PRIMARY KEY,
            value TEXT NOT NULL
        );
        "#,
    )
    .execute(pool)
    .await?;

    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS project (
            id                  TEXT PRIMARY KEY,
            name                TEXT NOT NULL,
            video_path          TEXT NOT NULL,
            video_content_hash  TEXT NOT NULL,
            video_duration_ms   INTEGER NOT NULL,
            video_width         INTEGER NOT NULL,
            video_height        INTEGER NOT NULL,
            video_fps           REAL NOT NULL,
            audio_wav_path      TEXT,
            language            TEXT NOT NULL,
            default_style_json  TEXT NOT NULL,
            context_description TEXT,
            speakers_json       TEXT NOT NULL,
            glossary_json       TEXT NOT NULL,
            clips_json          TEXT NOT NULL DEFAULT '[]',
            talk_summary        TEXT,
            export_config_json  TEXT,
            project_meta_json   TEXT,
            created_at          INTEGER NOT NULL,
            updated_at          INTEGER NOT NULL
        );
        "#,
    )
    .execute(pool)
    .await?;

    // Schema v2 migration: bring v1 project tables (no clip columns) forward.
    // Idempotent — on a fresh v2 table these ALTERs fail with "duplicate
    // column" and are intentionally ignored; on a v1 table they add the
    // columns (existing rows get the DEFAULT).
    let _ = sqlx::query("ALTER TABLE project ADD COLUMN clips_json TEXT NOT NULL DEFAULT '[]'")
        .execute(pool)
        .await;
    let _ = sqlx::query("ALTER TABLE project ADD COLUMN talk_summary TEXT")
        .execute(pool)
        .await;

    // Schema v3 migration: export config + project metadata columns.
    let _ = sqlx::query("ALTER TABLE project ADD COLUMN export_config_json TEXT")
        .execute(pool)
        .await;
    let _ = sqlx::query("ALTER TABLE project ADD COLUMN project_meta_json TEXT")
        .execute(pool)
        .await;

    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS caption (
            id             TEXT PRIMARY KEY,
            position       INTEGER NOT NULL,
            start_ms       INTEGER NOT NULL,
            end_ms         INTEGER NOT NULL,
            words_json     TEXT NOT NULL,
            speaker_id     TEXT,
            style_id       TEXT,
            notes          TEXT,
            ai_generated   INTEGER NOT NULL,
            last_edited_at INTEGER NOT NULL
        );
        "#,
    )
    .execute(pool)
    .await?;

    sqlx::query("CREATE INDEX IF NOT EXISTS caption_pos_idx ON caption(position)")
        .execute(pool)
        .await?;

    // Schema v4: NLE multi-track tables + caption.track_id.
    // Nested structs are stored as JSON TEXT columns, exactly like caption.words.
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS media_item (
            id                TEXT PRIMARY KEY,
            position          INTEGER NOT NULL,
            path              TEXT NOT NULL,
            content_hash      TEXT NOT NULL,
            kind              TEXT NOT NULL,
            duration_ms       INTEGER NOT NULL,
            width             INTEGER NOT NULL,
            height            INTEGER NOT NULL,
            fps               REAL NOT NULL,
            has_audio         INTEGER NOT NULL,
            audio_wav_path    TEXT,
            original_filename TEXT NOT NULL,
            added_at          INTEGER NOT NULL
        );
        "#,
    )
    .execute(pool)
    .await?;

    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS track (
            id       TEXT PRIMARY KEY,
            position INTEGER NOT NULL,
            kind     TEXT NOT NULL,
            name     TEXT NOT NULL,
            track_index INTEGER NOT NULL,
            enabled  INTEGER NOT NULL,
            locked   INTEGER NOT NULL,
            muted    INTEGER NOT NULL,
            solo     INTEGER NOT NULL
        );
        "#,
    )
    .execute(pool)
    .await?;

    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS timeline_item (
            id                TEXT PRIMARY KEY,
            position          INTEGER NOT NULL,
            track_id          TEXT NOT NULL,
            kind              TEXT NOT NULL,
            source_media_id   TEXT,
            in_ms             INTEGER NOT NULL,
            out_ms            INTEGER NOT NULL,
            timeline_start_ms INTEGER NOT NULL,
            speed             REAL NOT NULL,
            transform_json    TEXT NOT NULL,
            effects_json      TEXT NOT NULL,
            transition_json   TEXT,
            text_json         TEXT,
            enabled           INTEGER NOT NULL,
            locked            INTEGER NOT NULL
        );
        "#,
    )
    .execute(pool)
    .await?;

    // Idempotent — on a fresh v4 caption table this fails with "duplicate
    // column" and is intentionally ignored; on a v<=3 table it adds the column.
    let _ = sqlx::query("ALTER TABLE caption ADD COLUMN track_id TEXT")
        .execute(pool)
        .await;

    sqlx::query("INSERT OR IGNORE INTO meta (key, value) VALUES ('schema_version', ?1)")
        .bind(SCHEMA_VERSION.to_string())
        .execute(pool)
        .await?;

    Ok(())
}

/// Save a project to `path`, replacing any existing content. Atomic at
/// the row level via a transaction.
pub async fn save(project: &Project, path: &Path) -> AppResult<()> {
    let pool = open_pool(path).await?;
    let mut tx = pool.begin().await?;

    // Replace project row
    sqlx::query("DELETE FROM project").execute(&mut *tx).await?;
    sqlx::query(
        r#"
        INSERT INTO project (id, name, video_path, video_content_hash,
            video_duration_ms, video_width, video_height, video_fps,
            audio_wav_path, language, default_style_json, context_description,
            speakers_json, glossary_json, created_at, updated_at,
            clips_json, talk_summary, export_config_json, project_meta_json)
        VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20)
        "#,
    )
    .bind(&project.id)
    .bind(&project.name)
    .bind(&project.video_path)
    .bind(&project.video_content_hash)
    .bind(project.video_duration_ms)
    .bind(project.video_width)
    .bind(project.video_height)
    .bind(project.video_fps)
    .bind(&project.audio_wav_path)
    .bind(&project.language)
    .bind(serde_json::to_string(&project.default_style)?)
    .bind(&project.context_description)
    .bind(serde_json::to_string(&project.speakers)?)
    .bind(serde_json::to_string(&project.glossary)?)
    .bind(project.created_at)
    .bind(project.updated_at)
    .bind(serde_json::to_string(&project.clips)?)
    .bind(&project.talk_summary)
    .bind(serde_json::to_string(&project.export_config)?)
    .bind(serde_json::to_string(&project.project_meta)?)
    .execute(&mut *tx)
    .await?;

    // Replace captions
    sqlx::query("DELETE FROM caption").execute(&mut *tx).await?;
    for (pos, c) in project.captions.iter().enumerate() {
        sqlx::query(
            r#"
            INSERT INTO caption (id, position, start_ms, end_ms, words_json,
                speaker_id, style_id, notes, ai_generated, last_edited_at, track_id)
            VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)
            "#,
        )
        .bind(&c.id)
        .bind(pos as i64)
        .bind(c.start_ms)
        .bind(c.end_ms)
        .bind(serde_json::to_string(&c.words)?)
        .bind(&c.speaker_id)
        .bind(&c.style_id)
        .bind(&c.notes)
        .bind(if c.ai_generated { 1 } else { 0 })
        .bind(c.last_edited_at)
        .bind(&c.track_id)
        .execute(&mut *tx)
        .await?;
    }

    // Replace media items
    sqlx::query("DELETE FROM media_item")
        .execute(&mut *tx)
        .await?;
    for (pos, m) in project.media.iter().enumerate() {
        sqlx::query(
            r#"
            INSERT INTO media_item (id, position, path, content_hash, kind,
                duration_ms, width, height, fps, has_audio, audio_wav_path,
                original_filename, added_at)
            VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)
            "#,
        )
        .bind(&m.id)
        .bind(pos as i64)
        .bind(&m.path)
        .bind(&m.content_hash)
        .bind(serde_json::to_string(&m.kind)?)
        .bind(m.duration_ms)
        .bind(m.width)
        .bind(m.height)
        .bind(m.fps)
        .bind(if m.has_audio { 1 } else { 0 })
        .bind(&m.audio_wav_path)
        .bind(&m.original_filename)
        .bind(m.added_at)
        .execute(&mut *tx)
        .await?;
    }

    // Replace tracks. The marker records that the track table's contents —
    // INCLUDING an intentionally empty one — were written by a schema-v4
    // save, so `load` can tell "v4 file whose tracks were all deleted" apart
    // from "v<=3 file that never had tracks" and only backfills the latter.
    sqlx::query("INSERT OR REPLACE INTO meta (key, value) VALUES ('tracks_persisted', '1')")
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM track").execute(&mut *tx).await?;
    for (pos, t) in project.tracks.iter().enumerate() {
        sqlx::query(
            r#"
            INSERT INTO track (id, position, kind, name, track_index,
                enabled, locked, muted, solo)
            VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)
            "#,
        )
        .bind(&t.id)
        .bind(pos as i64)
        .bind(serde_json::to_string(&t.kind)?)
        .bind(&t.name)
        .bind(t.index)
        .bind(if t.enabled { 1 } else { 0 })
        .bind(if t.locked { 1 } else { 0 })
        .bind(if t.muted { 1 } else { 0 })
        .bind(if t.solo { 1 } else { 0 })
        .execute(&mut *tx)
        .await?;
    }

    // Replace timeline items
    sqlx::query("DELETE FROM timeline_item")
        .execute(&mut *tx)
        .await?;
    for (pos, it) in project.timeline_items.iter().enumerate() {
        sqlx::query(
            r#"
            INSERT INTO timeline_item (id, position, track_id, kind,
                source_media_id, in_ms, out_ms, timeline_start_ms, speed,
                transform_json, effects_json, transition_json, text_json,
                enabled, locked)
            VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15)
            "#,
        )
        .bind(&it.id)
        .bind(pos as i64)
        .bind(&it.track_id)
        .bind(serde_json::to_string(&it.kind)?)
        .bind(&it.source_media_id)
        .bind(it.in_ms)
        .bind(it.out_ms)
        .bind(it.timeline_start_ms)
        .bind(it.speed)
        .bind(serde_json::to_string(&it.transform)?)
        .bind(serde_json::to_string(&it.effects)?)
        .bind(
            it.transition_in
                .as_ref()
                .map(serde_json::to_string)
                .transpose()?,
        )
        .bind(it.text.as_ref().map(serde_json::to_string).transpose()?)
        .bind(if it.enabled { 1 } else { 0 })
        .bind(if it.locked { 1 } else { 0 })
        .execute(&mut *tx)
        .await?;
    }

    tx.commit().await?;
    pool.close().await;
    Ok(())
}

/// Load a project from `path`.
pub async fn load(path: &Path) -> AppResult<Project> {
    if !path.exists() {
        return Err(AppError::NotFound {
            entity: "project_file",
            id: path.to_string_lossy().to_string(),
        });
    }
    let pool = open_pool(path).await?;

    let row = sqlx::query("SELECT * FROM project LIMIT 1")
        .fetch_optional(&pool)
        .await?
        .ok_or_else(|| AppError::Validation("project file has no project row".to_string()))?;

    let default_style: Style =
        serde_json::from_str(row.get::<String, _>("default_style_json").as_str())?;
    let speakers: Vec<Speaker> =
        serde_json::from_str(row.get::<String, _>("speakers_json").as_str())?;
    let glossary: Vec<GlossaryTerm> =
        serde_json::from_str(row.get::<String, _>("glossary_json").as_str())?;
    let clips: Vec<Clip> = serde_json::from_str(row.get::<String, _>("clips_json").as_str())?;
    let talk_summary: Option<String> = row.get("talk_summary");
    let export_config: ExportConfig = row
        .get::<Option<String>, _>("export_config_json")
        .as_deref()
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or_default();
    let project_meta: ProjectMeta = row
        .get::<Option<String>, _>("project_meta_json")
        .as_deref()
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or_default();

    let caption_rows = sqlx::query("SELECT * FROM caption ORDER BY position")
        .fetch_all(&pool)
        .await?;

    let mut captions = Vec::with_capacity(caption_rows.len());
    for r in caption_rows {
        let words: Vec<Word> = serde_json::from_str(r.get::<String, _>("words_json").as_str())?;
        captions.push(Caption {
            id: r.get("id"),
            start_ms: r.get("start_ms"),
            end_ms: r.get("end_ms"),
            words,
            speaker_id: r.get("speaker_id"),
            style_id: r.get("style_id"),
            notes: r.get("notes"),
            ai_generated: r.get::<i64, _>("ai_generated") != 0,
            last_edited_at: r.get("last_edited_at"),
            track_id: r.try_get("track_id").ok().flatten(),
        });
    }

    // NLE multi-track tables.
    let media_rows = sqlx::query("SELECT * FROM media_item ORDER BY position")
        .fetch_all(&pool)
        .await?;
    let mut media = Vec::with_capacity(media_rows.len());
    for r in media_rows {
        media.push(MediaItem {
            id: r.get("id"),
            path: r.get("path"),
            content_hash: r.get("content_hash"),
            kind: serde_json::from_str(r.get::<String, _>("kind").as_str())?,
            duration_ms: r.get("duration_ms"),
            width: r.get::<i64, _>("width") as i32,
            height: r.get::<i64, _>("height") as i32,
            fps: r.get::<f64, _>("fps") as f32,
            has_audio: r.get::<i64, _>("has_audio") != 0,
            audio_wav_path: r.get("audio_wav_path"),
            original_filename: r.get("original_filename"),
            added_at: r.get("added_at"),
        });
    }

    let track_rows = sqlx::query("SELECT * FROM track ORDER BY position")
        .fetch_all(&pool)
        .await?;
    let mut tracks = Vec::with_capacity(track_rows.len());
    for r in track_rows {
        tracks.push(Track {
            id: r.get("id"),
            kind: serde_json::from_str(r.get::<String, _>("kind").as_str())?,
            name: r.get("name"),
            index: r.get::<i64, _>("track_index") as i32,
            enabled: r.get::<i64, _>("enabled") != 0,
            locked: r.get::<i64, _>("locked") != 0,
            muted: r.get::<i64, _>("muted") != 0,
            solo: r.get::<i64, _>("solo") != 0,
        });
    }

    let item_rows = sqlx::query("SELECT * FROM timeline_item ORDER BY position")
        .fetch_all(&pool)
        .await?;
    let mut timeline_items = Vec::with_capacity(item_rows.len());
    for r in item_rows {
        let transition_in = r
            .get::<Option<String>, _>("transition_json")
            .as_deref()
            .map(serde_json::from_str)
            .transpose()?;
        let text = r
            .get::<Option<String>, _>("text_json")
            .as_deref()
            .map(serde_json::from_str)
            .transpose()?;
        timeline_items.push(TimelineItem {
            id: r.get("id"),
            track_id: r.get("track_id"),
            kind: serde_json::from_str(r.get::<String, _>("kind").as_str())?,
            source_media_id: r.get("source_media_id"),
            in_ms: r.get("in_ms"),
            out_ms: r.get("out_ms"),
            timeline_start_ms: r.get("timeline_start_ms"),
            speed: r.get::<f64, _>("speed") as f32,
            transform: serde_json::from_str(r.get::<String, _>("transform_json").as_str())?,
            effects: serde_json::from_str(r.get::<String, _>("effects_json").as_str())?,
            transition_in,
            text,
            enabled: r.get::<i64, _>("enabled") != 0,
            locked: r.get::<i64, _>("locked") != 0,
        });
    }

    let mut project = Project {
        id: row.get("id"),
        name: row.get("name"),
        video_path: row.get("video_path"),
        video_content_hash: row.get("video_content_hash"),
        video_duration_ms: row.get("video_duration_ms"),
        video_width: row.get::<i64, _>("video_width") as i32,
        video_height: row.get::<i64, _>("video_height") as i32,
        video_fps: row.get::<f64, _>("video_fps") as f32,
        audio_wav_path: row.get("audio_wav_path"),
        language: row.get("language"),
        default_style,
        context_description: row.get("context_description"),
        captions,
        speakers,
        glossary,
        clips,
        talk_summary,
        export_config,
        project_meta,
        media,
        tracks,
        timeline_items,
        created_at: row.get("created_at"),
        updated_at: row.get("updated_at"),
    };

    // Backward-compat: any v<=3 file has no tracks. Synthesize the minimal
    // multi-track shape (media pool entry + Video/Caption tracks + the video
    // placed as one full-length clip) so the rest of the app can treat every
    // project uniformly. Shared with `project_create_from_video`.
    //
    // Empty `tracks` ALONE cannot distinguish a v<=3 file from a v4 project
    // whose tracks the user deliberately deleted (a legal ops sequence) — the
    // `tracks_persisted` meta marker, written by every v4 `save`, does.
    // Backfilling a marker-carrying file would resurrect the deleted timeline
    // on the next open.
    if project.tracks.is_empty() {
        let tracks_persisted = sqlx::query("SELECT value FROM meta WHERE key = 'tracks_persisted'")
            .fetch_optional(&pool)
            .await?
            .is_some();
        if !tracks_persisted {
            let has_audio = project.audio_wav_path.is_some();
            project.backfill_default_timeline(has_audio);
        }
    }

    pool.close().await;
    Ok(project)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{TrackKind, Word};
    use crate::services::video::MediaKind;

    fn sample_project() -> Project {
        use crate::model::{ExportConfig, ProjectMeta};
        Project {
            id: "p1".into(),
            name: "test.mp4".into(),
            video_path: "/videos/test.mp4".into(),
            video_content_hash: "abc123".into(),
            video_duration_ms: 90_000,
            video_width: 1920,
            video_height: 1080,
            video_fps: 29.97,
            audio_wav_path: Some("/cache/test.wav".into()),
            language: "no".into(),
            default_style: Style::broadcast_news(),
            context_description: Some("A sermon about grace.".into()),
            export_config: ExportConfig {
                format: "vtt".into(),
                burn_in: true,
                caption_size_px: 28,
                caption_color: "yellow".into(),
                caption_background: "black".into(),
                max_chars_per_line: 52,
                karaoke: Some(crate::services::karaoke::KaraokeOptions {
                    enabled: true,
                    style: crate::services::karaoke::KaraokeStyle::Sweep,
                    ..Default::default()
                }),
            },
            project_meta: ProjectMeta {
                title: "Grace: A Sermon".into(),
                description: "Sunday morning sermon on grace.".into(),
                proper_nouns: "Lars, kerygma, soteriology".into(),
                language: "no".into(),
            },
            captions: vec![
                Caption {
                    id: "c1".into(),
                    start_ms: 0,
                    end_ms: 2000,
                    words: vec![
                        Word::new("Hello", 0, 500, 95.0),
                        Word::new("world", 500, 2000, 72.0),
                    ],
                    speaker_id: Some("s1".into()),
                    style_id: None,
                    notes: Some("note".into()),
                    ai_generated: true,
                    last_edited_at: 100,
                    track_id: Some("tc".into()),
                },
                Caption {
                    id: "c2".into(),
                    start_ms: 2500,
                    end_ms: 4000,
                    words: vec![Word::new("Again", 2500, 4000, 88.0)],
                    speaker_id: None,
                    style_id: None,
                    notes: None,
                    ai_generated: false,
                    last_edited_at: 200,
                    track_id: Some("tc".into()),
                },
            ],
            speakers: vec![Speaker {
                id: "s1".into(),
                display_name: "Lars".into(),
                color_hex: Some("#4FD1C5".into()),
            }],
            glossary: vec![GlossaryTerm {
                id: "g1".into(),
                term: "kerygma".into(),
                aliases: vec!["kerigma".into()],
                definition: None,
                pronunciation_hint: None,
            }],
            clips: vec![Clip {
                id: "clip1".into(),
                title: "Grace changes everything".into(),
                hook: "The one line that reframes the whole talk.".into(),
                caption_ids: vec!["c1".into(), "c2".into()],
                start_ms: 0,
                end_ms: 4000,
            }],
            talk_summary: Some("A short sermon about grace.".into()),
            media: vec![MediaItem {
                id: "m1".into(),
                path: "/videos/test.mp4".into(),
                content_hash: "abc123".into(),
                kind: MediaKind::Video,
                duration_ms: 90_000,
                width: 1920,
                height: 1080,
                fps: 29.97,
                has_audio: true,
                audio_wav_path: Some("/cache/test.wav".into()),
                original_filename: "test.mp4".into(),
                added_at: 1000,
            }],
            tracks: vec![
                Track {
                    id: "tv".into(),
                    kind: TrackKind::Video,
                    name: "Video".into(),
                    index: 0,
                    enabled: true,
                    locked: false,
                    muted: false,
                    solo: false,
                },
                Track {
                    id: "tc".into(),
                    kind: TrackKind::Caption,
                    name: "Captions".into(),
                    index: 1,
                    enabled: true,
                    locked: false,
                    muted: false,
                    solo: false,
                },
            ],
            timeline_items: vec![TimelineItem {
                id: "ti1".into(),
                track_id: "tv".into(),
                kind: crate::model::TimelineItemKind::Av,
                source_media_id: Some("m1".into()),
                in_ms: 0,
                out_ms: 4000,
                timeline_start_ms: 0,
                speed: 1.0,
                transform: crate::model::Transform::default(),
                effects: vec![crate::model::Effect {
                    id: "e1".into(),
                    kind: "brightness".into(),
                    params: serde_json::json!({ "amount": 0.2 }),
                    enabled: true,
                }],
                transition_in: Some(crate::model::Transition {
                    kind: "crossfade".into(),
                    duration_ms: 300,
                }),
                text: None,
                enabled: true,
                locked: false,
            }],
            created_at: 1000,
            updated_at: 2000,
        }
    }

    /// Turn a file just written by the v4 `save` into a faithful v<=3
    /// simulation: strip the `tracks_persisted` marker a real old file never
    /// carries (the hand-built v1/v2 fixtures above have no meta rows at all).
    async fn strip_v4_tracks_marker(path: &Path) {
        let url = format!("sqlite:{}", path.to_string_lossy());
        let opts = SqliteConnectOptions::from_str(&url).unwrap();
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(opts)
            .await
            .unwrap();
        sqlx::query("DELETE FROM meta WHERE key = 'tracks_persisted'")
            .execute(&pool)
            .await
            .unwrap();
        pool.close().await;
    }

    #[tokio::test]
    async fn save_and_load_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("project.sundayedit");
        let original = sample_project();

        save(&original, &path).await.unwrap();
        let loaded = load(&path).await.unwrap();

        assert_eq!(
            loaded, original,
            "round-trip must preserve the project exactly"
        );
    }

    #[tokio::test]
    async fn save_overwrites_previous_content() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("project.sundayedit");

        let mut p = sample_project();
        save(&p, &path).await.unwrap();

        // Remove a caption and re-save — the old caption must not linger
        p.captions.pop();
        p.updated_at = 9999;
        save(&p, &path).await.unwrap();

        let loaded = load(&path).await.unwrap();
        assert_eq!(loaded.captions.len(), 1);
        assert_eq!(loaded.updated_at, 9999);
    }

    #[tokio::test]
    async fn load_missing_file_errors() {
        let err = load(Path::new("/nonexistent/x.sundayedit"))
            .await
            .unwrap_err();
        assert_eq!(err.code(), "not_found");
    }

    #[tokio::test]
    async fn clips_and_summary_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("clips.sundayedit");
        let p = sample_project();
        save(&p, &path).await.unwrap();
        let loaded = load(&path).await.unwrap();
        assert_eq!(loaded.clips, p.clips);
        assert_eq!(loaded.talk_summary, p.talk_summary);
    }

    #[tokio::test]
    async fn export_config_and_project_meta_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("meta.sundayedit");
        let p = sample_project();
        save(&p, &path).await.unwrap();
        let loaded = load(&path).await.unwrap();
        assert_eq!(loaded.export_config, p.export_config);
        assert_eq!(loaded.project_meta, p.project_meta);
        assert_eq!(loaded.export_config.format, "vtt");
        assert_eq!(loaded.export_config.caption_color, "yellow");
        assert_eq!(loaded.export_config.max_chars_per_line, 52);
        // Karaoke settings persist with the rest of the burn-in preferences —
        // otherwise the sidecar, the render and the preview proxy would drift
        // apart across a save/open cycle (E4a).
        let k = loaded
            .export_config
            .karaoke
            .as_ref()
            .expect("karaoke options survive the round trip");
        assert!(k.enabled);
        assert_eq!(k.style, crate::services::karaoke::KaraokeStyle::Sweep);
        assert_eq!(loaded.project_meta.title, "Grace: A Sermon");
        assert_eq!(
            loaded.project_meta.proper_nouns,
            "Lars, kerygma, soteriology"
        );
    }

    #[tokio::test]
    async fn loads_v2_file_defaults_export_config_and_project_meta() {
        // Build a v2-shaped project table (clips but no config/meta columns).
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("v2.sundayedit");
        let url = format!("sqlite:{}", path.to_string_lossy());
        let opts = SqliteConnectOptions::from_str(&url)
            .unwrap()
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(opts)
            .await
            .unwrap();
        sqlx::query(
            r#"CREATE TABLE project (
                id TEXT PRIMARY KEY, name TEXT NOT NULL, video_path TEXT NOT NULL,
                video_content_hash TEXT NOT NULL, video_duration_ms INTEGER NOT NULL,
                video_width INTEGER NOT NULL, video_height INTEGER NOT NULL, video_fps REAL NOT NULL,
                audio_wav_path TEXT, language TEXT NOT NULL, default_style_json TEXT NOT NULL,
                context_description TEXT, speakers_json TEXT NOT NULL, glossary_json TEXT NOT NULL,
                clips_json TEXT NOT NULL DEFAULT '[]', talk_summary TEXT,
                created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL)"#,
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO project VALUES ('p','n','/v','h',1000,1920,1080,30.0,NULL,'no',?1,NULL,'[]','[]','[]',NULL,0,0)",
        )
        .bind(serde_json::to_string(&Style::broadcast_news()).unwrap())
        .execute(&pool)
        .await
        .unwrap();
        pool.close().await;

        let loaded = load(&path).await.unwrap();
        // Should default gracefully
        assert_eq!(loaded.export_config.format, "srt");
        assert!(!loaded.export_config.burn_in);
        assert!(
            loaded.export_config.karaoke.is_none(),
            "a pre-E4a file must open with karaoke OFF, not silently enabled"
        );
        assert_eq!(loaded.project_meta.title, "");
        assert_eq!(loaded.project_meta.language, "auto");
    }

    #[tokio::test]
    async fn loads_v1_file_without_clip_columns() {
        // Build a v1-shaped project table (no clip columns), then load —
        // ensure_schema must migrate the columns in and default to empty clips.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("v1.sundayedit");
        let url = format!("sqlite:{}", path.to_string_lossy());
        let opts = SqliteConnectOptions::from_str(&url)
            .unwrap()
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(opts)
            .await
            .unwrap();
        sqlx::query(
            r#"CREATE TABLE project (
                id TEXT PRIMARY KEY, name TEXT NOT NULL, video_path TEXT NOT NULL,
                video_content_hash TEXT NOT NULL, video_duration_ms INTEGER NOT NULL,
                video_width INTEGER NOT NULL, video_height INTEGER NOT NULL, video_fps REAL NOT NULL,
                audio_wav_path TEXT, language TEXT NOT NULL, default_style_json TEXT NOT NULL,
                context_description TEXT, speakers_json TEXT NOT NULL, glossary_json TEXT NOT NULL,
                created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL)"#,
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO project VALUES ('p','n','/v','h',1000,1920,1080,30.0,NULL,'no',?1,NULL,'[]','[]',0,0)",
        )
        .bind(serde_json::to_string(&Style::broadcast_news()).unwrap())
        .execute(&pool)
        .await
        .unwrap();
        pool.close().await;

        let loaded = load(&path).await.unwrap();
        assert!(loaded.clips.is_empty());
        assert_eq!(loaded.talk_summary, None);
    }

    #[tokio::test]
    async fn caption_order_is_preserved() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("order.sundayedit");
        let p = sample_project();
        save(&p, &path).await.unwrap();
        let loaded = load(&path).await.unwrap();
        assert_eq!(loaded.captions[0].id, "c1");
        assert_eq!(loaded.captions[1].id, "c2");
    }

    #[tokio::test]
    async fn media_tracks_timeline_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nle.sundayedit");
        let p = sample_project();
        save(&p, &path).await.unwrap();
        let loaded = load(&path).await.unwrap();
        assert_eq!(loaded.media, p.media);
        assert_eq!(loaded.tracks, p.tracks);
        assert_eq!(loaded.timeline_items, p.timeline_items);
        // Spot-check nested JSON columns survived.
        let it = &loaded.timeline_items[0];
        assert_eq!(it.effects[0].kind, "brightness");
        assert_eq!(it.transition_in.as_ref().unwrap().duration_ms, 300);
        assert_eq!(it.transform, crate::model::Transform::default());
        assert_eq!(loaded.captions[0].track_id.as_deref(), Some("tc"));
    }

    #[tokio::test]
    async fn loads_v3_file_backfills_media_and_tracks() {
        // A v<=3 file has no tracks. We simulate one by saving a project with
        // empty media/tracks/timeline_items — on load, the backfill must
        // synthesize a video track + caption track + one media item, and give
        // every caption the caption track's id.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("v3.sundayedit");
        let mut p = sample_project();
        p.media = vec![];
        p.tracks = vec![];
        p.timeline_items = vec![];
        for c in p.captions.iter_mut() {
            c.track_id = None;
        }
        save(&p, &path).await.unwrap();
        strip_v4_tracks_marker(&path).await;

        let loaded = load(&path).await.unwrap();
        assert_eq!(loaded.media.len(), 1, "one media item synthesized");
        assert_eq!(loaded.media[0].content_hash, "abc123");
        assert_eq!(loaded.media[0].original_filename, "test.mp4");
        assert!(
            loaded.media[0].has_audio,
            "audio_wav_path present → has_audio"
        );

        let video_tracks: Vec<_> = loaded
            .tracks
            .iter()
            .filter(|t| t.kind == TrackKind::Video)
            .collect();
        let caption_tracks: Vec<_> = loaded
            .tracks
            .iter()
            .filter(|t| t.kind == TrackKind::Caption)
            .collect();
        assert_eq!(video_tracks.len(), 1, "one video track");
        assert_eq!(caption_tracks.len(), 1, "one caption track");

        let cap_track_id = &caption_tracks[0].id;
        for c in &loaded.captions {
            assert_eq!(
                c.track_id.as_ref(),
                Some(cap_track_id),
                "every caption points at the caption track"
            );
        }

        // The backfill also PLACES the video: one full-length Av clip at 0,
        // so lanes + preview see the source immediately.
        assert_eq!(loaded.timeline_items.len(), 1, "one placed clip");
        let it = &loaded.timeline_items[0];
        assert_eq!(it.track_id, video_tracks[0].id);
        assert_eq!(
            it.source_media_id.as_deref(),
            Some(loaded.media[0].id.as_str())
        );
        assert_eq!((it.in_ms, it.out_ms, it.timeline_start_ms), (0, 90_000, 0));
        assert_eq!(it.kind, crate::model::TimelineItemKind::Av);
        assert!(loaded.validate_timeline().is_ok());
    }

    #[tokio::test]
    async fn backfilled_project_round_trips_without_duplication() {
        // v<=3 file → load (backfills) → save (persists the v4 shape) → load
        // again: the second load must NOT re-backfill — same media, tracks and
        // the single placed clip, identical ids, no duplicates.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("v3rt.sundayedit");
        let mut p = sample_project();
        p.media = vec![];
        p.tracks = vec![];
        p.timeline_items = vec![];
        for c in p.captions.iter_mut() {
            c.track_id = None;
        }
        save(&p, &path).await.unwrap();
        strip_v4_tracks_marker(&path).await;

        let loaded = load(&path).await.unwrap();
        assert_eq!(loaded.timeline_items.len(), 1);

        let resaved = dir.path().join("v4rt.sundayedit");
        save(&loaded, &resaved).await.unwrap();
        let reloaded = load(&resaved).await.unwrap();
        assert_eq!(reloaded.media, loaded.media);
        assert_eq!(reloaded.tracks, loaded.tracks);
        assert_eq!(reloaded.timeline_items, loaded.timeline_items);
        assert_eq!(reloaded.captions, loaded.captions);
    }

    #[tokio::test]
    async fn deleted_timeline_survives_save_load_roundtrip() {
        // A v4 project can legitimately reach ZERO tracks through the public
        // ops alone: ripple-delete the auto-placed clip, then remove each
        // now-empty track (legal when no items/captions reference it, and
        // validate_timeline has no minimum-track rule). Saving stores zero
        // track rows — the `tracks_persisted` marker is what tells this file
        // apart from a v<=3 file, so reopening must NOT resurrect the
        // synthesized tracks + full-length clip the user deleted on purpose.
        use crate::services::timeline_ops::{remove_track, ripple_delete_item};

        let mut p = sample_project();
        p.captions = vec![]; // caption-free so the caption track is removable
        let p = ripple_delete_item(&p, "ti1").expect("deleting the auto-placed clip is legal");
        let p = remove_track(&p, "tv").expect("removing the emptied video track is legal");
        let p = remove_track(&p, "tc").expect("removing the caption track is legal");
        assert!(p.tracks.is_empty(), "ops legitimately reached zero tracks");
        assert!(p.timeline_items.is_empty());

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("emptied.sundayedit");
        save(&p, &path).await.unwrap();
        let loaded = load(&path).await.unwrap();

        assert!(
            loaded.tracks.is_empty(),
            "deleted tracks resurrected on load: {:?}",
            loaded
                .tracks
                .iter()
                .map(|t| (&t.name, &t.kind))
                .collect::<Vec<_>>()
        );
        assert!(
            loaded.timeline_items.is_empty(),
            "deleted clip resurrected on load: {:?}",
            loaded
                .timeline_items
                .iter()
                .map(|it| (&it.id, it.timeline_start_ms, it.out_ms))
                .collect::<Vec<_>>()
        );
    }
}
