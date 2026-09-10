//! Core domain models that map to the SQLite schema.
//!
//! These structs are the shared vocabulary between the storage layer,
//! the capture pipeline, and the ranking engine.

use chrono::{DateTime, Utc};

/// Fast non-cryptographic hash of a directory path (used as `dir_hash`).
pub fn dir_hash(path: &str) -> String {
    format!("{:016x}", xxhash_rust::xxh3::xxh3_64(path.as_bytes()))
}

/// Fast non-cryptographic hash of a command string (used as `cmd_hash`).
pub fn cmd_hash(cmd: &str) -> String {
    format!("{:016x}", xxhash_rust::xxh3::xxh3_64(cmd.as_bytes()))
}

/// A user-pinned command, resolved with enough context to render.
#[derive(Debug, Clone, PartialEq)]
pub struct PinnedCommand {
    /// FK → `commands.id`.
    pub command_id: i64,
    /// The literal command string (post-redaction).
    pub cmd_string: String,
    /// Absolute path of the owning project, `None` for non-project commands.
    pub project_path: Option<String>,
    /// UTC timestamp when the pin was created.
    pub pinned_at: DateTime<Utc>,
}

/// A single record being bulk-imported from legacy shell history.
#[derive(Debug, Clone, PartialEq)]
pub struct ImportEntry {
    /// The command string (pre-redaction).
    pub cmd: String,
    /// Timestamp from zsh extended history, `None` if unknown
    /// (bash history, or entries without a header).
    pub executed_at: Option<DateTime<Utc>>,
}

/// Outcome of a bulk history import.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImportReport {
    /// Number of new commands persisted (each with one execution row).
    pub imported: u64,
    /// Number of history lines whose command already existed.
    pub duplicates: u64,
    /// Number of commands that were altered by secret redaction.
    pub redacted: u64,
}
