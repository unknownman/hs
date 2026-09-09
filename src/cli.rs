//! Command-line interface definition for `hs`.
//!
//! All argument parsing and help-text generation lives here.
//! The routing logic that dispatches to subsystems lives in [`crate::main`].

use clap::{ArgAction, Parser, Subcommand, ValueEnum};

/// `hs` — What worked here before?
///
/// `hs` is a local, context-aware command memory for developers and DevOps.
/// Standard shell history only answers "What did I type?". `hs` answers
/// "What worked here before?" by silently capturing commands and enriching
/// them with context — where it ran, whether it succeeded, how often it
/// succeeds, and which project it belongs to.
///
/// When called with a query (`hs deploy app`), `hs` performs a fast CLI
/// search and returns ranked results. When called with no arguments
/// (`hs`), it launches an interactive terminal UI for browsing and
/// filtering history.
///
/// # Examples
///
/// Search for "deploy" within the current git project, only successful runs:
///
///     hs --project --ok deploy
///
/// Search globally for "docker build" commands from the last week:
///
///     hs --global --last 1w docker build
///
/// Search with multiple tech-stack filters:
///
///     hs -t cargo -t docker build
///
/// Launch the interactive TUI:
///
///     hs
#[derive(Parser, Debug)]
#[command(
    name = "hs",
    version,
    about = "A fast, private, project-aware shell history tool",
    long_about = "hs — What worked here before?\n\n\
        Standard shell history only answers \"What did I type?\".\n\
        hs answers \"What worked here before?\" by capturing commands\n\
        and enriching them with context: where it ran, whether it\n\
        succeeded, how often it succeeds, and which project it belongs to.\n\n\
        Run hs with no arguments to launch the interactive TUI, or\n\
        provide a query to perform a fast CLI search.\n\n\
        # Quick start\n\
        hs --project --ok deploy       # successful deploy commands in this project\n\
        hs --last 2d docker            # docker commands from the last 2 days\n\
        hs -t cargo -t docker build    # build commands tagged with cargo or docker"
)]
pub struct Cli {
    /// Search query. Omit to launch the interactive TUI.
    #[arg(
        trailing_var_arg = true,
        help = "Words to search for in command history (omit to launch TUI)"
    )]
    pub query: Option<Vec<String>>,

    // ── Scope filters ──────────────────────────────────────────────
    /// Restrict search to the current git project.
    ///
    /// Detects the nearest `.git` ancestor and searches only commands
    /// captured within that project boundary. Mutually exclusive with
    /// `--global`.
    #[arg(
        long,
        conflicts_with = "global",
        help = "Search only within the current git project"
    )]
    pub project: bool,

    /// Search all commands across every project.
    ///
    /// Overrides project boundaries and searches the entire history
    /// database. Mutually exclusive with `--project`.
    #[arg(
        long,
        conflicts_with = "project",
        help = "Search across all projects (default)"
    )]
    pub global: bool,

    // ── Outcome filters ────────────────────────────────────────────
    /// Only show commands that succeeded (exit code 0).
    ///
    /// Useful for finding known-working commands. Mutually exclusive
    /// with `--failed`.
    #[arg(
        long,
        conflicts_with = "failed",
        help = "Only show commands that exited successfully (exit code 0)"
    )]
    pub ok: bool,

    /// Only show commands that failed (exit code > 0).
    ///
    /// Useful for debugging or finding commands that need fixing.
    /// Mutually exclusive with `--ok`.
    #[arg(
        long,
        conflicts_with = "ok",
        help = "Only show commands that failed (exit code > 0)"
    )]
    pub failed: bool,

    // ── Presentation ────────────────────────────────────────────────
    /// Print ranked results as a table and exit.
    ///
    /// Bypasses the interactive TUI (and the auto-execute path) — useful
    /// for piping output or scripting. Results are also printed
    /// automatically when stdout is not a terminal.
    #[arg(long, help = "Print results as a table and exit (no TUI)")]
    pub print: bool,

    // ── Time window ────────────────────────────────────────────────
    /// Only show commands from the last time window.
    ///
    /// Accepts a duration string: `1h` (hours), `2d` (days),
    /// `1w` (weeks), `30m` (minutes). Examples: `--last 1h`,
    /// `--last 2d`, `--last 1w`.
    #[arg(long, value_name = "WINDOW", help = "Time window: 30m, 1h, 2d, 1w")]
    pub last: Option<String>,

    // ── Stack filter ───────────────────────────────────────────────
    /// Filter by tech-stack tags (repeatable).
    ///
    /// Matches commands captured in projects that contain specific
    /// marker files (e.g. `Cargo.toml` → `cargo`, `Dockerfile` →
    /// `docker`). Can be repeated to narrow results:
    /// `-t cargo -t docker`.
    #[arg(
        short = 't',
        long = "tag",
        value_name = "TAG",
        action = ArgAction::Append,
        help = "Filter by tech-stack tag (repeatable, e.g. -t cargo -t docker)"
    )]
    pub tags: Vec<String>,

    // ── Subcommands ────────────────────────────────────────────────
    #[command(subcommand)]
    pub command: Option<Commands>,
}

#[derive(Subcommand, Debug, Clone)]
pub enum Commands {
    /// Pin a command to prevent it from decaying in search ranking.
    ///
    /// Pinned commands always appear at the top of results regardless
    /// of recency or frequency scores.
    Pin {
        /// The numeric ID of the command to pin (from `hs` search results).
        id: i64,
    },

    /// Remove a pin from a previously pinned command.
    ///
    /// The command will return to normal ranking behavior.
    Unpin {
        /// The numeric ID of the command to unpin.
        id: i64,
    },

    /// List all currently pinned commands.
    ///
    /// Shows the command string, project, and pin timestamp for each
    /// pinned entry.
    Pins,

    /// Import standard shell history into the hs database.
    ///
    /// Reads from `~/.bash_history` or `~/.zsh_history` and bulk-inserts
    /// commands. Duplicate commands are merged (a new execution record is
    /// added). Secrets are redacted before persistence.
    Import,

    /// Generate shell hook installation scripts.
    ///
    /// Prints the preexec/precmd hook code for the chosen shell to
    /// stdout, pure and side-effect free, so it can be piped straight
    /// into your rc file:
    /// `eval "$(hs init zsh)"`.
    Init {
        /// Which shell to generate hooks for.
        #[arg(value_enum)]
        shell: Shell,
    },

    /// Check database health and configuration.
    ///
    /// Verifies: database file exists and is readable, WAL mode is
    /// active, schema version is current, shell hooks are properly
    /// sourced in the active shell, and the data directory permissions
    /// are correct.
    Doctor,

    /// (Internal) Capture a completed command execution — called by shell hooks.
    ///
    /// Stateless one-shot endpoint: the shell hook backgrounds this
    /// command *after* the user's command finishes, so the prompt is
    /// never blocked — even if the database is momentarily busy.
    #[command(hide = true)]
    Capture {
        /// The command string that was executed.
        #[arg(long, value_name = "STRING")]
        cmd: String,

        /// The working directory of the shell at execution time.
        #[arg(long, value_name = "PATH")]
        cwd: String,

        /// The process exit code (0 = success).
        #[arg(long, value_name = "CODE", default_value_t = 0)]
        exit: i32,

        /// Wall-clock duration of the command in milliseconds.
        #[arg(long = "duration-ms", value_name = "MS", default_value_t = 0)]
        duration_ms: i64,
    },
}

/// Scope of a search query — which project boundary to honour.
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum Scope {
    /// Only commands captured within the current git project.
    Project,
    /// All commands, regardless of project.
    Global,
}

/// Outcome filter — which exit-code category to include.
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum Outcome {
    /// Only successful commands (exit code 0).
    Ok,
    /// Only failed commands (exit code > 0).
    Failed,
}

/// Shells supported by `hs init`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Shell {
    /// Bash — uses a DEBUG trap + `PROMPT_COMMAND`.
    Bash,
    /// Zsh — uses `preexec`/`precmd` hooks.
    Zsh,
}

// ── Tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn parse_search_query() {
        let cli = Cli::try_parse_from(["hs", "deploy", "app"]).unwrap();
        assert_eq!(cli.query, Some(vec!["deploy".into(), "app".into()]));
        assert!(cli.command.is_none());
    }

    #[test]
    fn parse_no_args_launches_tui() {
        let cli = Cli::try_parse_from(["hs"]).unwrap();
        assert!(cli.query.is_none());
        assert!(cli.command.is_none());
    }

    #[test]
    fn parse_scope_flags() {
        let cli = Cli::try_parse_from(["hs", "--project", "build"]).unwrap();
        assert!(cli.project);
        assert!(!cli.global);

        let cli = Cli::try_parse_from(["hs", "--global", "build"]).unwrap();
        assert!(!cli.project);
        assert!(cli.global);
    }

    #[test]
    fn project_global_conflict() {
        let result = Cli::try_parse_from(["hs", "--project", "--global", "build"]);
        assert!(result.is_err(), "--project and --global must conflict");
    }

    #[test]
    fn parse_outcome_flags() {
        let cli = Cli::try_parse_from(["hs", "--ok", "build"]).unwrap();
        assert!(cli.ok);
        assert!(!cli.failed);
    }

    #[test]
    fn ok_failed_conflict() {
        let result = Cli::try_parse_from(["hs", "--ok", "--failed", "build"]);
        assert!(result.is_err(), "--ok and --failed must conflict");
    }

    #[test]
    fn parse_time_window() {
        let cli = Cli::try_parse_from(["hs", "--last", "2d", "docker"]).unwrap();
        assert_eq!(cli.last.as_deref(), Some("2d"));
    }

    #[test]
    fn parse_repeatable_tags() {
        let cli = Cli::try_parse_from(["hs", "-t", "cargo", "-t", "docker", "build"]).unwrap();
        assert_eq!(cli.tags, vec!["cargo", "docker"]);
    }

    #[test]
    fn parse_print_flag() {
        let cli = Cli::try_parse_from(["hs", "--print", "docker", "build"]).unwrap();
        assert!(cli.print);
        assert_eq!(
            cli.query,
            Some(vec!["docker".to_string(), "build".to_string()])
        );
    }

    #[test]
    fn parse_pin_subcommand() {
        let cli = Cli::try_parse_from(["hs", "pin", "42"]).unwrap();
        match cli.command.unwrap() {
            Commands::Pin { id } => assert_eq!(id, 42),
            _ => panic!("expected Pin"),
        }
    }

    #[test]
    fn parse_unpin_subcommand() {
        let cli = Cli::try_parse_from(["hs", "unpin", "42"]).unwrap();
        match cli.command.unwrap() {
            Commands::Unpin { id } => assert_eq!(id, 42),
            _ => panic!("expected Unpin"),
        }
    }

    #[test]
    fn parse_pins_subcommand() {
        let cli = Cli::try_parse_from(["hs", "pins"]).unwrap();
        assert!(matches!(cli.command.unwrap(), Commands::Pins));
    }

    #[test]
    fn parse_import_subcommand() {
        let cli = Cli::try_parse_from(["hs", "import"]).unwrap();
        assert!(matches!(cli.command.unwrap(), Commands::Import));
    }

    #[test]
    fn parse_init_subcommand() {
        let cli = Cli::try_parse_from(["hs", "init", "zsh"]).unwrap();
        match cli.command.unwrap() {
            Commands::Init { shell } => assert_eq!(shell, Shell::Zsh),
            _ => panic!("expected Init"),
        }
    }

    #[test]
    fn init_requires_shell() {
        let result = Cli::try_parse_from(["hs", "init"]);
        assert!(result.is_err(), "init must require a shell argument");
    }

    #[test]
    fn parse_doctor_subcommand() {
        let cli = Cli::try_parse_from(["hs", "doctor"]).unwrap();
        assert!(matches!(cli.command.unwrap(), Commands::Doctor));
    }

    #[test]
    fn parse_capture_subcommand() {
        let cli = Cli::try_parse_from([
            "hs",
            "capture",
            "--cmd",
            "docker build .",
            "--cwd",
            "/tmp",
            "--exit",
            "1",
            "--duration-ms",
            "4321",
        ])
        .unwrap();

        match cli.command.unwrap() {
            Commands::Capture {
                cmd,
                cwd,
                exit,
                duration_ms,
            } => {
                assert_eq!(cmd, "docker build .");
                assert_eq!(cwd, "/tmp");
                assert_eq!(exit, 1);
                assert_eq!(duration_ms, 4321);
            }
            _ => panic!("expected Capture"),
        }
    }

    #[test]
    fn capture_defaults() {
        let cli = Cli::try_parse_from(["hs", "capture", "--cmd", "ls", "--cwd", "/tmp"]).unwrap();
        match cli.command.unwrap() {
            Commands::Capture {
                cmd,
                cwd,
                exit,
                duration_ms,
            } => {
                assert_eq!(cmd, "ls");
                assert_eq!(cwd, "/tmp");
                assert_eq!(exit, 0);
                assert_eq!(duration_ms, 0);
            }
            _ => panic!("expected Capture"),
        }
    }

    #[test]
    fn combined_flags() {
        let cli = Cli::try_parse_from([
            "hs",
            "--project",
            "--ok",
            "--last",
            "1w",
            "-t",
            "cargo",
            "build",
        ])
        .unwrap();

        assert!(cli.project);
        assert!(cli.ok);
        assert_eq!(cli.last.as_deref(), Some("1w"));
        assert_eq!(cli.tags, vec!["cargo"]);
        assert_eq!(cli.query, Some(vec!["build".into()]));
    }
}
