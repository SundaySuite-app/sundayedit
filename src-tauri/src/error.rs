//! Centralised error type for SundayEdit backend.
//!
//! Serializes to `{ code, message }` JSON across the IPC boundary so
//! React can pattern-match on `code` for UI flows like "video file
//! missing → show relink dialog".

use serde::{Serialize, Serializer};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum AppError {
    #[error("database: {0}")]
    Database(#[from] sqlx::Error),

    #[error("migration: {0}")]
    Migration(#[from] sqlx::migrate::MigrateError),

    #[error("not found: {entity} id={id}")]
    NotFound { entity: &'static str, id: String },

    #[error("invariant violated: {0}")]
    Invariant(String),

    #[error("validation: {0}")]
    Validation(String),

    #[error("io: {0}")]
    Io(#[from] std::io::Error),

    #[error("invalid json: {0}")]
    Json(#[from] serde_json::Error),

    /// Network failure (e.g. downloading a Whisper model).
    #[error("network: {0}")]
    Network(String),

    /// User's video file moved or was deleted between sessions.
    /// The renderer triggers the relink UI on this.
    #[error("video missing at {0}")]
    VideoMissing(String),

    /// A `.sundayedit` file's stored `schema_version` is higher than this
    /// build's `SCHEMA_VERSION` — it was written by a newer version of the
    /// app. Refused outright rather than opened: this build doesn't know
    /// what the newer schema added, so loading it and saving back would
    /// silently drop whatever that was (see `services::project_file`).
    #[error(
        "project file schema {file_version} is newer than this build supports ({supported}) — \
         it was made by a newer version of SundayEdit"
    )]
    SchemaTooNew { file_version: i64, supported: i64 },

    #[error("internal: {0}")]
    Internal(String),
}

impl AppError {
    pub fn code(&self) -> &'static str {
        match self {
            AppError::Database(_) => "database",
            AppError::Migration(_) => "migration",
            AppError::NotFound { .. } => "not_found",
            AppError::Invariant(_) => "invariant",
            AppError::Validation(_) => "validation",
            AppError::Io(_) => "io",
            AppError::Json(_) => "json",
            AppError::Network(_) => "network",
            AppError::VideoMissing(_) => "video_missing",
            AppError::SchemaTooNew { .. } => "schema_too_new",
            AppError::Internal(_) => "internal",
        }
    }
}

impl Serialize for AppError {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        use serde::ser::SerializeStruct;
        let mut s = serializer.serialize_struct("AppError", 2)?;
        s.serialize_field("code", self.code())?;
        s.serialize_field("message", &self.to_string())?;
        s.end()
    }
}

pub type AppResult<T> = Result<T, AppError>;
