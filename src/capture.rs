//! Capture pipeline — turns a raw shell command into a stored execution.
//!
//! This is the orchestrator behind the hidden `hs capture` subcommand,
//! called by the bash/zsh hooks once a command has finished. The whole
//! pipeline is deliberately synchronous and cheap (target <50ms), and
//! the hooks background it so the prompt is never blocked.
//!
//! Pipeline (in order):
//!   1. Drop empty commands early.
//!   2. `redaction::sanitize_command` — scrub secrets before persistence.
//!   3. `context::find_project_root` — resolve the owning project (for
//!      project-scoped recall and stack tags).
//!   4. `Store::insert_execution` — upsert project + command, append the
//!      execution row, update stats.

use std::path::Path;

use crate::context::find_project_root;
use crate::db::DbPool;
use crate::db::repository::Store;
use crate::error::HsError;
use crate::redaction::sanitize_command;

/// Process a single completed command execution.
///
/// Best-effort by design: the shell hooks run this detached, so any
/// error is simply surfaced via `Result` and never reaches the user's
/// terminal. Empty/whitespace-only commands are ignored entirely.
pub fn process_capture(
    pool: &DbPool,
    cmd: &str,
    cwd: &Path,
    exit_code: i32,
    duration_ms: i64,
) -> Result<(), HsError> {
    if cmd.trim().is_empty() {
        return Ok(());
    }

    let clean = sanitize_command(cmd);
    let project_root = find_project_root(cwd);
    let project_path = project_root.as_deref().and_then(|p| p.to_str());

    let store = Store::new(pool.clone());
    store.insert_execution(
        project_path,
        &clean,
        exit_code,
        duration_ms,
        cwd.to_str().unwrap_or(""),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    use crate::db::tests::test_pool;

    #[test]
    fn captures_redacts_and_projects() {
        let pool = test_pool();
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("src")).unwrap();
        fs::create_dir_all(dir.path().join(".git")).unwrap();
        fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"demo\"\n",
        )
        .unwrap();
        fs::write(dir.path().join(".git/config"), "[core]\n").unwrap();

        let start_len = {
            let conn = pool.get().unwrap();
            let n: i64 = conn
                .query_row("SELECT COUNT(*) FROM executions", [], |r| r.get(0))
                .unwrap();
            n
        };

        let secret = format!("export AWS_ACCESS_KEY_ID={}", "AKIA0123456789ABCDEF");
        process_capture(&pool, &secret, &dir.path().join("src"), 1, 250).unwrap();

        let conn = pool.get().unwrap();
        let end_len: i64 = conn
            .query_row("SELECT COUNT(*) FROM executions", [], |r| r.get(0))
            .unwrap();
        assert_eq!(end_len, start_len + 1, "one execution row must be added");

        // The stored command string must never contain the secret.
        let stored: String = conn
            .query_row(
                "SELECT cmd.cmd_string
                 FROM commands cmd
                 JOIN executions ex ON ex.command_id = cmd.id
                 WHERE ex.exit_code = 1 AND ex.duration_ms = 250",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(
            !stored.contains("AKIA0123456789ABCDEF"),
            "secret must be redacted, got: {stored}"
        );
        assert!(stored.contains("AWS_ACCESS_KEY_ID"), "marker must survive");

        // The project root must have been detected and persisted.
        let projects: Vec<String> = conn
            .prepare("SELECT path FROM projects")
            .unwrap()
            .query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(projects.len(), 1, "one project row expected");
        assert_eq!(projects[0], dir.path().to_str().unwrap());
    }

    #[test]
    fn ignores_empty_commands() {
        let pool = test_pool();
        let dir = tempfile::tempdir().unwrap();

        for cmd in ["", "   ", "\t\n"] {
            process_capture(&pool, cmd, dir.path(), 0, 0).unwrap();
        }

        let conn = pool.get().unwrap();
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM executions", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0, "no executions for empty commands");
    }

    #[test]
    fn stores_execution_without_project() {
        let pool = test_pool();
        // A temp dir outside any git/etc. project marker.
        let dir = tempfile::tempdir().unwrap();

        process_capture(&pool, "echo hello", dir.path(), 0, 5).unwrap();

        let conn = pool.get().unwrap();
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM executions", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
    }
}
