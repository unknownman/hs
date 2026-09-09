//! `hs` — a fast, private, project-aware shell history tool.

mod cli;
mod context;
mod db;
mod error;
mod models;
mod redaction;

use std::path::PathBuf;

use clap::Parser;
use tracing::info;

use crate::cli::{Cli, Commands};
use crate::db::DbPool;

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "warn".parse().unwrap()),
        )
        .init();

    let cli = Cli::parse();

    if let Err(e) = run(cli) {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

/// Top-level dispatcher.  Resolves the database (when required) and
/// routes to the correct subsystem.
fn run(cli: Cli) -> Result<(), Box<dyn std::error::Error>> {
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
        Some(Commands::Init) => cmd_init(),

        // ── Hidden: capture (called by shell hooks) ─────────────────
        Some(Commands::Capture {
            precmd,
            preexec,
            cmd,
            cwd,
            exit,
        }) => {
            let pool = get_pool()?;
            cmd_capture(&pool, precmd, preexec, &cmd, &cwd, exit);
        }

        // ── No subcommand: search or launch TUI ─────────────────────
        None => {
            if let Some(ref query) = cli.query {
                let pool = get_pool()?;
                cmd_search(&pool, query, &cli);
            } else {
                cmd_tui();
            }
        }
    }

    Ok(())
}

/// Resolve the database pool, initialising the DB on first use.
fn get_pool() -> Result<DbPool, Box<dyn std::error::Error>> {
    let db_path = default_db_path()?;
    let pool = db::init_db(&db_path)?;
    info!(path = %db_path.display(), "database pool ready");
    Ok(pool)
}

/// Resolve the platform-appropriate path to the `hs` SQLite database.
fn default_db_path() -> Result<PathBuf, error::HsError> {
    let data_dir = dirs::data_local_dir().ok_or(error::HsError::DataDirNotFound)?;
    Ok(data_dir.join("hs").join("hs.db"))
}

// ── Route stubs ────────────────────────────────────────────────────────
//
// Each handler is a stub for Phase 2.  Real logic will be implemented
// in later phases.

/// Fast CLI search — will invoke the Ranking/Recall engine.
fn cmd_search(pool: &DbPool, query: &[String], cli: &Cli) {
    let _ = pool;
    let scope = if cli.project { "project" } else { "global" };
    let outcome = match (cli.ok, cli.failed) {
        (true, _) => "ok",
        (_, true) => "failed",
        _ => "all",
    };
    let last = cli.last.as_deref().unwrap_or("all time");
    let tags = if cli.tags.is_empty() {
        "none".to_string()
    } else {
        cli.tags.join(", ")
    };

    println!(
        "Route: Search | Query: {:?} | Scope: {} | Outcome: {} | Last: {} | Tags: [{}]",
        query, scope, outcome, last, tags,
    );
}

/// Launch the interactive TUI — will use `ratatui`.
fn cmd_tui() {
    println!("Route: TUI (not yet implemented)");
}

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

/// Generate shell hook installation scripts.
fn cmd_init() {
    println!("Route: Init (not yet implemented)");
}

/// Run health checks against the database and environment.
fn cmd_doctor(pool: &DbPool) {
    let _ = pool;
    println!("Route: Doctor (not yet implemented)");
}

/// Handle a capture event from the shell hooks.
fn cmd_capture(pool: &DbPool, _precmd: bool, preexec: bool, cmd: &str, cwd: &str, exit: i32) {
    let _ = pool;
    let phase = if preexec { "preexec" } else { "precmd" };
    println!(
        "Route: Capture | Phase: {} | Cmd: {:?} | Cwd: {} | Exit: {}",
        phase, cmd, cwd, exit,
    );
}
