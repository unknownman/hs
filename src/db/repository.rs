//! Repository-style API for the `hs` persistence layer.
//!
//! [`Store`] wraps a [`DbPool`](crate::db::DbPool) and provides
//! prepared-statement methods for all write and read operations.
//! Every public method acquires a single connection from the pool,
//! executes the operation, and returns the connection — keeping
//! latency low and pool contention minimal.

use chrono::{SecondsFormat, Utc};
use rusqlite::params;
use rusqlite::types::Value;

use crate::db::DbPool;
use crate::error::HsError;
use crate::models::{ImportEntry, ImportReport, PinnedCommand, cmd_hash, dir_hash};
use crate::ranking::{RawCandidate, SearchContext};
use crate::redaction::sanitize_command;

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
        let mut conn = self.pool.get()?;
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;

        // 1. Resolve or create the project.
        let project_id: Option<i64> = match project_path {
            Some(path) => {
                let hash = dir_hash(path);
                tx.execute(
                    "INSERT OR IGNORE INTO projects (dir_hash, path) VALUES (?1, ?2)",
                    params![hash, path],
                )?;
                let id: i64 = tx.query_row(
                    "SELECT id FROM projects WHERE dir_hash = ?1",
                    params![hash],
                    |row| row.get(0),
                )?;
                Some(id)
            }
            None => None,
        };

        // 2. Resolve or create the command.
        //
        // NOTE: SQLite UNIQUE indexes treat NULLs as distinct, so
        // `UNIQUE(cmd_hash, project_id)` does NOT dedup commands recorded
        // outside any project. We therefore check existence explicitly
        // (with `project_id IS ?`) before inserting, and reuse the id when
        // the command is already known. This keeps both capture and bulk
        // import idempotent for project-less commands.
        let c_hash = cmd_hash(cmd_string);
        let command_id = upsert_command(&tx, project_id, &c_hash, cmd_string)?;

        // 3. Insert the execution (triggers update command_stats).
        tx.execute(
            "INSERT INTO executions (command_id, exit_code, duration_ms, working_dir)
             VALUES (?1, ?2, ?3, ?4)",
            params![command_id, exit_code, duration_ms, cwd],
        )?;

        tx.commit()?;
        Ok(())
    }

    /// Bulk-import legacy shell history (Phase 8).
    ///
    /// Runs inside a **single SQLite transaction**: every command and
    /// execution row is inserted from one `Transaction`, which keeps a
    /// 20,000-line `.zsh_history` import well under a second instead of
    /// paying a fsync/commit per line.
    ///
    /// Privacy: every command passes through
    /// [`sanitize_command`](crate::redaction::sanitize_command) **before**
    /// hashing or persistence, so secrets never touch the database.
    ///
    /// Dedup: commands are keyed on `(cmd_hash, NULL project)`. A history
    /// line whose command already exists (from an earlier import or an
    /// earlier line in the same file) is counted as a duplicate and
    /// skipped entirely — making re-imports idempotent.
    ///
    /// Legacy history has no exit-code signal, so a neutral default
    /// (`exit_code = 0`) is recorded. Duration (in milliseconds) comes
    /// from zsh's `<elapsed>` header field when present; otherwise a
    /// neutral `0` is used. The original timestamp is recorded when the
    /// source format provided one.
    pub fn import_entries(
        &self,
        entries: Vec<ImportEntry>,
        working_dir: &str,
    ) -> Result<ImportReport, HsError> {
        let mut conn = self.pool.get()?;
        let tx = conn.transaction()?;

        // The insert loop lives in its own scope so the prepared statement
        // (which borrows `tx`) is dropped before `tx.commit()`.
        let report = {
            let mut insert_execution = tx.prepare(
                "INSERT INTO executions (command_id, exit_code, duration_ms, working_dir, executed_at)
                 VALUES (?1, 0, ?4, ?2, ?3)",
            )?;

            let mut report = ImportReport {
                imported: 0,
                duplicates: 0,
                redacted: 0,
            };

            for entry in entries {
                let clean = sanitize_command(&entry.cmd);
                if clean != entry.cmd {
                    report.redacted += 1;
                }
                if clean.trim().is_empty() {
                    continue;
                }

                let hash = cmd_hash(&clean);
                // Already present (earlier import / earlier line in this
                // file)? Skip — keeps re-imports idempotent.
                if command_id_for(&tx, &hash, None)?.is_some() {
                    report.duplicates += 1;
                    continue;
                }
                let command_id = upsert_command(&tx, None, &hash, &clean)?;
                let executed_at = entry
                    .executed_at
                    .unwrap_or_else(Utc::now)
                    .to_rfc3339_opts(SecondsFormat::Secs, true);
                insert_execution.execute(params![
                    command_id,
                    working_dir,
                    executed_at,
                    entry.duration_ms.unwrap_or(0),
                ])?;
                report.imported += 1;
            }

            report
        };

        tx.commit()?;
        Ok(report)
    }

    /// Total number of execution rows (used for the empty-state hint and
    /// `doctor` volume reporting).
    pub fn execution_count(&self) -> Result<i64, HsError> {
        let conn = self.pool.get()?;
        let n: i64 = conn.query_row("SELECT COUNT(*) FROM executions", [], |row| row.get(0))?;
        Ok(n)
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
                " bm25(commands_fts) AS bm,
                p.command_id IS NOT NULL AS is_pinned
                FROM commands c
                INNER JOIN commands_fts ON commands_fts.rowid = c.id
                LEFT JOIN command_stats s ON s.command_id = c.id
                LEFT JOIN pins p ON p.command_id = c.id",
            );
            match sanitize_fts_query(query) {
                Some(q) => {
                    wheres.push("commands_fts MATCH ?".to_string());
                    values.push(Value::Text(q));
                }
                // No searchable tokens (e.g. a query of `***`): the user
                // asked for nothing, so nothing can match.
                None => wheres.push("1 = 0".to_string()),
            }
        } else {
            sql.push_str(
                " 1.0 AS bm,
                p.command_id IS NOT NULL AS is_pinned
                FROM commands c
                LEFT JOIN command_stats s ON s.command_id = c.id
                LEFT JOIN pins p ON p.command_id = c.id",
            );
        }

        // Hard project filter (only active in non-global mode).
        if !ctx.global {
            if let Some(project_id) = ctx.current_project_id {
                wheres.push("c.project_id = ?".to_string());
                values.push(Value::from(project_id));
            } else {
                // `--project` was requested but this repo has no recorded
                // commands yet (`.git` exists, the `projects` table does
                // not). The only correct answer is "nothing matches" —
                // never silently widen to a global search.
                wheres.push("1 = 0".to_string());
            }
        }

        // Outcome filters.
        if ctx.ok_only {
            wheres.push("s.fail_count = 0 AND s.success_count > 0".to_string());
        }
        if ctx.failed_only {
            wheres.push("s.fail_count > 0".to_string());
        }

        // Recency filter. The modifier string is *parameterized* (bound
        // via `?`) so it is never interpolated into SQL — safe against
        // injection. `parse_time_window` only produces deterministic
        // strings from parsed integers, never raw user input.
        if let Some(ref modifier) = ctx.time_window {
            wheres
                .push("s.last_executed_at >= strftime('%Y-%m-%dT%H:%M:%SZ', 'now', ?)".to_string());
            values.push(Value::Text(modifier.clone()));
        }

        if !wheres.is_empty() {
            sql.push_str(" WHERE ");
            sql.push_str(&wheres.join(" AND "));
        }

        if ctx.query.is_some() {
            // bm25() returns negative values for better matches; ascending
            // sort places the most relevant text matches at the top.
            sql.push_str(" ORDER BY bm LIMIT 500");
        } else {
            // No query: surface pinned commands and most recent history first
            // so the Rust ranker receives the best candidates.
            sql.push_str(" ORDER BY is_pinned DESC, s.last_executed_at DESC LIMIT 500");
        }

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
                is_pinned: row.get::<_, bool>(7)?,
            })
        })?;

        rows.collect::<Result<Vec<_>, _>>().map_err(HsError::from)
    }

    /// Pin a command so it can be recalled later.
    ///
    /// Pinning is idempotent: re-pinning an already-pinned command is a
    /// no-op (the `pins` table uses the command id as its primary key).
    /// Returns `true` when the pin exists (inserted or already present),
    /// and `false` when no command with that id exists.
    pub fn pin_command(&self, command_id: i64) -> Result<bool, HsError> {
        let conn = self.pool.get()?;
        let exists: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM commands WHERE id = ?1)",
            params![command_id],
            |row| row.get(0),
        )?;
        if !exists {
            return Ok(false);
        }
        conn.execute(
            "INSERT OR IGNORE INTO pins (command_id) VALUES (?1)",
            params![command_id],
        )?;
        Ok(true)
    }

    /// Remove a pin from a previously pinned command.
    ///
    /// Unpinning an id that is not pinned is a no-op, so this can be
    /// called defensively without pre-checks.
    pub fn unpin_command(&self, command_id: i64) -> Result<(), HsError> {
        let conn = self.pool.get()?;
        conn.execute(
            "DELETE FROM pins WHERE command_id = ?1",
            params![command_id],
        )?;
        Ok(())
    }

    /// Permanently remove a command and everything attached to it.
    ///
    /// The privacy escape hatch: a leaked secret or a garbage typo can be
    /// erased from history outright. Returns `true` when a row was
    /// deleted, `false` when no command with that id exists.
    ///
    /// Deleting the `commands` row is all that is required: the schema's
    /// `ON DELETE CASCADE` foreign keys remove the `executions`, pins and
    /// stats, and the FTS5 triggers drop the search-index entry too.
    pub fn delete_command(&self, command_id: i64) -> Result<bool, HsError> {
        let conn = self.pool.get()?;
        let deleted = conn.execute("DELETE FROM commands WHERE id = ?1", params![command_id])?;
        Ok(deleted > 0)
    }

    /// List every pinned command, newest pin first.
    ///
    /// Joins the pinned command's string and (when present) its project
    /// root path. Project-less commands yield `project_path = None`. If a
    /// command is deleted, its pin is removed by the `ON DELETE CASCADE`
    /// foreign key, so it can never dangle here.
    pub fn list_pins(&self) -> Result<Vec<PinnedCommand>, HsError> {
        let conn = self.pool.get()?;
        let mut stmt = conn.prepare(
            "SELECT c.id, c.cmd_string, p.path, pins.pinned_at
             FROM pins
             JOIN commands c ON c.id = pins.command_id
             LEFT JOIN projects p ON p.id = c.project_id
             ORDER BY pins.pinned_at DESC, c.id",
        )?;
        let rows = stmt.query_map([], |row| {
            let pinned_raw: String = row.get(3)?;
            let pinned_at = chrono::DateTime::parse_from_rfc3339(&pinned_raw)
                .map(|dt| dt.with_timezone(&chrono::Utc))
                .unwrap_or_else(|_| chrono::Utc::now());
            Ok(PinnedCommand {
                command_id: row.get(0)?,
                cmd_string: row.get(1)?,
                project_path: row.get(2)?,
                pinned_at,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(HsError::from)
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
}

/// Look up the id of an existing command row for `(cmd_hash, project_id)`.
///
/// `project_id IS ?` (instead of `= ?`) matches `NULL` project ids too —
/// important since SQLite UNIQUE indexes treat `NULL`s as distinct, so
/// existence cannot be inferred from a failed insert.
fn command_id_for(
    conn: &rusqlite::Connection,
    cmd_hash: &str,
    project_id: Option<i64>,
) -> Result<Option<i64>, HsError> {
    use rusqlite::OptionalExtension;
    let id = conn
        .query_row(
            "SELECT id FROM commands WHERE cmd_hash = ?1 AND project_id IS ?2",
            params![cmd_hash, project_id],
            |row| row.get(0),
        )
        .optional()?;
    Ok(id)
}

/// Return the id of `(cmd_hash, project_id)`, inserting the command row
/// first if it does not exist. Null-safe (see [`command_id_for`]).
fn upsert_command(
    conn: &rusqlite::Connection,
    project_id: Option<i64>,
    cmd_hash: &str,
    cmd_string: &str,
) -> Result<i64, HsError> {
    if let Some(id) = command_id_for(conn, cmd_hash, project_id)? {
        return Ok(id);
    }
    // `INSERT OR IGNORE`: two hooks can race on the very first insert of a
    // command (e.g. a precmd that fires before the previous command's
    // capture finishes). Without it, one of them dies on the UNIQUE
    // index and the capture is silently dropped while aborting the
    // surrounding transaction.
    conn.execute(
        "INSERT OR IGNORE INTO commands (project_id, cmd_hash, cmd_string)
         VALUES (?1, ?2, ?3)",
        params![project_id, cmd_hash, cmd_string],
    )?;
    command_id_for(conn, cmd_hash, project_id)?
        .ok_or_else(|| HsError::from(rusqlite::Error::QueryReturnedNoRows))
}

// ── Tests ──────────────────────────────────────────────────────────────

/// Quote each searchable token so FTS5 treats it as a literal (AND'd)
/// phrase term instead of parsing operators out of user input.
///
/// Phase 8 hardening: instead of quoting raw chunks (which still leaks
/// FTS5 syntax — asterisks, colons, parens and unbalanced quotes can
/// produce parser errors or empty phrases), we extract only
/// tokenizer-producible runs (`[A-Za-z0-9_]`, matching SQLite's unicode61
/// tokenizer) and quote each one. FTS keywords (`AND`, `OR`, `NOT`) cease
/// to be operators because quoted terms are literal. Pure-punctuation
/// input yields `None`, which the caller turns into a no-match filter.
fn sanitize_fts_query(query: &str) -> Option<String> {
    let terms: Vec<String> = query
        .split_whitespace()
        .flat_map(|chunk| chunk.split(|c: char| !(c.is_alphanumeric() || c == '_')))
        .filter(|token| !token.is_empty())
        .map(|token| format!("\"{token}\""))
        .collect();

    if terms.is_empty() {
        None
    } else {
        Some(terms.join(" "))
    }
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
    fn pin_missing_command_returns_ok_false() {
        let store = test_store();
        let result = store.pin_command(999999);
        assert!(
            matches!(result, Ok(false)),
            "pinning a nonexistent command must degrade gracefully, got {result:?}"
        );
    }

    #[test]
    fn pin_is_idempotent() {
        let store = test_store();
        store
            .insert_execution(None, "cargo test", 0, 1000, "/proj")
            .unwrap();
        let id = command_id(&store, "cargo test", None);

        store.pin_command(id).unwrap();
        store.pin_command(id).unwrap();
        store.pin_command(id).unwrap();

        let conn = store.pool.get().unwrap();
        let count: i32 = conn
            .query_row("SELECT COUNT(*) FROM pins", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 1, "re-pinning must not duplicate the pin");
    }

    #[test]
    fn unpin_is_idempotent() {
        let store = test_store();
        store
            .insert_execution(Some("/proj"), "ls -la", 0, 10, "/proj")
            .unwrap();
        let id = command_id(&store, "ls -la", Some("/proj"));

        store.unpin_command(id).unwrap();
        store.unpin_command(id).unwrap();

        let conn = store.pool.get().unwrap();
        let count: i32 = conn
            .query_row("SELECT COUNT(*) FROM pins", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn list_pins_resolves_command_and_project_context() {
        let store = test_store();
        store
            .insert_execution(Some("/project-a"), "make build", 0, 100, "/project-a/src")
            .unwrap();
        store
            .insert_execution(None, "echo standalone", 0, 5, "/tmp")
            .unwrap();

        let proj_id = command_id(&store, "make build", Some("/project-a"));
        let bare_id = command_id(&store, "echo standalone", None);

        store.pin_command(proj_id).unwrap();
        store.pin_command(bare_id).unwrap();

        let pins = store.list_pins().unwrap();

        // Project-attributed pin carries its project path; the bare one is None.
        let proj_pin = pins.iter().find(|p| p.command_id == proj_id).unwrap();
        assert_eq!(proj_pin.cmd_string, "make build");
        assert_eq!(proj_pin.project_path.as_deref(), Some("/project-a"));
        assert!(proj_pin.pinned_at <= chrono::Utc::now());

        let bare_pin = pins.iter().find(|p| p.command_id == bare_id).unwrap();
        assert_eq!(bare_pin.cmd_string, "echo standalone");
        assert_eq!(bare_pin.project_path, None);
    }

    #[test]
    fn list_pins_orders_newest_first() {
        let store = test_store();
        store
            .insert_execution(Some("/older"), "old command", 0, 10, "/older")
            .unwrap();
        store
            .insert_execution(Some("/newer"), "new command", 0, 10, "/newer")
            .unwrap();
        let old_id = command_id(&store, "old command", Some("/older"));
        let new_id = command_id(&store, "new command", Some("/newer"));

        store.pin_command(old_id).unwrap();
        store.pin_command(new_id).unwrap();

        // Backdate the old pin so the ordering assertion is deterministic
        // even though both were inserted within the same UTC second.
        {
            let conn = store.pool.get().unwrap();
            conn.execute(
                "UPDATE pins SET pinned_at = '2020-01-01T00:00:00Z' WHERE command_id = ?1",
                params![old_id],
            )
            .unwrap();
        }

        let pins = store.list_pins().unwrap();
        assert_eq!(pins.len(), 2);
        assert_eq!(
            pins[0].command_id, new_id,
            "newest pin must be listed first"
        );
        assert_eq!(pins[1].command_id, old_id);
    }

    #[test]
    fn deleting_command_cascades_away_its_pin() {
        let store = test_store();
        store
            .insert_execution(None, "ephemeral", 0, 10, "/tmp")
            .unwrap();
        let id = command_id(&store, "ephemeral", None);
        store.pin_command(id).unwrap();

        {
            let conn = store.pool.get().unwrap();
            conn.execute("DELETE FROM commands WHERE id = ?1", params![id])
                .unwrap();
        }

        assert!(store.list_pins().unwrap().is_empty());
    }

    #[test]
    fn delete_command_removes_row_executions_stats_pins_and_fts() {
        let store = test_store();

        // A command with history, a pin, a stats entry and an FTS row.
        store
            .insert_execution(None, "docker login registry.example.com", 0, 10, "/tmp")
            .unwrap();
        store
            .insert_execution(None, "docker login registry.example.com", 1, 12, "/tmp")
            .unwrap();
        let id = command_id(&store, "docker login registry.example.com", None);
        store.pin_command(id).unwrap();

        // Sanity: the FTS index has the row before deletion.
        {
            let conn = store.pool.get().unwrap();
            let fts_before: i32 = conn
                .query_row(
                    "SELECT COUNT(*) FROM commands_fts WHERE rowid = ?1",
                    [id],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(fts_before, 1, "FTS row must exist before deletion");
        }

        assert!(
            store.delete_command(id).unwrap(),
            "delete_command must report true for an existing id"
        );

        let conn = store.pool.get().unwrap();
        let commands: i32 = conn
            .query_row("SELECT COUNT(*) FROM commands WHERE id = ?1", [id], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(commands, 0, "command row must be gone");

        let executions: i32 = conn
            .query_row(
                "SELECT COUNT(*) FROM executions WHERE command_id = ?1",
                [id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(executions, 0, "executions must cascade away");

        let stats: i32 = conn
            .query_row(
                "SELECT COUNT(*) FROM command_stats WHERE command_id = ?1",
                [id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(stats, 0, "command_stats must cascade away");

        let pins: i32 = conn
            .query_row(
                "SELECT COUNT(*) FROM pins WHERE command_id = ?1",
                [id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(pins, 0, "pins must cascade away");

        let fts: i32 = conn
            .query_row(
                "SELECT COUNT(*) FROM commands_fts WHERE rowid = ?1",
                [id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(fts, 0, "FTS delete trigger must drop the search entry");
        drop(conn);

        // Deleting an already-absent id reports false, not an error.
        assert!(
            !store.delete_command(id).unwrap(),
            "second delete of the same id must report false"
        );

        // And the command no longer surfaces in a search.
        let ctx = SearchContext {
            query: Some("docker login".to_string()),
            current_project_id: None,
            global: true,
            ok_only: false,
            failed_only: false,
            time_window: None,
        };
        assert!(
            store.fetch_candidates(&ctx).unwrap().is_empty(),
            "deleted command must vanish from search results"
        );
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
            time_window: None,
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
            time_window: None,
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
            time_window: None,
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
            time_window: None,
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
            time_window: None,
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

    /// Phase 8 FTS hardening: quotes, asterisks, colons, and the FTS
    /// keywords `AND`/`OR`/`NOT` must never become operators or errors.
    #[test]
    fn sanitize_fts_query_handles_hostile_input() {
        // Unbalanced quote → the quote char is dropped, words survive.
        assert_eq!(
            sanitize_fts_query("redis \"hgetall"),
            Some("\"redis\" \"hgetall\"".to_string())
        );
        // Asterisks/colons are separators, keywords become literal terms.
        assert_eq!(
            sanitize_fts_query("docker:* AND OR NOT (build)"),
            Some("\"docker\" \"AND\" \"OR\" \"NOT\" \"build\"".to_string())
        );
        // URL-ish input: scheme + host survive as separate terms.
        assert_eq!(
            sanitize_fts_query("curl https://api.example.com/health"),
            Some("\"curl\" \"https\" \"api\" \"example\" \"com\" \"health\"".to_string())
        );
        // Pure punctuation has no searchable tokens.
        assert_eq!(sanitize_fts_query("*** ::: \" )"), None);
    }

    /// A punctuation-only query must yield zero rows, not an FTS5 error.
    #[test]
    fn punctuation_only_query_returns_nothing() {
        let store = test_store();
        store
            .insert_execution(None, "cargo build", 0, 10, "/tmp")
            .unwrap();

        let ctx = SearchContext {
            query: Some("*** :: \"".to_string()),
            current_project_id: None,
            global: true,
            ok_only: false,
            failed_only: false,
            time_window: None,
        };
        let candidates = store.fetch_candidates(&ctx).unwrap();
        assert!(candidates.is_empty(), "no tokens → no candidates, no crash");
    }

    /// Phase 8 acceptance: bulk import runs atomically, redacts secrets,
    /// preserves timestamps, and dedups on re-import.
    #[test]
    fn bulk_import_redacts_preserves_timestamps_and_is_idempotent() {
        use chrono::DateTime as ChDateTime;

        let store = test_store();

        let secret = format!("AKIA{}", "A".repeat(16));
        let entries = vec![
            crate::models::ImportEntry {
                cmd: format!("export AWS_ACCESS_KEY_ID={secret}"),
                executed_at: ChDateTime::parse_from_rfc3339("2022-01-01T00:00:00Z")
                    .map(|dt| dt.with_timezone(&chrono::Utc))
                    .ok(),
                duration_ms: Some(1200),
            },
            crate::models::ImportEntry {
                cmd: "npm install \\\nlodash".to_string(), // multiline zsh entry
                executed_at: ChDateTime::parse_from_rfc3339("2022-06-15T12:30:00Z")
                    .map(|dt| dt.with_timezone(&chrono::Utc))
                    .ok(),
                duration_ms: None,
            },
            crate::models::ImportEntry {
                cmd: "echo no-timestamp".to_string(),
                executed_at: None,
                duration_ms: None,
            },
        ];

        let first = store.import_entries(entries, "/home/user").unwrap();
        assert_eq!(first.imported, 3);
        assert_eq!(first.redacted, 1, "the API key command must be counted");
        assert_eq!(first.duplicates, 0);

        let stored: Vec<(String, String, i64)> = {
            // max_size(1) pool: hold the connection only inside this
            // scope so later `Store` calls can also borrow it.
            let conn = store.pool.get().expect("pool get failed");
            let mut stmt = conn
                .prepare(
                    "SELECT c.cmd_string, e.executed_at, e.duration_ms
                     FROM commands c
                     JOIN executions e ON e.command_id = c.id
                     WHERE c.project_id IS NULL
                     ORDER BY e.executed_at",
                )
                .unwrap();
            let rows = stmt
                .query_map([], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, i64>(2)?,
                    ))
                })
                .unwrap();
            rows.filter_map(Result::ok).collect()
        };

        assert_eq!(stored.len(), 3);
        // Timestamps survive the round-trip (RFC3339 text form).
        assert_eq!(stored[0].1, "2022-01-01T00:00:00Z");
        assert_eq!(stored[1].1, "2022-06-15T12:30:00Z");
        // The zsh elapsed field (seconds→ms) survives the round-trip.
        assert_eq!(stored[0].2, 1200, "duration_ms must be persisted");
        assert_eq!(stored[1].2, 0, "missing duration imports as 0");
        assert_eq!(stored[2].2, 0);

        // The secret must never reach the database.
        assert!(
            stored[0].0.contains("[REDACTED]") && !stored[0].0.contains(&secret),
            "API key must be redacted before persistence"
        );

        // Multiline command preserved verbatim.
        assert_eq!(stored[1].0, "npm install \\\nlodash");

        // Re-import of the same lines must not create new rows.
        let same = vec![
            crate::models::ImportEntry {
                cmd: format!("export AWS_ACCESS_KEY_ID={secret}"),
                executed_at: None,
                duration_ms: None,
            },
            crate::models::ImportEntry {
                cmd: "npm install \\\nlodash".to_string(),
                executed_at: None,
                duration_ms: None,
            },
            crate::models::ImportEntry {
                cmd: "echo no-timestamp".to_string(),
                executed_at: None,
                duration_ms: None,
            },
        ];
        let second = store.import_entries(same, "/home/user").unwrap();
        assert_eq!(second.imported, 0, "no new rows on re-import");
        assert_eq!(second.duplicates, 3, "all three already existed");
        assert_eq!(
            store.execution_count().unwrap(),
            3,
            "total executions must be unchanged"
        );
    }

    /// Regression: `hs import` ingests *old* history, and its backdated
    /// timestamps must never downgrade the recency of a command the user
    /// is actively running. `last_executed_at` only moves forward.
    #[test]
    fn importing_old_history_does_not_downgrade_recency() {
        let store = test_store();

        // 1. A fresh execution for the command (executed_at = "now").
        store
            .insert_execution(Some("/proj"), "deploy app", 0, 100, "/proj")
            .unwrap();
        let command_id = command_id(&store, "deploy app", Some("/proj"));

        let fresh: String = {
            let conn = store.pool.get().unwrap();
            conn.query_row(
                "SELECT last_executed_at FROM command_stats WHERE command_id = ?1",
                [command_id],
                |row| row.get(0),
            )
            .unwrap()
        };

        // 2. An OLD execution lands (e.g. a 2020 line from the legacy
        //    history file) — the same pathway `import_entries` uses.
        {
            let conn = store.pool.get().unwrap();
            conn.execute(
                "INSERT INTO executions
                     (command_id, exit_code, duration_ms, working_dir, executed_at)
                 VALUES (?1, 0, 50, '/proj', '2020-01-01T00:00:00Z')",
                [command_id],
            )
            .unwrap();
        }

        let after_old: String = {
            let conn = store.pool.get().unwrap();
            conn.query_row(
                "SELECT last_executed_at FROM command_stats WHERE command_id = ?1",
                [command_id],
                |row| row.get(0),
            )
            .unwrap()
        };
        assert_eq!(
            after_old, fresh,
            "an older import must not downgrade last_executed_at"
        );

        // 3. A genuinely newer execution must still advance the clock.
        {
            let conn = store.pool.get().unwrap();
            conn.execute(
                "INSERT INTO executions
                     (command_id, exit_code, duration_ms, working_dir, executed_at)
                 VALUES (?1, 0, 50, '/proj', '2100-01-01T00:00:00Z')",
                [command_id],
            )
            .unwrap();
        }

        let after_new: String = {
            let conn = store.pool.get().unwrap();
            conn.query_row(
                "SELECT last_executed_at FROM command_stats WHERE command_id = ?1",
                [command_id],
                |row| row.get(0),
            )
            .unwrap()
        };
        assert_eq!(after_new, "2100-01-01T00:00:00Z");
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
            time_window: None,
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
            time_window: None,
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
            time_window: None,
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
            time_window: Some("-3 days".to_string()),
        };
        let windowed_cands = store.fetch_candidates(&windowed).unwrap();
        assert_eq!(windowed_cands.len(), 1, "window prunes stale command");
        assert_eq!(windowed_cands[0].cmd_string, "from b");
    }

    /// Acceptance: sub-day precision. A command run 45 minutes ago is
    /// excluded by `--last 30m` (modifier `-30 minutes`) but included by
    /// `--last 1h` (modifier `-1 hours`).
    #[test]
    fn sub_day_time_window_prunes_and_includes_precisely() {
        let store = test_store();

        // Insert a command, then backdate its stats to 45 minutes ago.
        store
            .insert_execution(Some("/proj"), "mid recency probe", 0, 10, "/proj")
            .unwrap();
        let id = command_id(&store, "mid recency probe", Some("/proj"));
        {
            let conn = store.pool.get().unwrap();
            conn.execute(
                "UPDATE command_stats SET last_executed_at =
                     strftime('%Y-%m-%dT%H:%M:%SZ', 'now', '-45 minutes') WHERE command_id = ?1",
                [id],
            )
            .unwrap();
        }

        // 30-minute window must EXCLUDE a 45-minute-old command.
        let short = SearchContext {
            query: None,
            current_project_id: None,
            global: true,
            ok_only: false,
            failed_only: false,
            time_window: Some("-30 minutes".to_string()),
        };
        let short_cands = store.fetch_candidates(&short).unwrap();
        assert!(
            short_cands.is_empty(),
            "-30 minutes must exclude a 45-minute-old command"
        );

        // 1-hour window must INCLUDE a 45-minute-old command.
        let hour = SearchContext {
            query: None,
            current_project_id: None,
            global: true,
            ok_only: false,
            failed_only: false,
            time_window: Some("-1 hours".to_string()),
        };
        let hour_cands = store.fetch_candidates(&hour).unwrap();
        assert_eq!(hour_cands.len(), 1, "-1 hours must include it");
        assert_eq!(hour_cands[0].cmd_string, "mid recency probe");
    }

    /// Acceptance: a pinned command with 0 historical runs outranks an
    /// unpinned command with 50 runs. Proves the pin boost is applied
    /// end-to-end (through fetch → rank).
    #[test]
    fn pinned_tracks_surface_through_fetch_and_rank() {
        let store = test_store();

        // One command with 50 successful runs, one with zero runs.
        for _ in 0..50 {
            store
                .insert_execution(Some("/proj"), "popular command", 0, 10, "/proj")
                .unwrap();
        }
        store
            .insert_execution(Some("/proj"), "pinned alone", 0, 10, "/proj")
            .unwrap();
        store
            .insert_execution(Some("/proj"), "pinned alone", 1, 10, "/proj")
            .unwrap();

        let popular_id = command_id(&store, "popular command", Some("/proj"));
        let pinned_id = command_id(&store, "pinned alone", Some("/proj"));

        // Pin the second (recent, but 1 run / 1 failure).
        store.pin_command(pinned_id).unwrap();

        let ctx = SearchContext {
            query: None,
            current_project_id: None,
            global: true,
            ok_only: false,
            failed_only: false,
            time_window: None,
        };

        let candidates = store.fetch_candidates(&ctx).unwrap();
        assert_eq!(candidates.len(), 2);
        let pinned_candidate = candidates
            .iter()
            .find(|c| c.command_id == pinned_id)
            .expect("pinned command must be fetched");
        assert!(
            pinned_candidate.is_pinned,
            "fetch_candidates must mark the pinned row"
        );
        let popular_candidate = candidates
            .iter()
            .find(|c| c.command_id == popular_id)
            .unwrap();
        assert!(
            !popular_candidate.is_pinned,
            "unpinned row must not be flagged"
        );

        let ranked = rank_commands(candidates, None);
        assert_eq!(ranked[0].command_id, pinned_id, "pinned must rank first");
        assert!(ranked[0].is_pinned);
        assert_eq!(ranked[1].command_id, popular_id);
        assert!(!ranked[1].is_pinned);
    }

    /// Full pins lifecycle: insert → pin → list_pins returns it → rank
    /// boosts to index 0 → unpin → list_pins empty → rank drops to
    /// normal position.
    #[test]
    fn pins_lifecycle_insert_pin_list_rank_unpin() {
        let store = test_store();

        // Two commands in the same project: "alpha" and "beta".
        // Both have one successful run so the only ranking signal is
        // recency (set deterministically below).
        store
            .insert_execution(Some("/proj"), "alpha cmd", 0, 10, "/proj")
            .unwrap();
        store
            .insert_execution(Some("/proj"), "beta cmd", 0, 10, "/proj")
            .unwrap();

        let alpha_id = command_id(&store, "alpha cmd", Some("/proj"));
        let _beta_id = command_id(&store, "beta cmd", Some("/proj"));

        // Backdate both to the same moment so recency is identical.
        {
            let conn = store.pool.get().unwrap();
            conn.execute(
                "UPDATE command_stats SET last_executed_at = '2025-01-01T00:00:00Z'",
                [],
            )
            .unwrap();
        }

        // ── Step 1: nothing pinned yet ──────────────────────────────
        let ctx = SearchContext {
            query: None,
            current_project_id: None,
            global: true,
            ok_only: false,
            failed_only: false,
            time_window: None,
        };
        let ranked = rank_commands(store.fetch_candidates(&ctx).unwrap(), None);
        assert_eq!(ranked.len(), 2, "both commands returned");
        // Same score → same sort position; neither is pinned.
        assert!(ranked.iter().all(|r| !r.is_pinned));

        // ── Step 2: pin "alpha" ─────────────────────────────────────
        store.pin_command(alpha_id).unwrap();
        let pins = store.list_pins().unwrap();
        assert_eq!(pins.len(), 1);
        assert_eq!(pins[0].command_id, alpha_id);
        assert_eq!(pins[0].cmd_string, "alpha cmd");

        let ranked = rank_commands(store.fetch_candidates(&ctx).unwrap(), None);
        assert_eq!(ranked[0].command_id, alpha_id, "pinned alpha must be first");
        assert!(ranked[0].is_pinned);

        // ── Step 3: unpin "alpha" ───────────────────────────────────
        store.unpin_command(alpha_id).unwrap();
        let pins = store.list_pins().unwrap();
        assert!(pins.is_empty(), "no pins after unpin");

        let ranked = rank_commands(store.fetch_candidates(&ctx).unwrap(), None);
        assert_eq!(ranked.len(), 2);
        assert!(
            ranked.iter().all(|r| !r.is_pinned),
            "neither command pinned after unpin"
        );
    }

    /// Sub-day window boundary: create two commands, backdate one to
    /// `now - 20 minutes` and the other to `now - 2 hours`. A 30-minute
    /// window must return only the recent one.
    #[test]
    fn sub_day_window_two_commands_boundary() {
        let store = test_store();

        store
            .insert_execution(Some("/proj"), "fresh cmd", 0, 10, "/proj")
            .unwrap();
        store
            .insert_execution(Some("/proj"), "stale cmd", 0, 10, "/proj")
            .unwrap();

        let fresh_id = command_id(&store, "fresh cmd", Some("/proj"));
        let stale_id = command_id(&store, "stale cmd", Some("/proj"));

        // Backdate to precise ages.
        {
            let conn = store.pool.get().unwrap();
            conn.execute(
                "UPDATE command_stats SET last_executed_at =
                     strftime('%Y-%m-%dT%H:%M:%SZ', 'now', '-20 minutes') WHERE command_id = ?1",
                [fresh_id],
            )
            .unwrap();
            conn.execute(
                "UPDATE command_stats SET last_executed_at =
                     strftime('%Y-%m-%dT%H:%M:%SZ', 'now', '-2 hours') WHERE command_id = ?1",
                [stale_id],
            )
            .unwrap();
        }

        // 30-minute window: only "fresh cmd" survives.
        let ctx = SearchContext {
            query: None,
            current_project_id: None,
            global: true,
            ok_only: false,
            failed_only: false,
            time_window: Some("-30 minutes".to_string()),
        };
        let cands = store.fetch_candidates(&ctx).unwrap();
        assert_eq!(cands.len(), 1, "only the 20-min-old command must survive");
        assert_eq!(cands[0].cmd_string, "fresh cmd");

        // Wider 3-hour window: both survive.
        let ctx_wide = SearchContext {
            query: None,
            current_project_id: None,
            global: true,
            ok_only: false,
            failed_only: false,
            time_window: Some("-3 hours".to_string()),
        };
        let cands = store.fetch_candidates(&ctx_wide).unwrap();
        assert_eq!(cands.len(), 2, "both survive a 3-hour window");
    }

    /// No query: `fetch_candidates` must surface recent commands over
    /// ancient ones (`ORDER BY s.last_executed_at DESC`), instead of the
    /// old `ORDER BY c.id` which returned the 500 oldest commands.
    #[test]
    fn no_query_fetches_recent_over_old_commands() {
        let store = test_store();

        store
            .insert_execution(Some("/proj"), "ancient command", 0, 10, "/proj")
            .unwrap();
        store
            .insert_execution(Some("/proj"), "recent command", 0, 10, "/proj")
            .unwrap();
        let ancient_id = command_id(&store, "ancient command", Some("/proj"));
        let recent_id = command_id(&store, "recent command", Some("/proj"));

        // Backdate the *first-inserted* command so id order and recency
        // order disagree: id says "ancient" = 1, recency says "recent" first.
        {
            let conn = store.pool.get().unwrap();
            conn.execute(
                "UPDATE command_stats SET last_executed_at =
                     '2020-01-01T00:00:00Z' WHERE command_id = ?1",
                [ancient_id],
            )
            .unwrap();
        }

        let ctx = SearchContext {
            query: None,
            current_project_id: None,
            global: true,
            ok_only: false,
            failed_only: false,
            time_window: None,
        };
        let candidates = store.fetch_candidates(&ctx).unwrap();
        assert_eq!(candidates.len(), 2);
        assert_eq!(
            candidates[0].command_id, recent_id,
            "recent command must be fetched before the ancient one"
        );
        assert_eq!(candidates[1].command_id, ancient_id);
    }

    /// No query: pinned commands outrank unpinned ones regardless of how
    /// stale they are (`ORDER BY is_pinned DESC`).
    #[test]
    fn no_query_pins_sort_first_even_when_stale() {
        let store = test_store();

        store
            .insert_execution(Some("/proj"), "stale pin", 0, 10, "/proj")
            .unwrap();
        store
            .insert_execution(Some("/proj"), "fresh plain", 0, 10, "/proj")
            .unwrap();
        let pinned_id = command_id(&store, "stale pin", Some("/proj"));
        let _fresh_id = command_id(&store, "fresh plain", Some("/proj"));

        store.pin_command(pinned_id).unwrap();
        {
            let conn = store.pool.get().unwrap();
            conn.execute(
                "UPDATE command_stats SET last_executed_at =
                     '2020-01-01T00:00:00Z' WHERE command_id = ?1",
                [pinned_id],
            )
            .unwrap();
        }

        let ctx = SearchContext {
            query: None,
            current_project_id: None,
            global: true,
            ok_only: false,
            failed_only: false,
            time_window: None,
        };
        let candidates = store.fetch_candidates(&ctx).unwrap();
        assert_eq!(candidates.len(), 2);
        assert!(
            candidates[0].is_pinned,
            "pinned command must sort first despite being stale"
        );
        assert_eq!(candidates[0].command_id, pinned_id);
        assert!(!candidates[1].is_pinned);
    }

    /// With a query, bm25 relevance — not `c.id` insertion order — drives
    /// the candidate ordering. Commands are inserted worst-match-first so
    /// the resulting order can only come from `ORDER BY bm`.
    #[test]
    fn query_ordering_is_driven_by_bm25_relevance() {
        let store = test_store();

        // Worst match (longest doc, "vim" diluted by noise) inserted FIRST,
        // best match (shortest doc) inserted LAST.
        store
            .insert_execution(
                Some("/proj"),
                "vim ./README.md ./src/main.rs ./src/lib.rs ./src/util.rs \
                 ./src/build.rs ./docs/design.md ./docs/guide.md ./docs/api.md",
                0,
                10,
                "/proj",
            )
            .unwrap();
        store
            .insert_execution(
                Some("/proj"),
                "vim ./src/main.rs ./src/lib.rs",
                0,
                10,
                "/proj",
            )
            .unwrap();
        store
            .insert_execution(Some("/proj"), "vim ./main.rs", 0, 10, "/proj")
            .unwrap();

        let best_id = command_id(&store, "vim ./main.rs", Some("/proj"));
        let worst_id = command_id(
            &store,
            "vim ./README.md ./src/main.rs ./src/lib.rs ./src/util.rs \
                 ./src/build.rs ./docs/design.md ./docs/guide.md ./docs/api.md",
            Some("/proj"),
        );

        let ctx = SearchContext {
            query: Some("vim".to_string()),
            current_project_id: None,
            global: true,
            ok_only: false,
            failed_only: false,
            time_window: None,
        };
        let candidates = store.fetch_candidates(&ctx).unwrap();
        assert_eq!(candidates.len(), 3);
        assert!(
            candidates.windows(2).all(|w| w[0].bm25 <= w[1].bm25),
            "candidates must be sorted by ascending bm25 (better matches first)"
        );
        assert_eq!(
            candidates[0].command_id, best_id,
            "most relevant match must rank first"
        );
        assert_ne!(
            candidates.last().unwrap().command_id,
            best_id,
            "best match must not trail the pack"
        );
        assert_eq!(
            candidates.last().unwrap().command_id,
            worst_id,
            "diluted command must be the worst match"
        );
    }
}
