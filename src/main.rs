//! `hs` — a fast, private, project-aware shell history tool.

mod capture;
mod cli;
mod context;
mod db;
mod error;
mod guard;
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
            cmd_pin(&pool, id);
        }
        Some(Commands::Unpin { id }) => {
            let pool = get_pool()?;
            cmd_unpin(&pool, id);
        }
        Some(Commands::Pins) => {
            let pool = get_pool()?;
            cmd_pins(&pool);
        }
        Some(Commands::Import) => {
            let pool = get_pool()?;
            cmd_import(&pool);
        }
        Some(Commands::Doctor) => {
            let pool = get_pool()?;
            cmd_doctor(&pool);
        }

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
/// Branching:
/// * zero results → quiet message, exit 0.
/// * `--print` or non-TTY stdout → table, exit 0.
/// * strict query with exactly one exact match → execute immediately
///   (guarded).
/// * otherwise → interactive TUI; `Enter` runs the selected command
///   (guarded), `Esc`/`Ctrl-C` exits without running.
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
        time_window_days: cli.last.as_deref().and_then(parse_time_window),
    };

    let candidates = store.fetch_candidates(&ctx)?;
    let ranked = rank_commands(candidates, current_project_id);

    if ranked.is_empty() {
        println!("No matching commands in history.");
        return Ok(0);
    }

    // A strict, exact single match is an implicit "do it again".
    let exact_match = query.is_some()
        && ranked.len() == 1
        && ranked[0].cmd_string == query.as_deref().unwrap_or_default();

    let interactive = !cli.print && std::io::stdout().is_terminal();

    if exact_match {
        let cmd = ranked[0].cmd_string.clone();
        return guard::execute_safely(&cmd);
    }

    if !interactive {
        ui::format::print_results_table(&ranked, current_project_id);
        return Ok(0);
    }

    match ui::tui::run(&ranked, current_project_id)? {
        Some(cmd) => guard::execute_safely(&cmd),
        None => Ok(0),
    }
}

/// Parse a `--last` window like `30m`, `1h`, `2d`, `1w` into whole days.
///
/// The ranking filter is day-granularized, so sub-day windows floor to
/// 1 day and hour windows round up (`1h` → 1, `30m` → 1, `2d` → 2,
/// `1w` → 7).
fn parse_time_window(spec: &str) -> Option<i64> {
    let spec = spec.trim();
    let (num, unit) = spec.split_at(spec.len().saturating_sub(1));
    let n: i64 = num.parse().ok()?;
    match unit {
        "m" => Some(1),
        "h" => {
            let days = n / 24;
            Some(if n % 24 == 0 { days } else { days + 1 }.max(1))
        }
        "d" => Some(n.max(1)),
        "w" => Some((n * 7).max(1)),
        _ => None,
    }
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

/// Pin a command by ID.
fn cmd_pin(pool: &DbPool, id: i64) {
    let _ = pool;
    println!("Route: Pin | ID: {id}");
}

/// Unpin a command by ID.
fn cmd_unpin(pool: &DbPool, id: i64) {
    let _ = pool;
    println!("Route: Unpin | ID: {id}");
}

/// List all pinned commands.
fn cmd_pins(pool: &DbPool) {
    let _ = pool;
    println!("Route: Pins");
}

/// Import standard shell history.
fn cmd_import(pool: &DbPool) {
    let _ = pool;
    println!("Route: Import");
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
fn cmd_doctor(pool: &DbPool) {
    let _ = pool;
    println!("Route: Doctor (not yet implemented)");
}

/// Handle a capture event from the shell hooks.
fn cmd_capture(pool: &DbPool, cmd: &str, cwd: &str, exit: i32, duration_ms: i64) {
    match capture::process_capture(pool, cmd, std::path::Path::new(cwd), exit, duration_ms) {
        Ok(()) => {}
        Err(e) => eprintln!("hs: capture failed: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_time_windows_to_days() {
        assert_eq!(parse_time_window("30m"), Some(1));
        assert_eq!(parse_time_window("1h"), Some(1));
        assert_eq!(parse_time_window("25h"), Some(2));
        assert_eq!(parse_time_window("2d"), Some(2));
        assert_eq!(parse_time_window("1w"), Some(7));
        assert_eq!(parse_time_window("0"), None);
        assert_eq!(parse_time_window("garbage"), None);
    }
}
