//! Core domain models that map to the SQLite schema.
//!
//! These structs are the shared vocabulary between the storage layer,
//! the capture pipeline, and the ranking engine.

#![allow(dead_code)]

use chrono::{DateTime, Utc};

/// Fast non-cryptographic hash of a directory path (used as `dir_hash`).
pub fn dir_hash(path: &str) -> String {
    format!("{:016x}", xxhash_rust::xxh3::xxh3_64(path.as_bytes()))
}

/// Fast non-cryptographic hash of a command string (used as `cmd_hash`).
pub fn cmd_hash(cmd: &str) -> String {
    format!("{:016x}", xxhash_rust::xxh3::xxh3_64(cmd.as_bytes()))
}

/// A known project root (typically the `.git` directory boundary).
#[derive(Debug, Clone, PartialEq)]
pub struct Project {
    /// Auto-incrementing primary key.
    pub id: i64,
    /// Hex-encoded xxh3 hash of the project root path.
    pub dir_hash: String,
    /// Human-readable absolute path to the project root.
    pub path: String,
}

/// A unique command string, scoped to a project.
///
/// The `(cmd_hash, project_id)` pair is unique — the same command in two
/// different projects is stored as two separate rows so that
/// project-specific ranking can work correctly.
#[derive(Debug, Clone, PartialEq)]
pub struct Command {
    /// Auto-incrementing primary key.
    pub id: i64,
    /// FK → `projects.id` (`None` for non-project commands).
    pub project_id: Option<i64>,
    /// Hex-encoded xxh3 hash of `cmd_string` for fast deduplication.
    pub cmd_hash: String,
    /// The literal command string the user typed (post-redaction).
    pub cmd_string: String,
    /// UTC timestamp when this command was first recorded.
    pub created_at: DateTime<Utc>,
}

/// A single execution of a [`Command`].
///
/// One command can have many executions — each time the user re-runs it,
/// a new `Execution` row is inserted.
#[derive(Debug, Clone, PartialEq)]
pub struct Execution {
    /// Auto-incrementing primary key.
    pub id: i64,
    /// FK → `commands.id`.
    pub command_id: i64,
    /// Process exit code (`Some(0)` = success).
    pub exit_code: Option<i32>,
    /// Wall-clock duration in milliseconds, `None` if unknown.
    pub duration_ms: Option<i64>,
    /// Absolute path of the working directory at execution time.
    pub working_dir: String,
    /// UTC timestamp when this execution occurred.
    pub executed_at: DateTime<Utc>,
}

/// Materialized aggregate stats for a command, maintained by SQLite triggers.
#[derive(Debug, Clone, PartialEq)]
pub struct CommandStats {
    /// FK → `commands.id`.
    pub command_id: i64,
    /// Number of executions with exit code 0.
    pub success_count: i64,
    /// Number of executions with exit code != 0.
    pub fail_count: i64,
    /// When this command was last executed, `None` if never.
    pub last_executed_at: Option<DateTime<Utc>>,
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
