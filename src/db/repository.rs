//! Repository-style API for the `hs` persistence layer.
//!
//! [`Store`] wraps a [`DbPool`](crate::db::DbPool) and provides
//! prepared-statement methods for all write and read operations.
//! Every public method acquires a single connection from the pool,
//! executes the operation, and returns the connection — keeping
//! latency low and pool contention minimal.

#![allow(dead_code)]

use rusqlite::params;
use rusqlite::types::Value;

use crate::db::DbPool;
use crate::error::HsError;
use crate::models::{CommandStats, cmd_hash, dir_hash};
use crate::ranking::{RawCandidate, SearchContext};

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

    /// Fetch raw candidates for ranking (Phase 6 recall halving).
    ///
    /// Builds a dynamic SQL query applying the hard filters in
    /// [`SearchContext`]:
    ///
    /// * FTS5 `MATCH` (with its `bm25` score) when a query is present;
    ///   a `1.0` sentinel score when not.
    /// * Project scoping (unless `global`).
    /// * Success / failure filters (`--ok`, `--failed`).
    /// * Recency window (`--last`).
    ///
    /// At most ~500 rows are returned — all complex scoring math is left
    /// to [`crate::ranking::rank_commands`].
    pub fn fetch_candidates(&self, ctx: &SearchContext) -> Result<Vec<RawCandidate>, HsError> {
        let conn = self.pool.get()?;

        let mut sql = String::from(
            "SELECT c.id, c.cmd_string, c.project_id,
                    s.success_count, s.fail_count, s.last_executed_at,",
        );

        let mut wheres: Vec<String> = Vec::new();
        let mut values: Vec<Value> = Vec::new();

        if let Some(query) = ctx.query.as_deref() {
            sql.push_str(
                " bm25(commands_fts) AS bm
                FROM commands c
                INNER JOIN commands_fts ON commands_fts.rowid = c.id
                LEFT JOIN command_stats s ON s.command_id = c.id",
            );
            wheres.push("commands_fts MATCH ?".to_string());
            values.push(Value::Text(sanitize_fts_query(query)));
        } else {
            sql.push_str(
                " 1.0 AS bm
                FROM commands c
                LEFT JOIN command_stats s ON s.command_id = c.id",
            );
        }

        // Hard project filter (only active in non-global mode).
        if !ctx.global
            && let Some(project_id) = ctx.current_project_id
        {
            wheres.push("c.project_id = ?".to_string());
            values.push(Value::from(project_id));
        }

        // Outcome filters.
        if ctx.ok_only {
            wheres.push("s.fail_count = 0 AND s.success_count > 0".to_string());
        }
        if ctx.failed_only {
            wheres.push("s.fail_count > 0".to_string());
        }

        // Recency filter.  Mirrors the storage format exactly so text
        // comparison is lexically ordered.
        if let Some(days) = ctx.time_window_days {
            wheres
                .push("s.last_executed_at >= strftime('%Y-%m-%dT%H:%M:%SZ', 'now', ?)".to_string());
            values.push(Value::Text(format!("-{days} days")));
        }

        if !wheres.is_empty() {
            sql.push_str(" WHERE ");
            sql.push_str(&wheres.join(" AND "));
        }

        sql.push_str(" ORDER BY c.id LIMIT 500");

        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(values.iter()), |row| {
            let last_raw: Option<String> = row.get(5)?;
            let last_executed_at = last_raw
                .and_then(|s| chrono::DateTime::parse_from_rfc3339(&s).ok())
                .map(|dt| dt.with_timezone(&chrono::Utc));
            Ok(RawCandidate {
                command_id: row.get(0)?,
                cmd_string: row.get(1)?,
                project_id: row.get(2)?,
                success_count: row.get(3)?,
                fail_count: row.get(4)?,
                last_executed_at,
                bm25: row.get(6)?,
            })
        })?;

        rows.collect::<Result<Vec<_>, _>>().map_err(HsError::from)
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

    /// Resolve the internal project id for a project root path.
    ///
    /// Returns `None` if the project has never been seen (no commands
    /// captured under it yet). Used to establish search context.
    pub fn get_project_id_by_path(&self, path: &str) -> Result<Option<i64>, HsError> {
        use rusqlite::OptionalExtension;
        let conn = self.pool.get()?;
        let hash = dir_hash(path);
        let id = conn
            .query_row(
                "SELECT id FROM projects WHERE dir_hash = ?1",
                params![hash],
                |row| row.get(0),
            )
            .optional()?;
        Ok(id)
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

/// Quote each whitespace-separated token so FTS5 treats it as a literal
/// (AND'd) phrase term instead of parsing operators/column filters out of
/// user input. Embedded double quotes are escaped by doubling (FTS5 rule).
fn sanitize_fts_query(query: &str) -> String {
    query
        .split_whitespace()
        .map(|token| format!("\"{}\"", token.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::ranking::{SearchContext, rank_commands};

    /// Convenience: build a [`Store`] backed by an in-memory database
    /// with all migrations applied.
    fn test_store() -> Store {
        let pool = crate::db::tests::test_pool();
        Store::new(pool)
    }

    /// Resolve a single command id by string + owning project path,
    /// releasing the connection promptly (test pool is `max_size(1)`).
    fn command_id(store: &Store, string: &str, project: Option<&str>) -> i64 {
        let conn = store.pool.get().unwrap();
        match project {
            Some(path) => conn
                .query_row(
                    "SELECT c.id FROM commands c
                     JOIN projects p ON p.id = c.project_id
                     WHERE c.cmd_string = ?1 AND p.path = ?2",
                    [string, path],
                    |r| r.get(0),
                )
                .unwrap(),
            None => conn
                .query_row(
                    "SELECT id FROM commands WHERE cmd_string = ?1",
                    [string],
                    |r| r.get(0),
                )
                .unwrap(),
        }
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

    // ── Phase 6: recall & ranking acceptance tests ────────────────────

    /// Acceptance 1: identical command strings in two projects; the
    /// command from the *current* project (the successful one) must win
    /// a global search via the project boost.
    #[test]
    fn contextual_order_prefers_current_project() {
        let store = test_store();
        store
            .insert_execution(Some("/proj/a"), "deploy prod", 0, 100, "/proj/a")
            .unwrap();
        store
            .insert_execution(Some("/proj/b"), "deploy prod", 1, 100, "/proj/b")
            .unwrap();

        let conn = store.pool.get().unwrap();
        let project_a: i64 = conn
            .query_row("SELECT id FROM projects WHERE path = '/proj/a'", [], |r| {
                r.get(0)
            })
            .unwrap();
        drop(conn);

        let cmd_a = command_id(&store, "deploy prod", Some("/proj/a"));
        let cmd_b = command_id(&store, "deploy prod", Some("/proj/b"));

        let ctx = SearchContext {
            query: None,
            current_project_id: Some(project_a),
            global: true,
            ok_only: false,
            failed_only: false,
            time_window_days: None,
        };

        let candidates = store.fetch_candidates(&ctx).unwrap();
        assert_eq!(candidates.len(), 2, "both projects returned");

        let ranked = rank_commands(candidates, Some(project_a));
        assert_eq!(ranked[0].command_id, cmd_a, "current-project cmd first");
        assert_eq!(ranked[1].command_id, cmd_b);

        // Stats are surfaced for the UI.
        assert_eq!(ranked[0].success_count, 1);
        assert_eq!(ranked[0].fail_count, 0);
        assert_eq!(ranked[1].success_count, 0);
        assert_eq!(ranked[1].fail_count, 1);
    }

    /// Acceptance 2: in the same project, a reliable "deploy app" must
    /// crush an unreliable one.
    ///
    /// Note: identical strings dedupe to one command row per project, so
    /// the two aliases differ slightly.
    #[test]
    fn success_overrides_failure_rank() {
        let store = test_store();

        // "deploy app": 1 success, 5 failures (unreliable).
        store
            .insert_execution(Some("/proj"), "deploy app", 0, 100, "/proj")
            .unwrap();
        for _ in 0..5 {
            store
                .insert_execution(Some("/proj"), "deploy app", 1, 100, "/proj")
                .unwrap();
        }
        // "deploy app --prod": 5 successes, 0 failures (reliable).
        for _ in 0..5 {
            store
                .insert_execution(Some("/proj"), "deploy app --prod", 0, 100, "/proj")
                .unwrap();
        }

        let conn = store.pool.get().unwrap();
        let project: i64 = conn
            .query_row("SELECT id FROM projects WHERE path = '/proj'", [], |r| {
                r.get(0)
            })
            .unwrap();
        drop(conn);

        let reliable = command_id(&store, "deploy app --prod", Some("/proj"));
        let unreliable = command_id(&store, "deploy app", Some("/proj"));

        let ctx = SearchContext {
            query: None,
            current_project_id: Some(project),
            global: true,
            ok_only: false,
            failed_only: false,
            time_window_days: None,
        };

        let candidates = store.fetch_candidates(&ctx).unwrap();
        let ranked = rank_commands(candidates, Some(project));

        assert_eq!(ranked.len(), 2);
        assert_eq!(ranked[0].command_id, reliable);
        assert_eq!(ranked[1].command_id, unreliable);
        // 5/0 (× 1.2) vs 1/5 (× 0.5): must be a decisive gap, not a tie.
        assert!(
            ranked[0].final_score > ranked[1].final_score * 2.0,
            "reliable must score >2x unreliable: {} vs {}",
            ranked[0].final_score,
            ranked[1].final_score
        );
    }

    /// Acceptance 3: two reliable commands, one run 2h ago, one 100 days
    /// ago — recency must decide.
    #[test]
    fn recency_ranks_recent_command_first() {
        let store = test_store();

        store
            .insert_execution(Some("/proj"), "recent probe", 0, 100, "/proj")
            .unwrap();
        store
            .insert_execution(Some("/proj"), "stale probe", 0, 100, "/proj")
            .unwrap();
        // Equal run counts so the frequency multiplier cancels out.
        store
            .insert_execution(Some("/proj"), "recent probe", 0, 100, "/proj")
            .unwrap();
        store
            .insert_execution(Some("/proj"), "stale probe", 0, 100, "/proj")
            .unwrap();

        let project = {
            let conn = store.pool.get().unwrap();
            conn.query_row("SELECT id FROM projects WHERE path = '/proj'", [], |r| {
                r.get(0)
            })
            .unwrap()
        };
        let recent = command_id(&store, "recent probe", Some("/proj"));
        let stale = command_id(&store, "stale probe", Some("/proj"));

        {
            let conn = store.pool.get().unwrap();
            conn.execute(
                "UPDATE command_stats SET last_executed_at =
                     strftime('%Y-%m-%dT%H:%M:%SZ', 'now', '-2 hours')  WHERE command_id = ?1",
                [recent],
            )
            .unwrap();
            conn.execute(
                "UPDATE command_stats SET last_executed_at =
                     strftime('%Y-%m-%dT%H:%M:%SZ', 'now', '-100 days') WHERE command_id = ?1",
                [stale],
            )
            .unwrap();
        }

        let ctx = SearchContext {
            query: None,
            current_project_id: Some(project),
            global: true,
            ok_only: false,
            failed_only: false,
            time_window_days: None,
        };

        let candidates = store.fetch_candidates(&ctx).unwrap();
        assert_eq!(candidates.len(), 2);
        let ranked = rank_commands(candidates, Some(project));

        assert_eq!(ranked[0].command_id, recent);
        assert_eq!(ranked[1].command_id, stale);
        // Exact expected ratio: (1.5 / 0.8) × (freq identical) = 1.875.
        assert!(
            (ranked[0].final_score / ranked[1].final_score - 1.875).abs() < 1e-9,
            "recency ratio: {}",
            ranked[0].final_score / ranked[1].final_score
        );
    }

    /// FTS recall: query filtering narrows candidates and yields real
    /// (negative) bm25 values that the ranker inverts.
    #[test]
    fn query_filters_via_fts_and_scores() {
        let store = test_store();

        store
            .insert_execution(Some("/proj"), "cargo build", 0, 100, "/proj")
            .unwrap();
        store
            .insert_execution(Some("/proj"), "cargo test", 0, 100, "/proj")
            .unwrap();
        store
            .insert_execution(Some("/proj"), "make build", 0, 100, "/proj")
            .unwrap();

        let ctx = SearchContext {
            query: Some("cargo".to_string()),
            current_project_id: None,
            global: true,
            ok_only: false,
            failed_only: false,
            time_window_days: None,
        };

        let candidates = store.fetch_candidates(&ctx).unwrap();
        assert_eq!(candidates.len(), 2, "only 'cargo' rows should match");
        assert!(
            candidates.iter().all(|c| c.cmd_string.starts_with("cargo")),
            "FTS must filter to matching commands"
        );
        // Real FTS bm25 scores are negative; the sentinel is 1.0.
        assert!(
            candidates.iter().all(|c| c.bm25 <= 0.0),
            "bm25 should be negative for a real query"
        );

        let ranked = rank_commands(candidates, None);
        assert_eq!(ranked.len(), 2);
        assert!(ranked[0].final_score > 0.0);
    }

    /// Regression: raw user input full of FTS5 syntax (hyphens, colons,
    /// operators) must not be parsed as operators — it is quoted per token.
    #[test]
    fn query_with_fts_special_chars_is_quoted() {
        let store = test_store();
        store
            .insert_execution(
                Some("/proj"),
                "echo hello-from-hs-project --force",
                0,
                100,
                "/proj",
            )
            .unwrap();

        let ctx = SearchContext {
            query: Some("echo hello-from-hs-project  --force".to_string()),
            current_project_id: None,
            global: true,
            ok_only: false,
            failed_only: false,
            time_window_days: None,
        };

        let candidates = store.fetch_candidates(&ctx).unwrap();
        assert_eq!(
            candidates.len(),
            1,
            "hyphenated command must be found despite raw FTS5 syntax"
        );
        assert_eq!(
            candidates[0].cmd_string,
            "echo hello-from-hs-project --force"
        );
    }

    /// Hard filters: --ok / --failed narrow candidate sets correctly.
    #[test]
    fn outcome_filters_narrow_candidates() {
        let store = test_store();
        store
            .insert_execution(None, "good cmd", 0, 10, "/tmp")
            .unwrap();
        store
            .insert_execution(None, "bad cmd", 1, 10, "/tmp")
            .unwrap();

        let ok_ctx = SearchContext {
            query: None,
            current_project_id: None,
            global: true,
            ok_only: true,
            failed_only: false,
            time_window_days: None,
        };
        let ok = store.fetch_candidates(&ok_ctx).unwrap();
        assert_eq!(ok.len(), 1);
        assert_eq!(ok[0].cmd_string, "good cmd");

        let failed_ctx = SearchContext {
            query: None,
            current_project_id: None,
            global: true,
            ok_only: false,
            failed_only: true,
            time_window_days: None,
        };
        let failed = store.fetch_candidates(&failed_ctx).unwrap();
        assert_eq!(failed.len(), 1);
        assert_eq!(failed[0].cmd_string, "bad cmd");
    }

    /// Hard filters: non-global mode scopes to one project; --last
    /// trims by last_executed_at.
    #[test]
    fn project_and_time_window_filters() {
        let store = test_store();
        store
            .insert_execution(Some("/proj/a"), "from a", 0, 10, "/proj/a")
            .unwrap();
        store
            .insert_execution(Some("/proj/b"), "from b", 0, 10, "/proj/b")
            .unwrap();
        store
            .insert_execution(Some("/proj/b"), "fresh b", 0, 10, "/proj/b")
            .unwrap();

        let conn = store.pool.get().unwrap();
        let project_b: i64 = conn
            .query_row("SELECT id FROM projects WHERE path = '/proj/b'", [], |r| {
                r.get(0)
            })
            .unwrap();
        // Age "fresh b" instantly so the window can prune it.
        conn.execute(
            "UPDATE command_stats SET last_executed_at =
                 strftime('%Y-%m-%dT%H:%M:%SZ', 'now', '-8 days') WHERE command_id = (
                     SELECT id FROM commands WHERE cmd_string = 'fresh b'
                 )",
            [],
        )
        .unwrap();
        drop(conn);

        // Non-global scoping.
        let scoped = SearchContext {
            query: None,
            current_project_id: Some(project_b),
            global: false,
            ok_only: false,
            failed_only: false,
            time_window_days: None,
        };
        let scoped_cands = store.fetch_candidates(&scoped).unwrap();
        assert_eq!(scoped_cands.len(), 2, "only project B commands");

        // Same scope + a 3-day window excludes the 8-day-old command.
        let windowed = SearchContext {
            query: None,
            current_project_id: Some(project_b),
            global: false,
            ok_only: false,
            failed_only: false,
            time_window_days: Some(3),
        };
        let windowed_cands = store.fetch_candidates(&windowed).unwrap();
        assert_eq!(windowed_cands.len(), 1, "window prunes stale command");
        assert_eq!(windowed_cands[0].cmd_string, "from b");
    }
}
