//! Shell history import (`hs import`).
//!
//! Onboards existing users by bulk-loading their legacy `~/.zsh_history`
//! or `~/.bash_history` in a single transaction (see
//! [`Store::import_entries`](crate::db::repository::Store::import_entries)).
//!
//! # Formats
//!
//! * **Bash** — one history entry per line.
//! * **Zsh extended history** — `: <unixtime>:<elapsed>;<command>` with
//!   the command allowed to span multiple lines (continuation lines until
//!   the next `: ts:...;` header). Lines without a header are appended to
//!   the current entry, or treated as a bare command when no entry is open.

use std::fs;
use std::io;
use std::path::PathBuf;

use chrono::{DateTime, Utc};

use crate::db::repository::Store;
use crate::error::HsError;
use crate::models::ImportEntry;

/// Default working directory recorded on imported executions (legacy
/// history has no cwd metadata). Falls back to `/` if home is unknown.
fn fallback_working_dir() -> String {
    dirs::home_dir()
        .map(|h| h.to_string_lossy().into_owned())
        .unwrap_or_else(|| "/".to_string())
}

/// Run `hs import`, returning the process exit code.
///
/// An explicit `path` overrides automatic detection
/// (`~/.zsh_history`, then `~/.bash_history`).
pub fn run_import(store: &Store, explicit: Option<PathBuf>) -> Result<i32, HsError> {
    let source = match explicit {
        Some(path) => path,
        None => detect_history_file().ok_or_else(|| {
            HsError::IoError(io::Error::new(
                io::ErrorKind::NotFound,
                "no ~/.zsh_history or ~/.bash_history found",
            ))
        })?,
    };

    if !source.is_file() {
        println!("[hs] No history file found at {}", source.display());
        return Ok(1);
    }

    let contents = fs::read_to_string(&source)?;
    let is_zsh = source
        .file_name()
        .map(|name| name.to_string_lossy().contains("zsh"))
        .unwrap_or(false);

    let entries = if is_zsh {
        parse_zsh(&contents)
    } else {
        parse_bash(&contents)
    };

    let report = store.import_entries(entries, &fallback_working_dir())?;
    println!(
        "[hs] Imported {} commands ({} duplicates skipped, {} secrets redacted)",
        report.imported, report.duplicates, report.redacted
    );
    Ok(0)
}

/// Locate an existing shell history file, preferring zsh.
fn detect_history_file() -> Option<PathBuf> {
    let home = dirs::home_dir()?;
    for name in [".zsh_history", ".bash_history"] {
        let candidate = home.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// Parse bash history: one command per line, no timestamps.
fn parse_bash(contents: &str) -> Vec<ImportEntry> {
    contents
        .lines()
        .map(str::trim_end)
        .filter(|line| !line.trim().is_empty())
        .map(|line| ImportEntry {
            cmd: line.to_string(),
            executed_at: None,
        })
        .collect()
}

/// Parse zsh extended history, joining multi-line entries.
fn parse_zsh(contents: &str) -> Vec<ImportEntry> {
    let mut entries: Vec<ImportEntry> = Vec::new();
    let mut open: Option<ImportEntry> = None;

    for line in contents.lines() {
        if let Some((ts, body_offset)) = zsh_header(line) {
            if let Some(e) = open.take()
                && !e.cmd.trim().is_empty()
            {
                entries.push(e);
            }
            open = Some(ImportEntry {
                cmd: line[body_offset..].to_string(),
                executed_at: Some(ts),
            });
        } else if let Some(current) = open.as_mut() {
            current.cmd.push('\n');
            current.cmd.push_str(line);
        } else {
            // No open entry: a bare line (e.g. a history written without
            // extended headers) is its own command.
            entries.push(ImportEntry {
                cmd: line.to_string(),
                executed_at: None,
            });
        }
    }

    if let Some(e) = open.take()
        && !e.cmd.trim().is_empty()
    {
        entries.push(e);
    }

    entries
}

/// Parse a zsh extended-history header of the form
/// `: <unixtime>:<elapsed>;` (elapsed optional) and return the timestamp
/// plus the byte offset just past the `;`.
fn zsh_header(line: &str) -> Option<(DateTime<Utc>, usize)> {
    let rest = line.strip_prefix(':')?.trim_start();
    // Body begins after the first `;`.
    let semicolon = rest.find(';')?;
    let header = &rest[..semicolon];
    let ts_str = header.split(':').next()?.trim();
    let ts: i64 = ts_str.parse().ok()?;
    // Offset into the ORIGINAL line: ':' (1) + leading whitespace trimmed
    // from rest, + header length + ';'.
    let prefix_len = line.len() - rest.len();
    let body_offset = prefix_len + header.len() + 1;
    DateTime::from_timestamp(ts, 0).map(|dt| (dt, body_offset))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zsh_header_parses_with_and_without_elapsed() {
        let (ts, off) = zsh_header(": 1641024000:7;echo hi").unwrap();
        assert_eq!(ts.timestamp(), 1641024000);
        assert_eq!(&": 1641024000:7;echo hi"[off..], "echo hi");

        let (ts, off) = zsh_header(": 1641024000;bare").unwrap();
        assert_eq!(ts.timestamp(), 1641024000);
        assert_eq!(&": 1641024000;bare"[off..], "bare");

        assert!(zsh_header("not a header").is_none());
        assert!(zsh_header(": abc;x").is_none());
    }

    #[test]
    fn parse_bash_skips_blanks() {
        let entries = parse_bash("ls\n\nexport FOO=1\n  \ncd /");
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].cmd, "ls");
        assert!(entries[0].executed_at.is_none());
        assert_eq!(entries[2].cmd, "cd /");
    }

    #[test]
    fn parse_zsh_joins_multiline_and_records_timestamps() {
        // Build the fake key at runtime (push-protection: no secret
        // literals in committed source).
        let key = format!("AKIA{}", "B".repeat(16));
        let contents = format!(
            ": 1641024000:3;echo first\n\
             : 1641024100:2;npm install \\\n\
             lodash --no-save\n\
             : 1641024200:0;export AWS_ACCESS_KEY_ID={key} --dev\n\
             : 1641024300:1;bare\n"
        );
        let entries = parse_zsh(&contents);

        assert_eq!(entries.len(), 4);
        assert_eq!(entries[0].cmd, "echo first");
        assert_eq!(entries[0].executed_at.unwrap().timestamp(), 1641024000);
        assert_eq!(entries[1].cmd, "npm install \\\nlodash --no-save");
        assert_eq!(entries[1].executed_at.unwrap().timestamp(), 1641024100);
        // The secret is intentionally NOT redacted here — redaction is the
        // persistence layer's job (`Store::import_entries`). We only assert
        // the parser preserved the raw text.
        assert!(entries[2].cmd.contains(&key));
        assert_eq!(entries[3].cmd, "bare");
    }

    /// Phase 8 acceptance test: end-to-end import from a mock
    /// `.zsh_history` file via `run_import`.
    #[test]
    fn e2e_import_redacts_and_is_idempotent() {
        use crate::db::tests::test_pool;

        let dir = tempfile::tempdir().unwrap();
        let history_path = dir.path().join(".zsh_history");

        let key = format!("SK_TEST_{}{}", "x".repeat(18), "extra");
        let contents = format!(
            ": 1600000000:5;echo alpha\n\
             : 1600000100:1;git status\n\
             : 1600000200:2;curl -H \"Authorization: Bearer {key}\" https://api.x\n\
             : 1600000300:0;sleep 0\n"
        );
        fs::write(&history_path, contents).unwrap();

        let store = Store::new(test_pool());
        let code = run_import(&store, Some(history_path.clone())).unwrap();
        assert_eq!(code, 0);

        let stored = store.command_strings().unwrap();
        assert_eq!(stored.len(), 4, "each unique entry imported");
        assert!(
            stored
                .iter()
                .any(|c| c.contains("[REDACTED]") && !c.contains(&key)),
            "Bearer token must be redacted in the DB"
        );
        assert!(stored.contains(&"echo alpha".to_string()));
        assert!(stored.contains(&"git status".to_string()));

        // Timestamp fidelity: the first command carries its header time.
        assert_eq!(
            store.executed_at("echo alpha").unwrap().as_deref(),
            Some("2020-09-13T12:26:40Z")
        );

        // Idempotence: re-importing the same file changes nothing.
        let second = run_import(&store, Some(history_path)).unwrap();
        assert_eq!(second, 0);
        assert_eq!(
            store.command_strings().unwrap().len(),
            4,
            "no duplicate commands after re-import"
        );
        assert_eq!(
            store.execution_count().unwrap(),
            4,
            "no duplicate executions after re-import"
        );
    }
}
