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
//! | `commands` | TABLE | Deduplicated command strings, optionally scoped to a project |
//! | `executions` | TABLE | Individual command runs with exit code, duration, cwd |
//! | `commands_fts` | FTS5 | Full-text search index over `cmd_string` |
//! | `commands_ai` | TRIGGER | Keeps FTS5 in sync on INSERT |
//! | `commands_ad` | TRIGGER | Keeps FTS5 in sync on DELETE |
//! | `commands_au` | TRIGGER | Keeps FTS5 in sync on UPDATE |

use r2d2_sqlite::rusqlite::Connection;

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

    // V1: core tables + FTS5
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
-- commands: one row per unique (cmd_string, project_hash) pair
-- ────────────────────────────────────────────────────────────────────
CREATE TABLE IF NOT EXISTS commands (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    cmd_string      TEXT    NOT NULL,
    project_hash    TEXT,                          -- NULL = not inside a project
    created_at      TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    UNIQUE(cmd_string, project_hash)
);

-- ────────────────────────────────────────────────────────────────────
-- executions: one row per time a command is actually run
-- ────────────────────────────────────────────────────────────────────
CREATE TABLE IF NOT EXISTS executions (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    command_id      INTEGER NOT NULL REFERENCES commands(id) ON DELETE CASCADE,
    exit_code       INTEGER,                       -- NULL = unknown (e.g. killed)
    duration_ms     INTEGER,                       -- NULL = unknown
    working_dir     TEXT    NOT NULL,
    executed_at     TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
);

CREATE INDEX IF NOT EXISTS idx_executions_command_id
    ON executions(command_id);

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
"#;
