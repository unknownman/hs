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

/// A user-pinned command that resists rank decay.
#[derive(Debug, Clone, PartialEq)]
pub struct Pin {
    /// FK → `commands.id`.
    pub command_id: i64,
    /// UTC timestamp when the pin was created.
    pub pinned_at: DateTime<Utc>,
}
