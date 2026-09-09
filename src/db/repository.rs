//! Repository-style API for the `hs` persistence layer.
//!
//! [`Store`] wraps a [`DbPool`](crate::db::DbPool) and provides
//! prepared-statement methods for all write and read operations.
//! Every public method acquires a single connection from the pool,
//! executes the operation, and returns the connection — keeping
//! latency low and pool contention minimal.

#![allow(dead_code)]

use rusqlite::params;

use crate::db::DbPool;
use crate::error::HsError;
use crate::models::{CommandStats, cmd_hash, dir_hash};

/// High-level interface over the SQLite persistence layer.
///
/// All methods are synchronous and acquire a connection from the pool
/// per call.  This matches the shell-hook latency budget (< 50 ms).
pub struct Store {
    pool: DbPool,
}

impl Store {
    /// Create a new `Store` backed by the given connection pool.
    pub fn new(pool: DbPool) -> Self {
        Self { pool }
    }

    /// Record a command execution.
    ///
    /// This method:
    /// 1. Resolves (or creates) the project for `project_path`.
    /// 2. Resolves (or creates) the command, deduplicating on
    ///    `(cmd_hash, project_id)`.
    /// 3. Inserts the execution row.  Triggers auto-update
    ///    `command_stats`.
    ///
    /// # Arguments
    ///
    /// * `project_path` — Absolute path to the project root (`None` if
    ///   the command was run outside any known project).
    /// * `cmd_string` — The literal command the user typed.
    /// * `exit_code` — Process exit code (`0` = success).
    /// * `duration_ms` — Wall-clock duration in milliseconds.
    /// * `cwd` — Working directory at execution time.
    pub fn insert_execution(
        &self,
        project_path: Option<&str>,
        cmd_string: &str,
        exit_code: i32,
        duration_ms: i64,
        cwd: &str,
    ) -> Result<(), HsError> {
        let conn = self.pool.get()?;

        // 1. Resolve or create the project.
        let project_id: Option<i64> = match project_path {
            Some(path) => {
                let hash = dir_hash(path);
                conn.execute(
                    "INSERT OR IGNORE INTO projects (dir_hash, path) VALUES (?1, ?2)",
                    params![hash, path],
                )?;
                let id: i64 = conn.query_row(
                    "SELECT id FROM projects WHERE dir_hash = ?1",
                    params![hash],
                    |row| row.get(0),
                )?;
                Some(id)
            }
            None => None,
        };

        // 2. Resolve or create the command.
        let c_hash = cmd_hash(cmd_string);
        conn.execute(
            "INSERT OR IGNORE INTO commands (project_id, cmd_hash, cmd_string)
             VALUES (?1, ?2, ?3)",
            params![project_id, c_hash, cmd_string],
        )?;
        let command_id: i64 = conn.query_row(
            "SELECT id FROM commands WHERE cmd_hash = ?1 AND project_id IS ?2",
            params![c_hash, project_id],
            |row| row.get(0),
        )?;

        // 3. Insert the execution (triggers update command_stats).
        conn.execute(
            "INSERT INTO executions (command_id, exit_code, duration_ms, working_dir)
             VALUES (?1, ?2, ?3, ?4)",
            params![command_id, exit_code, duration_ms, cwd],
        )?;

        Ok(())
    }

    /// Pin a command so it resists rank decay in search results.
    pub fn pin_command(&self, command_id: i64) -> Result<(), HsError> {
        let conn = self.pool.get()?;
        conn.execute(
            "INSERT OR IGNORE INTO pins (command_id) VALUES (?1)",
            params![command_id],
        )?;
        Ok(())
    }

    /// Remove a pin from a previously pinned command.
    pub fn unpin_command(&self, command_id: i64) -> Result<(), HsError> {
        let conn = self.pool.get()?;
        conn.execute(
            "DELETE FROM pins WHERE command_id = ?1",
            params![command_id],
        )?;
        Ok(())
    }

    /// Retrieve materialized stats for a command.
    ///
    /// Returns `None` if no stats exist yet (command has never been
    /// executed through the Store).
    pub fn get_command_stats(&self, command_id: i64) -> Result<Option<CommandStats>, HsError> {
        let conn = self.pool.get()?;
        let result = conn.query_row(
            "SELECT command_id, success_count, fail_count, last_executed_at
             FROM command_stats WHERE command_id = ?1",
            params![command_id],
            |row| {
                Ok(CommandStats {
                    command_id: row.get(0)?,
                    success_count: row.get(1)?,
                    fail_count: row.get(2)?,
                    last_executed_at: row.get(3)?,
                })
            },
        );

        match result {
            Ok(stats) => Ok(Some(stats)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(HsError::DatabaseError(e)),
        }
    }
}

// ── Tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Convenience: build a [`Store`] backed by an in-memory database
    /// with all migrations applied.
    fn test_store() -> Store {
        let pool = crate::db::tests::test_pool();
        Store::new(pool)
    }

    #[test]
    fn trigger_updates_command_stats() {
        let store = test_store();

        // Insert a successful execution.
        store.insert_execution(None, "ls", 0, 12, "/tmp").unwrap();

        // Insert a failed execution for the same command.
        store.insert_execution(None, "ls", 1, 50, "/tmp").unwrap();

        // Look up the command_id and stats in one connection scope.
        {
            let conn = store.pool.get().unwrap();
            let command_id: i64 = conn
                .query_row(
                    "SELECT id FROM commands WHERE cmd_string = 'ls'",
                    [],
                    |row| row.get(0),
                )
                .unwrap();

            let (success_count, fail_count): (i64, i64) = conn
                .query_row(
                    "SELECT success_count, fail_count FROM command_stats
                     WHERE command_id = ?1",
                    [command_id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .unwrap();

            assert_eq!(success_count, 1, "success_count should be 1");
            assert_eq!(fail_count, 1, "fail_count should be 1");
        }

        // Verify last_executed_at is set.
        {
            let conn = store.pool.get().unwrap();
            let has_last: bool = conn
                .query_row(
                    "SELECT last_executed_at IS NOT NULL
                     FROM command_stats
                     WHERE command_id = (
                         SELECT id FROM commands WHERE cmd_string = 'ls'
                     )",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert!(has_last, "last_executed_at should be set");
        }
    }

    #[test]
    fn fts5_search_through_repository() {
        let store = test_store();

        store
            .insert_execution(None, "cargo build --release", 0, 5000, "/project")
            .unwrap();

        let conn = store.pool.get().unwrap();
        let count: i32 = conn
            .query_row(
                "SELECT COUNT(*) FROM commands
                 WHERE id IN (
                     SELECT rowid FROM commands_fts
                     WHERE commands_fts MATCH 'build'
                 )",
                [],
                |row| row.get(0),
            )
            .unwrap();

        assert_eq!(count, 1, "FTS5 should find the 'build' token");
    }

    #[test]
    fn same_command_same_project_deduplicates() {
        let store = test_store();

        // Insert the same command in the same project twice.
        store
            .insert_execution(Some("/my/project"), "make deploy", 0, 200, "/my/project")
            .unwrap();
        store
            .insert_execution(Some("/my/project"), "make deploy", 0, 210, "/my/project")
            .unwrap();

        let conn = store.pool.get().unwrap();

        // Exactly one row in commands.
        let cmd_count: i32 = conn
            .query_row(
                "SELECT COUNT(*) FROM commands WHERE cmd_string = 'make deploy'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(cmd_count, 1, "commands table should have exactly one row");

        // Exactly two rows in executions.
        let exec_count: i32 = conn
            .query_row("SELECT COUNT(*) FROM executions", [], |row| row.get(0))
            .unwrap();
        assert_eq!(exec_count, 2, "executions table should have two rows");
    }

    #[test]
    fn same_command_different_projects_are_separate() {
        let store = test_store();

        store
            .insert_execution(Some("/project-a"), "make build", 0, 100, "/project-a/src")
            .unwrap();
        store
            .insert_execution(Some("/project-b"), "make build", 0, 120, "/project-b/src")
            .unwrap();

        let conn = store.pool.get().unwrap();
        let cmd_count: i32 = conn
            .query_row(
                "SELECT COUNT(*) FROM commands WHERE cmd_string = 'make build'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(cmd_count, 2, "same command in different projects = 2 rows");
    }

    #[test]
    fn pin_and_unpin() {
        let store = test_store();

        store
            .insert_execution(None, "cargo test", 0, 1000, "/proj")
            .unwrap();

        // Resolve the command_id in a scoped block, then release the
        // connection back to the pool before calling Store methods.
        let command_id = {
            let conn = store.pool.get().unwrap();
            conn.query_row(
                "SELECT id FROM commands WHERE cmd_string = 'cargo test'",
                [],
                |row| row.get(0),
            )
            .unwrap()
        };

        // Pin it.
        store.pin_command(command_id).unwrap();
        let pin_count: i32 = {
            let conn = store.pool.get().unwrap();
            conn.query_row("SELECT COUNT(*) FROM pins", [], |row| row.get(0))
                .unwrap()
        };
        assert_eq!(pin_count, 1);

        // Unpin it.
        store.unpin_command(command_id).unwrap();
        let pin_count: i32 = {
            let conn = store.pool.get().unwrap();
            conn.query_row("SELECT COUNT(*) FROM pins", [], |row| row.get(0))
                .unwrap()
        };
        assert_eq!(pin_count, 0);
    }

    #[test]
    fn foreign_key_rejects_orphan_pin() {
        let store = test_store();
        let conn = store.pool.get().unwrap();

        let result = conn.execute("INSERT INTO pins (command_id) VALUES (999999)", []);
        assert!(result.is_err(), "FK should reject orphan pin");
    }

    #[test]
    fn non_project_command_stored_with_null_project() {
        let store = test_store();

        store
            .insert_execution(None, "echo hello", 0, 5, "/tmp")
            .unwrap();

        let conn = store.pool.get().unwrap();
        let project_id: Option<i64> = conn
            .query_row(
                "SELECT project_id FROM commands WHERE cmd_string = 'echo hello'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(project_id, None, "project_id should be NULL");
    }
}
