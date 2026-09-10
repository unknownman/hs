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
use std::path::PathBuf;

use clap::Parser;
use tracing::info;

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

    let cli = Cli::parse();

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
/// * `hs <query>` (any query), `--print`, or non-TTY stdout → ranked
///   table, exit 0.
/// * `hs` with no query on a TTY → interactive TUI; `Enter` runs the
///   selected command (guarded), `Esc`/`Ctrl-C` exits without running.
fn cmd_search_and_exec(cli: &Cli) -> Result<i32, HsError> {
    let pool = get_pool()?;
    let store = db::repository::Store::new(pool);

    // Establish the "current project" context for soft boost + grouping.
    let cwd = std::env::current_dir()?;
    let current_project_id = context::find_project_root(&cwd)
        .as_deref()
        .and_then(|root| root.to_str())
        .and_then(|path| store.get_project_id_by_path(path).ok())
        .flatten();

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

    if !should_launch_tui(
        cli.query.is_some(),
        cli.print,
        std::io::stdout().is_terminal(),
    ) {
        ui::format::print_results_table(&ranked, current_project_id);
        return Ok(0);
    }

    ui::tui::run(&ranked, current_project_id)?.map_or(Ok(0), |cmd| guard::execute_safely(&cmd))
}

/// Decide whether to launch the interactive TUI or print a table.
///
/// The TUI is the *only* surface that can run a command (via explicit
/// `Enter`), and it only makes sense as the default action with no
/// query on a real terminal.
fn should_launch_tui(query_present: bool, print: bool, is_terminal: bool) -> bool {
    !query_present && !print && is_terminal
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
fn cmd_pin(pool: &DbPool, id: i64) -> Result<i32, HsError> {
    let store = db::repository::Store::new(pool.clone());
    store.pin_command(id)?;
    println!("[hs] Pinned command #{id}");
    Ok(0)
}

/// Unpin a command by ID. No-op (but still an exit 0) if not pinned.
fn cmd_unpin(pool: &DbPool, id: i64) -> Result<i32, HsError> {
    let store = db::repository::Store::new(pool.clone());
    store.unpin_command(id)?;
    println!("[hs] Unpinned command #{id}");
    Ok(0)
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
    fn should_launch_tui_only_without_query_or_print_on_a_terminal() {
        // `hs` on a TTY → TUI. Every other combination → table.
        assert!(should_launch_tui(false, false, true));
        assert!(
            !should_launch_tui(true, false, true),
            "query forces a table"
        );
        assert!(
            !should_launch_tui(false, true, true),
            "--print forces a table"
        );
        assert!(
            !should_launch_tui(false, false, false),
            "pipe forces a table"
        );
        assert!(!should_launch_tui(true, true, true));
        assert!(!should_launch_tui(true, false, false));
        assert!(!should_launch_tui(false, true, false));
        assert!(!should_launch_tui(true, true, false));
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
}
