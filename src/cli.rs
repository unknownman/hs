//! Command-line interface definition for `hs`.
//!
//! All argument parsing and help-text generation lives here.
//! The routing logic that dispatches to subsystems lives in [`crate::main`].

use clap::{Parser, Subcommand, ValueEnum};
use std::path::PathBuf;

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
        hs --ok build                  # successful build commands"
)]
pub struct Cli {
    /// Search query. Omit to launch the interactive TUI.
    ///
    /// A query acts as a **pre-filter** for the TUI (or, with `--print`,
    /// for the ranked table). Hyphen-prefixed words like `--force` are
    /// accepted as part of the query instead of being rejected as unknown
    /// flags. TRADEOFF: clap's `allow_hyphen_values` collects *all*
    /// post-query tokens into the query, so real flags typed after the
    /// query (`hs deploy --print`) are restored by
    /// [`Cli::normalize_embedded_flags`].
    #[arg(
        allow_hyphen_values = true,
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

    // ── Subcommands ────────────────────────────────────────────────
    #[command(subcommand)]
    pub command: Option<Commands>,
}

impl Cli {
    /// Restore real flags the user typed *after* the first query word
    /// (e.g. `hs deploy --print`, `hs docker --last 1d`).
    ///
    /// clap's `allow_hyphen_values` means query words may start with `-`
    /// (`hs --force` searches for `--force`), but as a side effect every
    /// token after the query is collected into the query — so `--print`
    /// would otherwise be swallowed. This hoists the recognised flags back
    /// into their dedicated fields (`--last` together with its value);
    /// unknown hyphen words (like `--force`) stay in the query. An
    /// empty or all-whitespace query is normalized to `None`. Called right
    /// after [`Parser::parse`].
    pub fn normalize_embedded_flags(&mut self) {
        let Some(query) = self.query.as_mut() else {
            return;
        };

        let mut i = 0;
        while i < query.len() {
            let hoisted = match query[i].as_str() {
                "--print" => {
                    self.print = true;
                    true
                }
                "--ok" => {
                    self.ok = true;
                    true
                }
                "--failed" => {
                    self.failed = true;
                    true
                }
                "--project" => {
                    self.project = true;
                    true
                }
                "--global" => {
                    self.global = true;
                    true
                }
                // `--last WINDOW` consumes the flag *and* its value.
                "--last" if i + 1 < query.len() => {
                    self.last = Some(query[i + 1].clone());
                    query.remove(i + 1);
                    true
                }
                "--last" => false,
                // `--last=1d` — standard clap `--flag=value` syntax bound
                // with `=`. The whole token is one string, so strip the
                // flag prefix and hoist its remainder as the window.
                kind if kind.starts_with("--last=") => {
                    self.last = Some(kind[7..].to_string());
                    true
                }
                _ => false,
            };
            if hoisted {
                query.remove(i);
            } else {
                i += 1;
            }
        }

        // An all-empty/whitespace query is equivalent to no query at all
        // (`hs docker --print` or `hs "   "`): normalize to `None` so the
        // caller never hands a zero-match `WHERE 1 = 0` to the FTS engine.
        if self
            .query
            .as_ref()
            .is_some_and(|words| words.is_empty() || words.iter().all(|w| w.trim().is_empty()))
        {
            self.query = None;
        }
    }
}

#[derive(Subcommand, Debug, Clone)]
pub enum Commands {
    /// Pin a command by its numeric ID.
    ///
    /// The command is remembered in the `pins` list and surfaces in
    /// `hs pins` for quick recall. Pinning is idempotent. Use the ID
    /// shown in `hs` search results.
    Pin {
        /// The numeric ID of the command to pin (from `hs` search results).
        id: i64,
    },

    /// Remove a pin from a previously pinned command.
    ///
    /// The command disappears from `hs pins` on the next invocation.
    /// Unpinning an unpinned command is a no-op and still exits 0.
    Unpin {
        /// The numeric ID of the command to unpin.
        id: i64,
    },

    /// Permanently remove a command and all of its executions.
    ///
    /// The privacy escape hatch: use it when a secret ever evades
    /// redaction, or a useless typo pollutes your history. Deletes the
    /// command row, cascading to its executions, stats, and pins, and
    /// dropping it from search results. This cannot be undone. Alias: `rm`.
    #[command(alias = "rm")]
    Delete {
        /// The numeric ID of the command to permanently delete (from `hs`
        /// search results).
        id: i64,
    },

    /// List all currently pinned commands.
    ///
    /// Shows each pinned command's string, project, and pin timestamp.
    Pins,

    /// Import standard shell history into the hs database.
    ///
    /// Reads from `~/.bash_history` or `~/.zsh_history` (auto-detected)
    /// and bulk-inserts commands inside a single transaction. Duplicate
    /// commands are skipped, and secrets are redacted before persistence.
    /// Pass an explicit `PATH` to import from any history file.
    Import {
        /// History file to import (default: auto-detect
        /// `~/.zsh_history` / `~/.bash_history`).
        #[arg(value_name = "PATH")]
        path: Option<PathBuf>,
    },

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
        ///
        /// `allow_hyphen_values` lets commands that *start* with `-`
        /// (e.g. `-ls` or `rm -rf`) arrive intact — clap would otherwise
        /// reject the value as an unknown flag before the hook can fire.
        #[arg(long, value_name = "STRING", allow_hyphen_values = true)]
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
    fn query_allows_hyphen_prefixed_flags() {
        // `hs --force` must parse `--force` as the query, not die with an
        // unknown-flag error.
        let cli = Cli::try_parse_from(["hs", "--force"]).unwrap();
        assert_eq!(cli.query, Some(vec!["--force".to_string()]));
        assert!(!cli.print, "--print must remain a real flag");
    }

    #[test]
    fn flags_after_the_query_are_normalized() {
        // `hs deploy --print` → the trailing `--print` must be restored
        // as the print flag while `deploy` stays the query.
        let mut cli = Cli::try_parse_from(["hs", "deploy", "--print"]).unwrap();
        cli.normalize_embedded_flags();
        assert!(cli.print, "--print after the query must still be a flag");
        assert_eq!(cli.query, Some(vec!["deploy".to_string()]));

        // Outcome/scope flags typed after the query are hoisted too.
        let mut cli = Cli::try_parse_from(["hs", "docker", "build", "--ok", "--project"]).unwrap();
        cli.normalize_embedded_flags();
        assert!(cli.ok);
        assert!(cli.project);
        assert_eq!(
            cli.query,
            Some(vec!["docker".to_string(), "build".to_string()])
        );

        // Unknown hyphen words survive the normalization. (`rm` is now a
        // real subcommand alias, so a bare `hs rm ...` routes to
        // `delete`; use another leading word to search for it.)
        let mut cli = Cli::try_parse_from(["hs", "find", "-rf", "--force"]).unwrap();
        cli.normalize_embedded_flags();
        assert_eq!(
            cli.query,
            Some(vec![
                "find".to_string(),
                "-rf".to_string(),
                "--force".to_string()
            ])
        );

        // Flags before the query are parsed natively and untouched.
        let mut cli = Cli::try_parse_from(["hs", "--print", "docker", "--force"]).unwrap();
        cli.normalize_embedded_flags();
        assert!(cli.print);
        assert_eq!(
            cli.query,
            Some(vec!["docker".to_string(), "--force".to_string()])
        );
    }

    #[test]
    fn flags_after_the_query_with_values_are_normalized() {
        // `hs docker --last 1d` → `--last` *and* its value must be
        // restored as the time window, leaving `docker` as the query.
        let mut cli = Cli::try_parse_from(["hs", "docker", "--last", "1d"]).unwrap();
        cli.normalize_embedded_flags();
        assert_eq!(cli.last.as_deref(), Some("1d"));
        assert_eq!(cli.query, Some(vec!["docker".to_string()]));

        // A trailing `--last` with no value stays in the query.
        let mut cli = Cli::try_parse_from(["hs", "docker", "--last"]).unwrap();
        cli.normalize_embedded_flags();
        assert_eq!(cli.last, None);
        assert_eq!(
            cli.query,
            Some(vec!["docker".to_string(), "--last".to_string()])
        );
    }

    #[test]
    fn flags_with_equals_are_normalized() {
        // `hs docker --last=1d` — standard `--flag=value` syntax — must
        // hoist the bound value into the window filter, not leak `--last=1d`
        // into the FTS query as a literal search term.
        let mut cli = Cli::try_parse_from(["hs", "docker", "--last=1d"]).unwrap();
        cli.normalize_embedded_flags();
        assert_eq!(cli.last.as_deref(), Some("1d"));
        assert_eq!(cli.query, Some(vec!["docker".to_string()]));

        // `=`-bound boolean flags work too, mixing with later `--last`.
        let mut cli =
            Cli::try_parse_from(["hs", "docker", "build", "--print", "--last=2d"]).unwrap();
        cli.normalize_embedded_flags();
        assert!(cli.print);
        assert_eq!(cli.last.as_deref(), Some("2d"));
        assert_eq!(
            cli.query,
            Some(vec!["docker".to_string(), "build".to_string()])
        );
    }

    #[test]
    fn empty_or_whitespace_queries_normalize_to_none() {
        // `hs "   "` — an all-whitespace query — must become `None`
        // instead of a zero-match FTS query.
        let mut cli = Cli::try_parse_from(["hs", "   "]).unwrap();
        assert_eq!(cli.query, Some(vec!["   ".to_string()]));
        cli.normalize_embedded_flags();
        assert_eq!(cli.query, None);

        // `hs --print` with no query words is trivially `None` from
        // clap; normalize must not break it.
        let mut cli = Cli::try_parse_from(["hs", "--print"]).unwrap();
        assert!(cli.print);
        assert_eq!(cli.query, None);
        cli.normalize_embedded_flags();
        assert_eq!(cli.query, None);
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
    fn removed_tag_flag_is_swallowed_into_query() {
        // `-t`/`--tag` were removed as flags. With `allow_hyphen_values`
        // the words are no longer hard errors — they join the query, so
        // searching for a literal flag like `--force` keeps working.
        let cli = Cli::try_parse_from(["hs", "-t", "cargo", "build"]).unwrap();
        assert_eq!(
            cli.query,
            Some(vec![
                "-t".to_string(),
                "cargo".to_string(),
                "build".to_string()
            ])
        );
        // A known flag still parses as a flag when it appears first.
        let cli = Cli::try_parse_from(["hs", "--tag", "docker", "build"]).unwrap();
        assert!(cli.command.is_none());
        assert_eq!(
            cli.query,
            Some(vec![
                "--tag".to_string(),
                "docker".to_string(),
                "build".to_string()
            ])
        );
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
    fn parse_delete_subcommand() {
        let cli = Cli::try_parse_from(["hs", "delete", "42"]).unwrap();
        match cli.command.unwrap() {
            Commands::Delete { id } => assert_eq!(id, 42),
            _ => panic!("expected Delete"),
        }
    }

    #[test]
    fn parse_delete_rm_alias() {
        let cli = Cli::try_parse_from(["hs", "rm", "42"]).unwrap();
        match cli.command.unwrap() {
            Commands::Delete { id } => assert_eq!(id, 42),
            _ => panic!("expected Delete via the rm alias"),
        }
    }

    #[test]
    fn delete_requires_id() {
        let result = Cli::try_parse_from(["hs", "delete"]);
        assert!(result.is_err(), "delete must require a command id");
    }

    #[test]
    fn parse_import_subcommand() {
        let cli = Cli::try_parse_from(["hs", "import"]).unwrap();
        match cli.command.unwrap() {
            Commands::Import { path } => assert!(path.is_none()),
            _ => panic!("expected Import"),
        }
    }

    #[test]
    fn parse_import_with_explicit_path() {
        let cli = Cli::try_parse_from(["hs", "import", "~/backup/history.bak"]).unwrap();
        match cli.command.unwrap() {
            Commands::Import { path } => {
                assert_eq!(path, Some(PathBuf::from("~/backup/history.bak")))
            }
            _ => panic!("expected Import"),
        }
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
    fn capture_accepts_hyphenated_command() {
        // Commands starting with `-` (e.g. `-ls`) must not be swallowed
        // by clap as an unknown option.
        let cli = Cli::try_parse_from(["hs", "capture", "--cmd", "-ls", "--cwd", "/tmp"]).unwrap();
        match cli.command.unwrap() {
            Commands::Capture { cmd, cwd, .. } => {
                assert_eq!(cmd, "-ls");
                assert_eq!(cwd, "/tmp");
            }
            _ => panic!("expected Capture"),
        }
    }

    #[test]
    fn combined_flags() {
        let cli =
            Cli::try_parse_from(["hs", "--project", "--ok", "--last", "1w", "build"]).unwrap();

        assert!(cli.project);
        assert!(cli.ok);
        assert_eq!(cli.last.as_deref(), Some("1w"));
        assert_eq!(cli.query, Some(vec!["build".into()]));
    }
}
