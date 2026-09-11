//! `hs` — a fast, private, project-aware shell history tool.

mod capture;
mod cli;
mod context;
mod db;
mod doctor;
mod error;
mod guard;
mod import;
mod models;
mod ranking;
mod redaction;
mod ui;

use std::io::IsTerminal as _;
use std::path::{Path, PathBuf};

use clap::Parser;
use tracing::{info, warn};

use crate::cli::{Cli, Commands, Shell};
use crate::db::DbPool;
use crate::error::HsError;
use crate::ranking::{SearchContext, rank_commands};

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "warn".parse().unwrap()),
        )
        .init();

    let mut cli = Cli::parse();
    cli.normalize_embedded_flags();

    let code = match run(cli) {
        Ok(code) => code,
        Err(HsError::Cancelled) => 130,
        Err(e) => {
            eprintln!("error: {e}");
            1
        }
    };
    std::process::exit(code);
}

/// Top-level dispatcher: routes to subcommands, or performs the full
/// search → rank → display → execute pipeline.
fn run(cli: Cli) -> Result<i32, HsError> {
    match cli.command {
        // ── Subcommands that need the database ──────────────────────
        Some(Commands::Pin { id }) => {
            let pool = get_pool()?;
            return cmd_pin(&pool, id);
        }
        Some(Commands::Unpin { id }) => {
            let pool = get_pool()?;
            return cmd_unpin(&pool, id);
        }
        Some(Commands::Delete { id }) => {
            let pool = get_pool()?;
            return cmd_delete(&pool, id);
        }
        Some(Commands::Pins) => {
            let pool = get_pool()?;
            return cmd_pins(&pool);
        }
        Some(Commands::Import { path }) => {
            let pool = get_pool()?;
            return cmd_import(&pool, path);
        }
        Some(Commands::Doctor) => return cmd_doctor(),

        // ── Subcommands that do NOT need the database ───────────────
        Some(Commands::Init { shell }) => cmd_init(shell),

        // ── Hidden: capture (called by shell hooks) ─────────────────
        Some(Commands::Capture {
            cmd,
            cwd,
            exit,
            duration_ms,
        }) => {
            let pool = get_pool()?;
            cmd_capture(&pool, &cmd, &cwd, exit, duration_ms);
        }

        // ── No subcommand: search history and act ───────────────────
        None => return cmd_search_and_exec(&cli),
    }

    Ok(0)
}

/// Search → rank → display → (optionally) safely execute.
///
/// Searching NEVER executes a command. Only an explicit `Enter` in the
/// interactive TUI hands the command to the execution guard.
///
/// Branching:
/// * zero results → quiet message, exit 0.
/// * `--print` or non-TTY stdout → ranked table, exit 0.
/// * terminal (with or without a query) → interactive TUI. A query
///   pre-filters the candidate list; `Enter` runs the selected command
///   (guarded), `Esc`/`Ctrl-C` exits without running.
fn cmd_search_and_exec(cli: &Cli) -> Result<i32, HsError> {
    let pool = get_pool()?;
    let store = db::repository::Store::new(pool.clone());

    // Establish the "current project" context for soft boost + grouping.
    // Discovery of the git root is decoupled from its database id: a
    // brand-new repo has a `.git` but no recorded commands yet, so the id
    // lookup legitimately returns `None` while the repo itself exists.
    //
    // The cwd is taken from `$PWD` when available because that is the
    // *logical* path the shell hooks pass to `capture --cwd`. `current_dir`
    // (getcwd) can return a *physical* path when a symlink sits between the
    // shell and the working directory (e.g. macOS `/var` → `/private/var`),
    // which would disagree with the stored project path and silently break
    // project scoping.
    let cwd = std::env::var("PWD")
        .ok()
        .map(std::path::PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| {
            std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("/"))
        });
    let project_root = context::find_project_root(&cwd);
    let current_project_id = project_root
        .as_deref()
        .and_then(|root| root.to_str())
        .and_then(|path| store.get_project_id_by_path(path).ok())
        .flatten();

    // `--project` is an explicit request to scope results to *this* git
    // repository. It requires a resolvable git root: when `project_root`
    // is `None` we are outside any repo and there is nothing to scope to.
    // (When the root exists but is not in the DB yet, the search proceeds
    // and simply finds no commands — see `fetch_candidates`.)
    if cli.project && project_root.is_none() {
        eprintln!("hs: --project flag requires being inside a git repository");
        return Ok(1);
    }

    let query = cli.query.clone().map(|words| words.join(" "));

    let ctx = SearchContext {
        query: query.clone(),
        current_project_id,
        global: !cli.project,
        ok_only: cli.ok,
        failed_only: cli.failed,
        time_window: cli.last.as_deref().and_then(parse_time_window),
    };

    let candidates = store.fetch_candidates(&ctx)?;
    let ranked = rank_commands(candidates, current_project_id);

    if ranked.is_empty() {
        // Distinguish "fresh install" from "nothing matched": a brand-new
        // database points the user at capture/import instead of silence.
        let has_any_history = store.execution_count()? > 0;
        println!("{}", no_results_message(has_any_history));
        return Ok(0);
    }

    if !should_launch_tui(cli.print, std::io::stdout().is_terminal()) {
        ui::format::print_results_table(&ranked, current_project_id);
        return Ok(0);
    }

    ui::tui::run(&ranked, current_project_id)?
        .map_or(Ok(0), |cmd| execute_and_record(&pool, &cmd, &cwd))
}

/// Run a TUI-selected command and record it in the database.
///
/// The child runs in a non-interactive subshell (`$SHELL -c`), so the
/// user's bash/zsh hooks never fire for it. Without this self-capture the
/// `success_count` / `last_executed_at` of frequently-used commands would
/// stagnate. Timing is measured around the child process and the result is
/// fed straight back into [`capture::process_capture`].
///
/// Recording is strictly best-effort: a capture failure is logged and
/// swallowed so it can never clobber the child's real exit code.
fn execute_and_record(pool: &DbPool, cmd: &str, cwd: &Path) -> Result<i32, HsError> {
    let start = std::time::Instant::now();
    let exit_code = guard::execute_safely(cmd)?;
    let duration_ms = start.elapsed().as_millis() as i64;

    if let Err(e) = capture::process_capture(pool, cmd, cwd, exit_code, duration_ms) {
        warn!(error = %e, "failed to record TUI-executed command");
    }

    Ok(exit_code)
}

/// Decide whether to launch the interactive TUI or print a table.
///
/// The TUI is the *only* surface that can run a command (via explicit
/// `Enter`). It launches on a real terminal whenever `--print` is absent —
/// a query merely pre-filters which commands populate it. `--print` (or a
/// non-terminal stdout) forces the ranked table instead.
fn should_launch_tui(print: bool, is_terminal: bool) -> bool {
    !print && is_terminal
}

/// Parse a `--last` window like `30m`, `1h`, `2d`, `1w` into a SQLite
/// relative-time modifier string.
///
/// SQLite's `date`/`strftime` modifiers accept exact sub-day precision
/// (`'-30 minutes'`, `'-1 hours'`), so sub-day windows no longer round
/// up to a whole day. Only integers are accepted as input and the output
/// is fully deterministic — the returned string is always bound as a
/// **parameterized value** by the query builder (never interpolated into
/// SQL), so malformed input is rejected here before it can even reach
/// the database.
fn parse_time_window(spec: &str) -> Option<String> {
    let spec = spec.trim();
    let (num, unit) = spec.split_at(spec.len().saturating_sub(1));
    let n: i64 = num.parse().ok()?;
    let modifier = match unit {
        "m" => format!("-{n} minutes"),
        "h" => format!("-{n} hours"),
        "d" => format!("-{n} days"),
        "w" => format!("-{} days", n * 7),
        _ => return None,
    };
    Some(modifier)
}

/// Resolve the database pool, initialising the DB on first use.
fn get_pool() -> Result<DbPool, HsError> {
    let db_path = default_db_path()?;
    let pool = db::init_db(&db_path)?;
    info!(path = %db_path.display(), "database pool ready");
    Ok(pool)
}

/// Resolve the platform-appropriate path to the `hs` SQLite database.
fn default_db_path() -> Result<PathBuf, HsError> {
    let data_dir = dirs::data_local_dir().ok_or(HsError::DataDirNotFound)?;
    Ok(data_dir.join("hs").join("hs.db"))
}

// ── Subcommand handlers ────────────────────────────────────────────────────

/// Pin a command by ID, so it surfaces in `hs pins`.
///
/// Exits 1 with a friendly message when no command has that id.
fn cmd_pin(pool: &DbPool, id: i64) -> Result<i32, HsError> {
    let store = db::repository::Store::new(pool.clone());
    match store.pin_command(id)? {
        true => {
            println!("[hs] Pinned command #{id}");
            Ok(0)
        }
        false => {
            eprintln!("[hs] No command found with id #{id}.");
            Ok(1)
        }
    }
}

/// Unpin a command by ID. No-op (but still an exit 0) if not pinned.
fn cmd_unpin(pool: &DbPool, id: i64) -> Result<i32, HsError> {
    let store = db::repository::Store::new(pool.clone());
    store.unpin_command(id)?;
    println!("[hs] Unpinned command #{id}");
    Ok(0)
}

/// Permanently remove a command and all of its history.
///
/// The privacy escape hatch: deletes the command, its executions, stats,
/// pins, and search-index entry. Reports an error and exits 1 when the
/// id does not exist.
fn cmd_delete(pool: &DbPool, id: i64) -> Result<i32, HsError> {
    let store = db::repository::Store::new(pool.clone());
    match store.delete_command(id)? {
        true => {
            println!("[hs] Deleted command #{id} and all of its history");
            Ok(0)
        }
        false => {
            eprintln!("[hs] No command with id #{id} found.");
            Ok(1)
        }
    }
}

/// List every pinned command as a table (or a quiet empty message).
fn cmd_pins(pool: &DbPool) -> Result<i32, HsError> {
    let store = db::repository::Store::new(pool.clone());
    let pins = store.list_pins()?;
    ui::format::print_pins_table(&pins);
    Ok(0)
}

/// Import standard shell history (auto-detected or explicit path).
fn cmd_import(pool: &DbPool, path: Option<PathBuf>) -> Result<i32, HsError> {
    let store = db::repository::Store::new(pool.clone());
    import::run_import(&store, path)
}

/// Generate shell hook installation scripts for the chosen shell.
fn cmd_init(shell: Shell) {
    let script = match shell {
        Shell::Bash => include_str!("../hooks/bash.sh"),
        Shell::Zsh => include_str!("../hooks/zsh.sh"),
    };
    print!("{script}");
}

/// Run health checks against the database and environment.
fn cmd_doctor() -> Result<i32, HsError> {
    let db_path = default_db_path()?;
    Ok(doctor::run_doctor(&db_path))
}

/// Handle a capture event from the shell hooks.
fn cmd_capture(pool: &DbPool, cmd: &str, cwd: &str, exit: i32, duration_ms: i64) {
    match capture::process_capture(pool, cmd, std::path::Path::new(cwd), exit, duration_ms) {
        Ok(()) => {}
        Err(e) => eprintln!("hs: capture failed: {e}"),
    }
}

/// Pick the "no results" message, distinguishing a fresh install from an
/// ordinary miss. `has_any_history` = the database holds ≥1 execution.
fn no_results_message(has_any_history: bool) -> &'static str {
    if has_any_history {
        "No matching commands in history."
    } else {
        "No command history yet. Run some commands or run `hs import` to get started."
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn should_launch_tui_when_terminal_and_not_print() {
        // TTY without --print → TUI, query or not (the query pre-filters).
        assert!(should_launch_tui(false, true));
        // --print forces a table even on a real terminal.
        assert!(!should_launch_tui(true, true));
        // Non-terminal (pipe/script) honors --print → table.
        assert!(!should_launch_tui(false, false));
        assert!(!should_launch_tui(true, false));
    }

    #[test]
    fn parses_time_windows_to_sqlite_modifiers() {
        assert_eq!(parse_time_window("30m"), Some("-30 minutes".to_string()));
        assert_eq!(parse_time_window("1h"), Some("-1 hours".to_string()));
        assert_eq!(parse_time_window("2d"), Some("-2 days".to_string()));
        assert_eq!(parse_time_window("1w"), Some("-7 days".to_string()));
        assert_eq!(parse_time_window("0m"), Some("-0 minutes".to_string()));
        assert_eq!(parse_time_window("0"), None);
        assert_eq!(parse_time_window("garbage"), None);
        assert_eq!(parse_time_window(""), None);
    }

    #[test]
    fn empty_state_message_guides_fresh_install() {
        // No history at all → guide the user toward capture/import.
        assert!(no_results_message(false).contains("hs import"));
        assert_eq!(
            no_results_message(false),
            "No command history yet. Run some commands or run `hs import` to get started."
        );
        // History exists but nothing matched → plain "no results".
        assert!(no_results_message(true).contains("No matching"));
        assert!(!no_results_message(true).contains("hs import"));
    }

    /// A TUI-executed command (run via `execute_and_record`) must be
    /// persisted like a shell-hook capture: one execution row, and the
    /// command's stats updated so recency/frequency ranking does not
    /// stagnate.
    #[test]
    fn execute_and_record_self_captures_and_updates_stats() {
        use crate::db::tests::test_pool;

        let pool = test_pool();
        let dir = tempfile::tempdir().unwrap();
        let cwd = dir.path().to_path_buf();

        let code = execute_and_record(&pool, "true", &cwd).unwrap();
        assert_eq!(code, 0, "child exit code must propagate");

        let conn = pool.get().unwrap();
        let executions: i64 = conn
            .query_row("SELECT COUNT(*) FROM executions", [], |r| r.get(0))
            .unwrap();
        assert_eq!(executions, 1, "TUI execution must be recorded");

        let (success_count, fail_count, last): (i64, i64, Option<String>) = conn
            .query_row(
                "SELECT s.success_count, s.fail_count, s.last_executed_at
                 FROM command_stats s
                 JOIN commands c ON c.id = s.command_id
                 WHERE c.cmd_string = 'true'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(success_count, 1, "success_count must increment");
        assert_eq!(fail_count, 0);
        assert!(last.is_some(), "last_executed_at must be set");
    }
}
