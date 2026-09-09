//! Centralized error types for `hs`.
//!
//! Every subsystem maps its failures into [`HsError`], keeping the
//! error-propagation chain uniform and the call-sites clean.

use std::path::PathBuf;

/// The single error type used throughout the `hs` crate.
#[derive(Debug, thiserror::Error)]
pub enum HsError {
    #[error("database error: {0}")]
    DatabaseError(#[from] rusqlite::Error),

    #[error("database migration failed at step {step}: {message}")]
    MigrationError { step: u32, message: String },

    #[error("I/O error: {0}")]
    IoError(#[from] std::io::Error),

    #[error("could not resolve data directory")]
    DataDirNotFound,

    #[error("failed to create data directory `{path}`: {source}")]
    CreateDataDir {
        path: PathBuf,
        source: std::io::Error,
    },

    #[error("r2d2 pool error: {0}")]
    PoolError(String),

    #[error("execution cancelled by user")]
    Cancelled,
}

impl From<r2d2::Error> for HsError {
    fn from(err: r2d2::Error) -> Self {
        HsError::PoolError(err.to_string())
    }
}
