//! SQLite storage engine with WAL mode, connection pooling, and FTS5.
//!
//! # Concurrency Model
//!
//! Multiple shell tabs may capture commands simultaneously.  The pool
//! uses [`r2d2_sqlite::SqliteConnectionManager`] configured with
//! `PRAGMA journal_mode = WAL` and `PRAGMA synchronous = NORMAL`,
//! which allows concurrent readers and a single serialized writer
//! without `SQLITE_BUSY` errors under normal load.
//!
//! # Schema Versioning
//!
//! Migrations are tracked via SQLite's built-in `PRAGMA user_version`.
//! Each migration increments the version by one.  The runner checks the
//! current version and applies only the migrations that have not yet run.

pub mod migrations;

use std::path::Path;

use r2d2_sqlite::SqliteConnectionManager;
use r2d2_sqlite::rusqlite::Connection;

use crate::error::HsError;

/// Type alias for the pooled SQLite connection.
pub type DbPool = r2d2::Pool<SqliteConnectionManager>;

/// Initialize the database at `db_path`, applying all pending migrations.
///
/// If the file does not exist it will be created.  WAL mode and foreign
/// keys are enforced on every connection that leaves the pool.
///
/// # Errors
///
/// Returns [`HsError`] if the connection pool cannot be created or if
/// any migration fails.
pub fn init_db(db_path: &Path) -> Result<DbPool, HsError> {
    // Ensure parent directory exists so SQLite can create the file.
    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent).map_err(|source| HsError::CreateDataDir {
            path: parent.to_path_buf(),
            source,
        })?;
    }

    let manager = SqliteConnectionManager::file(db_path).with_init(customize_connection);

    let pool = r2d2::Pool::builder().max_size(4).build(manager)?;

    // Apply migrations using a dedicated connection from the pool.
    {
        let conn = pool.get()?;
        migrations::run(&conn)?;
    }

    Ok(pool)
}

/// Per-connection initialization hook.
///
/// Executed once when a connection is created (or recycled after an
/// error).  These PRAGMAs are **required** for correct concurrent
/// behavior.
fn customize_connection(conn: &mut Connection) -> Result<(), r2d2_sqlite::rusqlite::Error> {
    conn.execute_batch(
        "PRAGMA journal_mode = WAL;
         PRAGMA synchronous  = NORMAL;
         PRAGMA foreign_keys = ON;",
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build an in-memory pool with all migrations applied.
    ///
    /// Note: in-memory SQLite databases cannot use WAL mode — this is
    /// expected.  The `db_init_with_real_file` test covers WAL.
    fn test_pool() -> DbPool {
        let manager = SqliteConnectionManager::memory().with_init(customize_connection);

        let pool = r2d2::Pool::builder()
            .max_size(1)
            .build(manager)
            .expect("failed to build test pool");

        // Apply schema migrations.
        let conn = pool.get().expect("failed to get connection");
        migrations::run(&conn).expect("migrations failed");

        pool
    }

    #[test]
    fn pool_initializes_with_foreign_keys() {
        let pool = test_pool();
        let conn = pool.get().expect("failed to get connection");

        let fk: i32 = conn
            .query_row("PRAGMA foreign_keys", [], |row| row.get(0))
            .expect("failed to query foreign_keys");

        assert_eq!(fk, 1, "foreign keys should be enabled");
    }

    #[test]
    fn migrations_apply_clean_schema() {
        let pool = test_pool();
        let conn = pool.get().expect("failed to get connection");

        let objects: Vec<String> = {
            let mut stmt = conn
                .prepare(
                    "SELECT name FROM sqlite_master \
                     WHERE type IN ('table','trigger') ORDER BY name",
                )
                .unwrap();
            let rows = stmt.query_map([], |row| row.get(0)).unwrap();
            rows.filter_map(Result::ok).collect()
        };

        assert!(objects.contains(&"commands".to_string()));
        assert!(objects.contains(&"executions".to_string()));
        assert!(objects.contains(&"commands_ai".to_string()));
        assert!(objects.contains(&"commands_ad".to_string()));
        assert!(objects.contains(&"commands_au".to_string()));
    }

    #[test]
    fn foreign_key_constraint_enforced() {
        let pool = test_pool();
        let conn = pool.get().expect("failed to get connection");

        conn.execute(
            "INSERT INTO commands (cmd_string, project_hash) VALUES ('ls', NULL)",
            [],
        )
        .expect("failed to insert command");

        let result = conn.execute(
            "INSERT INTO executions (command_id, working_dir) VALUES (999999, '/tmp')",
            [],
        );

        assert!(result.is_err(), "FK violation should have been rejected");
    }

    #[test]
    fn fts5_insert_and_search() {
        let pool = test_pool();
        let conn = pool.get().expect("failed to get connection");

        conn.execute(
            "INSERT INTO commands (cmd_string, project_hash) \
             VALUES ('cargo build --release', NULL)",
            [],
        )
        .expect("failed to insert");

        let count: i32 = conn
            .query_row(
                "SELECT COUNT(*) FROM commands WHERE id IN (
                    SELECT rowid FROM commands_fts WHERE commands_fts MATCH 'build'
                )",
                [],
                |row| row.get(0),
            )
            .expect("FTS5 query failed");

        assert_eq!(count, 1, "FTS5 should find the 'build' token");
    }

    #[test]
    fn db_init_with_real_file() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("hs.db");

        let pool = init_db(&db_path).expect("init_db failed");

        assert!(db_path.exists(), "database file should exist on disk");

        // WAL mode is only available on real files — verify here.
        let conn = pool.get().unwrap();
        let mode: String = conn
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .unwrap();
        assert_eq!(mode, "wal");
    }
}
