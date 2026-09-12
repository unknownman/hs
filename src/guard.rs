//! Safety by Default — the execution guard.
//!
//! Every command that leaves `hs` (via the TUI) passes through here.
//! Safe commands run immediately; commands classified
//! [`RiskLevel::High`](crate::context::RiskLevel::High) by the context
//! engine first require an explicit interactive confirmation.
//!
//! The confirmation itself is injectable (see
//! [`execute_safely_with`]) so unit tests can exercise both branches
//! without a TTY.

use crate::context::{RiskLevel, analyze_risk};
use crate::error::HsError;

/// Execute `cmd_string` through the user's `$SHELL`, honoring the guard.
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
///
/// The shell is resolved **once** from the `SHELL` environment variable
/// (falling back to `/bin/sh`) and injected into [`run_child`], so the
/// shell selection is a pure argument — no caller or test ever has to
/// mutate the global environment (which would be Undefined Behavior in a
/// multi-threaded harness).
pub fn execute_safely_with<F>(cmd_string: &str, confirm: F) -> Result<i32, HsError>
where
    F: FnOnce(&str) -> bool,
{
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string());

    // A command containing a literally-redacted secret (`[REDACTED]`) is
    // guaranteed to fail as written (the TUI offers no in-place editing).
    // Intercept it before the risk classifier and demand an explicit
    // confirmation; declining aborts just like a declined High Risk.
    if cmd_string.contains("[REDACTED]") {
        eprintln!("\x1b[1;33m[!] WARNING: This command contains [REDACTED] secrets.\x1b[0m");
        eprintln!("    It will likely fail unless you manually edit and run it outside hs.");
        eprintln!("    {cmd_string}");
        return if confirm(cmd_string) {
            run_child(cmd_string, &shell)
        } else {
            Err(HsError::Cancelled)
        };
    }

    match analyze_risk(cmd_string) {
        RiskLevel::Safe => run_child(cmd_string, &shell),
        RiskLevel::High => {
            eprintln!("\x1b[1;31m[!] CAUTION: This command is flagged as High Risk\x1b[0m");
            eprintln!("    {cmd_string}");
            if confirm(cmd_string) {
                run_child(cmd_string, &shell)
            } else {
                Err(HsError::Cancelled)
            }
        }
    }
}

/// Default interactive confirmation.  Fails **closed**: a non-TTY, a
/// raw-mode Ctrl-C ([`std::io::ErrorKind::Interrupted`] inside
/// [`dialoguer::Error::IO`]), or the dialoguer cancel key (`Esc`/`q`,
/// surfaced as `Ok(None)`) all decline the command instead of crashing —
/// the caller then returns [`HsError::Cancelled`] gracefully.
fn default_confirm(_cmd: &str) -> bool {
    match dialoguer::Confirm::new()
        .with_prompt("Are you sure you want to execute this?")
        .default(false)
        .interact_opt()
    {
        Ok(Some(true)) => true,
        // Explicit "no", cancel (Esc / `q`, surfaced as raw `None`), or
        // Ctrl-C: all decline — never a crash.
        Ok(Some(false)) | Ok(None) => {
            eprintln!("[hs] Aborted — nothing was executed.");
            false
        }
        Err(dialoguer::Error::IO(e)) if e.kind() == std::io::ErrorKind::Interrupted => {
            eprintln!("[hs] Aborted — confirmation interrupted (Ctrl-C).");
            false
        }
        Err(dialoguer::Error::IO(_)) => {
            eprintln!("[hs] Aborted — confirmation unavailable (not a TTY).");
            false
        }
    }
}

/// Spawn `<shell> -c <cmd>` inheriting stdio, wait, and return the
/// child's exit code.
///
/// The interactive shell path is injected by the caller — resolved from
/// the `SHELL` environment variable in [`execute_safely_with`], with
/// `/bin/sh` as the fallback — so tests can substitute a throwaway mock
/// shell by argument without mutating the global environment. Using the
/// user's real shell means bash/zsh-isms — `[[ ... ]]`, process
/// substitution `<(...)`, arrays — run exactly as the user typed them
/// historically rather than failing under a minimal `/bin/sh` like dash.
fn run_child(cmd_string: &str, shell: &str) -> Result<i32, HsError> {
    println!("\x1b[2m[hs] Running: {cmd_string}\x1b[0m");

    let status = std::process::Command::new(shell)
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
    fn redacted_command_triggers_confirmation() {
        // A `[REDACTED]` secret leaks no credentials, so the command never
        // spells out a real token and is harmless to truly run — but the
        // guard must still demand confirmation before letting it through.
        // Quoting prevents `[REDACTED]` from being glob-expanded by the shell.
        let redacted = "printf '%s\\n' '[REDACTED] secret-token'";

        // Declining aborts with Cancelled, exactly once, no subprocess.
        let mut probe = Probe::from(false);
        let result = execute_safely_with(redacted, |cmd| {
            probe.called_with.push(cmd.to_string());
            probe.answer
        });
        assert!(
            matches!(result, Err(HsError::Cancelled)),
            "declined redacted command must yield Cancelled, got: {result:?}"
        );
        assert_eq!(probe.called_with, vec![redacted.to_string()]);

        // Confirming proceeds and runs the command.
        let mut probe = Probe::from(true);
        let code = execute_safely_with(redacted, |cmd| {
            probe.called_with.push(cmd.to_string());
            probe.answer
        })
        .expect("confirmed redacted command must run");
        assert_eq!(code, 0, "child exit code must propagate");
        assert_eq!(probe.called_with, vec![redacted.to_string()]);
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

    /// Verify that when a high-risk command is *declined*, the
    /// confirmation callback is invoked exactly once, the result is
    /// strictly `Err(HsError::Cancelled)`, and **no subprocess is
    /// spawned** (the callback return controls the branch, and `run_child`
    /// is never reached on the `Cancelled` path).
    #[test]
    fn high_risk_declined_cancels_without_spawning_subprocess() {
        let mut probe = Probe::from(false);
        let dangerous = "dd if=/dev/sda of=/dev/null bs=1M count=1";

        let result = execute_safely_with(dangerous, |cmd| {
            probe.called_with.push(cmd.to_string());
            probe.answer
        });

        // 1. Must return exactly HsError::Cancelled.
        assert!(
            matches!(result, Err(HsError::Cancelled)),
            "declined high-risk command must yield Cancelled, got: {result:?}"
        );

        // 2. Confirmation was asked exactly once with the correct string.
        assert_eq!(probe.called_with.len(), 1, "confirm called exactly once");
        assert_eq!(probe.called_with[0], dangerous);

        // 3. No subprocess was launched — if `run_child` had run, it would
        //    have returned `Ok(exit_code)`, never `Err(Cancelled)`.
        //    The `Cancelled` variant exists solely because the declined
        //    branch never reaches `run_child`.
    }

    #[test]
    fn run_child_honors_injected_shell() {
        use std::fs;
        use std::os::unix::fs::PermissionsExt;

        // A throwaway "shell" that records its invocation, then delegates
        // to the real /bin/sh so the child command still runs.
        let tmp = std::env::temp_dir().join(format!(
            "hs_guard_shell_{:?}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&tmp).unwrap();

        let marker = tmp.join("fake_shell_invoked");
        let fake_shell = tmp.join("fake_shell.sh");
        fs::write(
            &fake_shell,
            format!(
                "#!/bin/sh\nprintf 'fake-shell-invoked\\n' >> '{}'\nexec /bin/sh \"$@\"\n",
                marker.display()
            ),
        )
        .unwrap();
        let mut perms = fs::metadata(&fake_shell).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&fake_shell, perms).unwrap();
        let fake_shell = fake_shell.to_str().unwrap().to_string();

        // The shell is injected purely by argument — no global `SHELL`
        // mutation, hence no `unsafe`, so this is safe to run inside the
        // multi-threaded test harness.
        let result = run_child("true", &fake_shell);

        assert_eq!(result.unwrap_or(-1), 0, "child must run and exit 0");
        let log = fs::read_to_string(&marker).unwrap_or_default();
        assert!(
            log.contains("fake-shell-invoked"),
            "run_child must have executed via the injected shell, marker log was: {log:?}"
        );

        let _ = fs::remove_dir_all(&tmp);
    }
}
