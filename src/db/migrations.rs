//! Schema migrations for the `hs` SQLite database.
//!
//! Versioning is handled via `PRAGMA user_version`.  Each migration is a
//! numbered step; the runner applies only those whose number exceeds the
//! current `user_version`.
//!
//! # V1 Schema
//!
//! | Object | Type | Purpose |
//! |--------|------|---------|
//! | `projects` | TABLE | Known project roots (git boundaries) |
//! | `commands` | TABLE | Deduplicated command strings, scoped to a project |
//! | `executions` | TABLE | Individual command runs with exit code, duration, cwd |
//! | `command_stats` | TABLE | Materialized aggregates for fast ranking (auto-updated via triggers) |
//! | `pins` | TABLE | User-pinned commands that resist rank decay |
//! | `commands_fts` | FTS5 | Full-text search index over `cmd_string` |
//! | `commands_ai` | TRIGGER | Keeps FTS5 in sync on INSERT |
//! | `commands_ad` | TRIGGER | Keeps FTS5 in sync on DELETE |
//! | `commands_au` | TRIGGER | Keeps FTS5 in sync on UPDATE |
//! | `stats_on_insert` | TRIGGER | Auto-updates `command_stats` on new execution |

use rusqlite::Connection;

use crate::error::HsError;

/// Current schema version.  Bump this and add a new `V{N}_MIGRATION`
/// constant every time the schema changes.
const CURRENT_VERSION: u32 = 1;

/// Run all pending migrations against `conn`.
///
/// Idempotent — safe to call on every `hs` startup.
pub fn run(conn: &Connection) -> Result<(), HsError> {
    let current: u32 = conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .map_err(HsError::DatabaseError)?;

    if current >= CURRENT_VERSION {
        return Ok(());
    }

    // V1: fully normalized schema + FTS5 + stats triggers
    if current < 1 {
        conn.execute_batch(V1_MIGRATION)
            .map_err(|e| HsError::MigrationError {
                step: 1,
                message: e.to_string(),
            })?;
    }

    // Mark schema as up-to-date.
    conn.execute_batch(&format!("PRAGMA user_version = {CURRENT_VERSION}"))
        .map_err(|e| HsError::MigrationError {
            step: CURRENT_VERSION,
            message: e.to_string(),
        })?;

    Ok(())
}

// ---------------------------------------------------------------------------
// SQL fragments
// ---------------------------------------------------------------------------

const V1_MIGRATION: &str = r#"
-- ────────────────────────────────────────────────────────────────────
-- projects: known project roots (typically a .git directory boundary)
-- ────────────────────────────────────────────────────────────────────
CREATE TABLE IF NOT EXISTS projects (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    dir_hash        TEXT    NOT NULL UNIQUE,
    path            TEXT    NOT NULL
);

-- ────────────────────────────────────────────────────────────────────
-- commands: one row per unique (cmd_hash, project_id) pair
-- ────────────────────────────────────────────────────────────────────
CREATE TABLE IF NOT EXISTS commands (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    project_id      INTEGER REFERENCES projects(id) ON DELETE SET NULL,
    cmd_hash        TEXT    NOT NULL,
    cmd_string      TEXT    NOT NULL,
    created_at      TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    UNIQUE(cmd_hash, project_id)
);

-- ────────────────────────────────────────────────────────────────────
-- executions: one row per time a command is actually run
-- ────────────────────────────────────────────────────────────────────
CREATE TABLE IF NOT EXISTS executions (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    command_id      INTEGER NOT NULL REFERENCES commands(id) ON DELETE CASCADE,
    exit_code       INTEGER,
    duration_ms     INTEGER,
    working_dir     TEXT    NOT NULL,
    executed_at     TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
);

CREATE INDEX IF NOT EXISTS idx_executions_command_id
    ON executions(command_id);

-- ────────────────────────────────────────────────────────────────────
-- command_stats: materialized aggregates kept in sync by triggers
-- ────────────────────────────────────────────────────────────────────
CREATE TABLE IF NOT EXISTS command_stats (
    command_id      INTEGER PRIMARY KEY REFERENCES commands(id) ON DELETE CASCADE,
    success_count   INTEGER NOT NULL DEFAULT 0,
    fail_count      INTEGER NOT NULL DEFAULT 0,
    last_executed_at TEXT
);

-- ────────────────────────────────────────────────────────────────────
-- pins: user-pinned commands that resist rank decay
-- ────────────────────────────────────────────────────────────────────
CREATE TABLE IF NOT EXISTS pins (
    command_id      INTEGER PRIMARY KEY REFERENCES commands(id) ON DELETE CASCADE,
    pinned_at       TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
);

-- ────────────────────────────────────────────────────────────────────
-- FTS5 virtual table for fast full-text search on command strings
-- ────────────────────────────────────────────────────────────────────
CREATE VIRTUAL TABLE IF NOT EXISTS commands_fts USING fts5(
    cmd_string,
    content='commands',
    content_rowid='id'
);

-- ────────────────────────────────────────────────────────────────────
-- Triggers to keep FTS index synchronised with the commands table
-- ────────────────────────────────────────────────────────────────────
CREATE TRIGGER IF NOT EXISTS commands_ai AFTER INSERT ON commands BEGIN
    INSERT INTO commands_fts(rowid, cmd_string)
    VALUES (new.id, new.cmd_string);
END;

CREATE TRIGGER IF NOT EXISTS commands_ad AFTER DELETE ON commands BEGIN
    INSERT INTO commands_fts(commands_fts, rowid, cmd_string)
    VALUES ('delete', old.id, old.cmd_string);
END;

CREATE TRIGGER IF NOT EXISTS commands_au AFTER UPDATE ON commands BEGIN
    INSERT INTO commands_fts(commands_fts, rowid, cmd_string)
    VALUES ('delete', old.id, old.cmd_string);
    INSERT INTO commands_fts(rowid, cmd_string)
    VALUES (new.id, new.cmd_string);
END;

-- ────────────────────────────────────────────────────────────────────
-- Trigger to auto-update command_stats on each new execution
-- ────────────────────────────────────────────────────────────────────
CREATE TRIGGER IF NOT EXISTS stats_on_insert AFTER INSERT ON executions BEGIN
    INSERT INTO command_stats (command_id, success_count, fail_count, last_executed_at)
    VALUES (
        new.command_id,
        CASE WHEN new.exit_code = 0 THEN 1 ELSE 0 END,
        CASE WHEN new.exit_code != 0 THEN 1 ELSE 0 END,
        new.executed_at
    )
    ON CONFLICT(command_id) DO UPDATE SET
        success_count = command_stats.success_count + CASE WHEN excluded.success_count > 0 THEN 1 ELSE 0 END,
        fail_count    = command_stats.fail_count    + CASE WHEN excluded.fail_count    > 0 THEN 1 ELSE 0 END,
        last_executed_at = excluded.last_executed_at;
END;
"#;
