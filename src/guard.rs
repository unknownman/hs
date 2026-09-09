//! Safety by Default — the execution guard.
//!
//! Every command that leaves `hs` (via the TUI or an exact match)
//! passes through here. Safe commands run immediately; commands
//! classified [`RiskLevel::High`](crate::context::RiskLevel::High) by
//! the context engine first require an explicit interactive
//! confirmation.
//!
//! The confirmation itself is injectable (see
//! [`execute_safely_with`]) so unit tests can exercise both branches
//! without a TTY.

use crate::context::{RiskLevel, analyze_risk};
use crate::error::HsError;

/// Execute `cmd_string` through `/bin/sh`, honoring the risk guard.
///
/// * `Safe` — prints a subtle indicator, then runs the command with
///   stdin/stdout/stderr inherited (interactive programs like `vim`
///   keep working).
/// * `High` — prints a bold red warning and asks for confirmation.
///   Declining returns [`HsError::Cancelled`].
///
/// Returns the child process exit code on success.
pub fn execute_safely(cmd_string: &str) -> Result<i32, HsError> {
    execute_safely_with(cmd_string, default_confirm)
}

/// Same as [`execute_safely`], with a pluggable confirmation prompt.
pub fn execute_safely_with<F>(cmd_string: &str, confirm: F) -> Result<i32, HsError>
where
    F: FnOnce(&str) -> bool,
{
    match analyze_risk(cmd_string) {
        RiskLevel::Safe => run_child(cmd_string),
        RiskLevel::High => {
            eprintln!("\x1b[1;31m[!] CAUTION: This command is flagged as High Risk\x1b[0m");
            eprintln!("    {cmd_string}");
            if confirm(cmd_string) {
                run_child(cmd_string)
            } else {
                Err(HsError::Cancelled)
            }
        }
    }
}

/// Default interactive confirmation.  Fails **closed**: if no TTY is
/// available the prompt errors and the command is declined.
fn default_confirm(_cmd: &str) -> bool {
    dialoguer::Confirm::new()
        .with_prompt("Are you sure you want to execute this?")
        .default(false)
        .interact()
        .inspect(|&yes| {
            if !yes {
                eprintln!("[hs] Aborted — nothing was executed.");
            }
        })
        .unwrap_or_else(|_| {
            eprintln!("[hs] Aborted — confirmation unavailable (not a TTY).");
            false
        })
}

/// Spawn `/bin/sh -c <cmd>` inheriting stdio, wait, and return the
/// child's exit code.  `sh` gives us shell semantics (globs, pipes,
/// env expansion) exactly matching what the user typed historically.
fn run_child(cmd_string: &str) -> Result<i32, HsError> {
    println!("\x1b[2m[hs] Running: {cmd_string}\x1b[0m");
    let status = std::process::Command::new("sh")
        .arg("-c")
        .arg(cmd_string)
        .status()?;
    Ok(status.code().unwrap_or(1))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Confirmation probe: records whether it was called and returns a
    /// scripted answer.
    struct Probe {
        called_with: Vec<String>,
        answer: bool,
    }

    impl Probe {
        fn from(answer: bool) -> Self {
            Probe {
                called_with: Vec::new(),
                answer,
            }
        }
    }

    #[test]
    fn safe_command_bypasses_confirmation() {
        let mut probe = Probe::from(true); // would confirm, but must not be asked
        let code = execute_safely_with("printf ok", |cmd| {
            probe.called_with.push(cmd.to_string());
            probe.answer
        })
        .unwrap();

        assert_eq!(code, 0);
        assert!(
            probe.called_with.is_empty(),
            "safe commands must not trigger the confirmation prompt"
        );
    }

    #[test]
    fn high_risk_command_without_confirmation_is_cancelled() {
        let mut probe = Probe::from(false);
        let result = execute_safely_with("rm -rf /", |cmd| {
            probe.called_with.push(cmd.to_string());
            probe.answer
        });

        assert!(matches!(result, Err(HsError::Cancelled)));
        assert_eq!(
            probe.called_with,
            vec!["rm -rf /"]
                .into_iter()
                .map(String::from)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn high_risk_command_confirmed_executes() {
        let mut probe = Probe::from(true);
        // Matches the `dd if=` risk pattern but is harmless (writes one
        // byte to a temp file), so confirming it is safe to truly run.
        let cmd = "dd if=/dev/zero of=/tmp/hs_guard_test.bin bs=1 count=1";
        let code = execute_safely_with(cmd, |c| {
            probe.called_with.push(c.to_string());
            probe.answer
        })
        .expect("confirmed high-risk command must run");

        assert_eq!(code, 0, "child exit code must propagate");
        assert_eq!(probe.called_with, vec![cmd.to_string()]);
    }

    #[test]
    fn child_exit_code_propagates() {
        let code = execute_safely_with("false", |_| true).unwrap();
        assert_eq!(code, 1);
        let code = execute_safely_with("exit 42", |_| true).unwrap();
        assert_eq!(code, 42);
    }

    #[test]
    fn sh_inherits_stdio_for_interactive_commands() {
        // `yes | head -1` proves the pipeline runs under sh with stdio wired.
        let code = execute_safely_with("printf 'interactive-ready\n' | head -1", |_| true).unwrap();
        assert_eq!(code, 0);
    }
}
