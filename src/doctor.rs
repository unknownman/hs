//! System diagnostics (`hs doctor`).
//!
//! Reports installation health with colored `[✓]`/`[✗]` markers:
//! database path + permissions, WAL mode + integrity, database volume,
//! and shell-hook presence in `~/.bashrc` / `~/.zshrc`.
//!
//! Exits `1` if any hard check fails, `0` otherwise. Shell-hook checks
//! are soft (a user may legitimately only use one shell).

use std::path::Path;

const CHECK: &str = "\x1b[32m[✓]\x1b[0m ";
const CROSS: &str = "\x1b[31m[✗]\x1b[0m ";

/// Run the full diagnostic report. Returns the process exit code.
pub fn run_doctor(db_path: &Path) -> i32 {
    let mut failed = false;

    // 1. Database file + parent-directory permissions.
    let exists = db_path.is_file();
    if exists {
        println!("{CHECK}Database exists at {}", db_path.display());
    } else {
        println!("{CROSS}Database missing at {}", db_path.display());
        println!("    Run any command that hs can capture (hooks installed via `hs init`).");
        failed = true;
    }

    let parent = db_path.parent().unwrap_or_else(|| Path::new("."));
    if writable(parent) {
        println!("{CHECK}Parent directory writable: {}", parent.display());
    } else {
        println!("{CROSS}Parent directory NOT writable: {}", parent.display());
        failed = true;
    }

    if exists {
        match crate::db::init_db(db_path) {
            Ok(pool) => {
                // 2. WAL mode + integrity.
                match pool.get() {
                    Ok(conn) => {
                        let mode: String = conn
                            .query_row("PRAGMA journal_mode", [], |r| r.get(0))
                            .unwrap_or_default();
                        if mode == "wal" {
                            println!("{CHECK}Journal mode: wal");
                        } else {
                            println!("{CROSS}Journal mode is `{mode}` (expected `wal`)");
                            failed = true;
                        }

                        let integrity: String = conn
                            .query_row("PRAGMA integrity_check", [], |r| r.get(0))
                            .unwrap_or_else(|_| "error".to_string());
                        if integrity == "ok" {
                            println!("{CHECK}Integrity check: ok");
                        } else {
                            println!("{CROSS}Integrity check failed: {integrity}");
                            failed = true;
                        }

                        // 3. Volume.
                        let commands: i64 = conn
                            .query_row("SELECT COUNT(*) FROM commands", [], |r| r.get(0))
                            .unwrap_or(0);
                        let executions: i64 = conn
                            .query_row("SELECT COUNT(*) FROM executions", [], |r| r.get(0))
                            .unwrap_or(0);
                        let size = std::fs::metadata(db_path)
                            .map(|m| human_size(m.len()))
                            .unwrap_or_else(|_| "?".to_string());
                        println!(
                            "{CHECK}{} unique commands · {} executions · {} on disk",
                            thousands(commands),
                            thousands(executions),
                            size
                        );
                    }
                    Err(e) => {
                        println!("{CROSS}Could not open database connection: {e}");
                        failed = true;
                    }
                }
            }
            Err(e) => {
                println!("{CROSS}Could not open database: {e}");
                failed = true;
            }
        }
    }

    // 4. Shell hooks (soft check — user may only use one shell).
    let bashrc = hook_status(dirs::home_dir().map(|h| h.join(".bashrc")).as_deref());
    let zshrc = hook_status(dirs::home_dir().map(|h| h.join(".zshrc")).as_deref());
    println!("{bashrc}");
    println!("{zshrc}");

    println!();
    if failed {
        println!("\x1b[31mhs doctor: some checks failed.\x1b[0m");
        1
    } else {
        println!("\x1b[32mhs doctor: everything looks good.\x1b[0m");
        0
    }
}

/// True if a file can be created and removed inside `dir`.
fn writable(dir: &Path) -> bool {
    let probe = dir.join(format!(".hs_doctor_probe_{}", std::process::id()));
    match std::fs::File::create(&probe) {
        Ok(file) => {
            drop(file);
            std::fs::remove_file(&probe).is_ok()
        }
        Err(_) => false,
    }
}

/// Describe whether `rc_path` sources the `hs` hook.
fn hook_status(rc_path: Option<&Path>) -> String {
    let Some(path) = rc_path else {
        return format!("{CHECK}no home directory");
    };
    if !path.is_file() {
        return format!("{CROSS}not found: {} (soft — skipped)", path.display());
    }
    let source = std::fs::read_to_string(path).unwrap_or_default();
    let sourced = source.lines().any(|line| {
        line.contains("hs init") || line.contains("hooks/bash.sh") || line.contains("hooks/zsh.sh")
    });
    if sourced {
        format!("{CHECK}hook sourced in {}", path.display())
    } else {
        format!(
            "{CROSS}no `hs init` hook found in {} (soft — skipped)",
            path.display()
        )
    }
}

/// `1234567` → `1,234,567`.
fn thousands(n: i64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// Human-readable file size: `4.2 MB`, `420 KB`, `900 B`.
fn human_size(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = 1024.0 * 1024.0;
    let size = bytes as f64;
    if size >= MB {
        format!("{:.1} MB", size / MB)
    } else if size >= KB {
        format!("{:.0} KB", size / KB)
    } else {
        format!("{bytes} B")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thousands_formatting() {
        assert_eq!(thousands(0), "0");
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(1000), "1,000");
        assert_eq!(thousands(1234567), "1,234,567");
    }

    #[test]
    fn human_size_formatting() {
        assert_eq!(human_size(900), "900 B");
        assert_eq!(human_size(1024), "1 KB");
        assert_eq!(human_size(420 * 1024), "420 KB");
        assert_eq!(human_size(4_200_000), "4.0 MB");
    }
}
