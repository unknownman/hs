//! Core domain models that map to the SQLite schema.
//!
//! These structs are the shared vocabulary between the storage layer,
//! the capture pipeline, and the ranking engine.

#![allow(dead_code)]

use chrono::{DateTime, Utc};

/// A unique command string, optionally scoped to a project.
///
/// The `(cmd_string, project_hash)` pair is unique — the same command
/// in two different projects is stored as two separate rows so that
/// project-specific ranking can work correctly.
#[derive(Debug, Clone, PartialEq)]
pub struct Command {
    /// Auto-incrementing primary key.
    pub id: i64,
    /// The literal command string the user typed (post-redaction).
    pub cmd_string: String,
    /// Hex hash of the project root path, or `None` for non-project commands.
    pub project_hash: Option<String>,
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
