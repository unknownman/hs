//! System diagnostics (`hs doctor`).
//!
//! Reports installation health with colored `[✓]`/`[✗]` markers:
//! database path + permissions, WAL mode + integrity, database volume,
//! and shell-hook presence in `~/.bashrc` / `~/.zshrc`.
//!
//! Exits `1` if any hard check fails, `0` otherwise. Shell-hook checks
//! are soft (a user may legitimately only use one shell).

use std::path::{Path, PathBuf};

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
                        let size = human_size(database_size_plus_sidecars(db_path));
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
    //    macOS defaults to *login* shells, so the hook commonly lives in
    //    `~/.bash_profile` / `~/.zprofile`; check those as fallbacks so
    //    macOS users never get a false negative for an installed hook.
    let home = dirs::home_dir();
    let bash = hook_status(home.as_deref(), &[".bashrc", ".bash_profile"]);
    let zsh = hook_status(home.as_deref(), &[".zshrc", ".zprofile"]);
    println!("{bash}");
    println!("{zsh}");

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

/// Total bytes the database occupies on disk.
///
/// SQLite runs in WAL mode, so live pages live in `hs.db-wal` (and
/// `hs.db-shm`, the shared-memory index) until a checkpoint. The main
/// `hs.db` plainly understates real disk usage; count all three files.
fn database_size_plus_sidecars(db_path: &Path) -> u64 {
    let mut bytes = std::fs::metadata(db_path).map(|m| m.len()).unwrap_or(0);
    for sidecar in ["db-wal", "db-shm"] {
        if let Ok(metadata) = std::fs::metadata(db_path.with_extension(sidecar)) {
            bytes = bytes.saturating_add(metadata.len());
        }
    }
    bytes
}

/// Describe whether any of the candidate rc files sources the `hs` hook.
///
/// The first candidate is the interactive rc (`.bashrc`/`.zshrc`); the
/// second is the login-shell rc (`.bash_profile`/`.zprofile`), which is
/// where hooks live when shells run as login shells (the macOS default).
/// A hook sourced in *any* existing candidate satisfies the check.
fn hook_status(home: Option<&Path>, rc_files: &[&str]) -> String {
    let Some(home) = home else {
        return format!("{CHECK}no home directory");
    };
    let candidates: Vec<PathBuf> = rc_files.iter().map(|f| home.join(f)).collect();
    let existing: Vec<&PathBuf> = candidates.iter().filter(|p| p.is_file()).collect();

    if existing.is_empty() {
        let listed = candidates
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join(", ");
        return format!("{CROSS}not found: {listed} (soft — skipped)");
    }

    for path in &existing {
        let source = std::fs::read_to_string(*path).unwrap_or_default();
        let sourced = source.lines().any(|line| {
            line.contains("hs init")
                || line.contains("hooks/bash.sh")
                || line.contains("hooks/zsh.sh")
        });
        if sourced {
            return format!("{CHECK}hook sourced in {}", path.display());
        }
    }

    let listed = existing
        .iter()
        .map(|p| p.display().to_string())
        .collect::<Vec<_>>()
        .join(", ");
    format!("{CROSS}no `hs init` hook found in {listed} (soft — skipped)")
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

    #[test]
    fn hook_status_prefers_primary_rc() {
        let tmp = tempfile::tempdir().unwrap();
        let bashrc = tmp.path().join(".bashrc");
        let bash_profile = tmp.path().join(".bash_profile");
        std::fs::write(&bashrc, "eval \"$(hs init bash)\"").unwrap();
        std::fs::write(&bash_profile, "eval \"$(hs init bash)\"").unwrap();
        let result = hook_status(Some(tmp.path()), &[".bashrc", ".bash_profile"]);
        assert!(
            result.starts_with(CHECK),
            "primary .bashrc must satisfy the check"
        );
        assert!(
            result.contains(".bashrc"),
            "primary path must be named, got: {result}"
        );
    }

    #[test]
    fn hook_status_falls_back_to_login_rc() {
        let tmp = tempfile::tempdir().unwrap();
        let bash_profile = tmp.path().join(".bash_profile");
        std::fs::write(&bash_profile, "eval \"$(hs init bash)\"").unwrap();
        // .bashrc intentionally absent — simulates a macOS login shell.
        let result = hook_status(Some(tmp.path()), &[".bashrc", ".bash_profile"]);
        assert!(
            result.starts_with(CHECK),
            "login-shell .bash_profile must satisfy the check"
        );
        assert!(
            result.contains(".bash_profile"),
            "fallback path must be named, got: {result}"
        );
    }

    #[test]
    fn hook_status_lists_all_missing_rc_files() {
        let tmp = tempfile::tempdir().unwrap();
        let result = hook_status(Some(tmp.path()), &[".bashrc", ".bash_profile"]);
        assert!(result.starts_with(CROSS), "missing files must report cross");
        assert!(
            result.contains(".bashrc") && result.contains(".bash_profile"),
            "both candidates must appear in the missing report: {result}"
        );
    }

    #[test]
    fn database_size_includes_wal_and_shm_sidecars() {
        let tmp = std::env::temp_dir().join(format!(
            "hs_doctor_size_{:?}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&tmp).unwrap();

        let db = tmp.join("hs.db");
        std::fs::write(&db, vec![0u8; 400]).unwrap();
        std::fs::write(db.with_extension("db-wal"), vec![0u8; 600]).unwrap();
        std::fs::write(db.with_extension("db-shm"), vec![0u8; 32 * 1024]).unwrap();

        assert_eq!(database_size_plus_sidecars(&db), 400 + 600 + 32 * 1024);

        // Missing sidecars are simply skipped; only the main file counts.
        std::fs::remove_file(db.with_extension("db-wal")).unwrap();
        assert_eq!(database_size_plus_sidecars(&db), 400 + 32 * 1024);

        // Missing main file reports zero rather than panicking. Use a
        // distinct name — with_extension(..) on `hs.db-nope` would rewrite
        // to `hs.db-shm`, which still exists.
        let other = tmp.join("other.db");
        assert_eq!(database_size_plus_sidecars(&other), 0);

        let _ = std::fs::remove_dir_all(&tmp);
    }
}
