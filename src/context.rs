//! Context Engine — understands *where* commands run and *how dangerous*
//! they are.
//!
//! Three responsibilities:
//!
//! 1. **Project detection** — locate the nearest `.git` boundary.
//! 2. **Stack detection** — infer the tech stack from indicator files.
//! 3. **Risk classification** — flag obviously destructive commands.
//!
//! All functions are pure and take raw inputs, returning enriched
//! results.  Nothing here touches the database or the shell.

#![allow(dead_code)]

use regex::Regex;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

// ────────────────────────────────────────────────────────────────────────
// Project detection
// ────────────────────────────────────────────────────────────────────────

/// Walk upwards from `current_dir`, looking for a `.git` boundary.
///
/// Returns the first ancestor that contains a `.git` entry (directory
/// for a regular checkout, file for a worktree).  Returns `None` if no
/// `.git` exists between `current_dir` and the filesystem root.
pub fn find_project_root(current_dir: &Path) -> Option<PathBuf> {
    current_dir
        .ancestors()
        .find(|dir| dir.join(".git").exists())
        .map(Path::to_path_buf)
}

// ────────────────────────────────────────────────────────────────────────
// Stack detection
// ────────────────────────────────────────────────────────────────────────

/// Detect the tech stack of a project from indicator files.
///
/// Returns a deterministic, de-duplicated list of tags in a fixed order.
/// The presence of a marker file maps to a tag:
///
/// | File | Tag |
/// |------|-----|
/// | `Cargo.toml` | `rust` |
/// | `package.json` | `node` |
/// | `Dockerfile` | `docker` |
/// | `pyproject.toml` | `python` |
/// | `requirements.txt` | `python` |
/// | `go.mod` | `go` |
pub fn detect_stacks(project_root: &Path) -> Vec<String> {
    // (indicator file, tag) — ordered so results are deterministic.
    const CHECKS: &[(&str, &str)] = &[
        ("Cargo.toml", "rust"),
        ("package.json", "node"),
        ("Dockerfile", "docker"),
        ("pyproject.toml", "python"),
        ("requirements.txt", "python"),
        ("go.mod", "go"),
    ];

    let mut stacks: Vec<String> = Vec::new();
    for (file, tag) in CHECKS {
        if project_root.join(file).exists() && !stacks.contains(&(*tag).to_string()) {
            stacks.push((*tag).to_string());
        }
    }
    stacks
}

// ────────────────────────────────────────────────────────────────────────
// Risk classification
// ────────────────────────────────────────────────────────────────────────

/// Severity classification for a command string.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RiskLevel {
    /// No destructive pattern detected.
    Safe,
    /// Potentially destructive — the safe-execution guard should prompt.
    High,
}

/// Cached compiled risk patterns.
static RISK_PATTERNS: OnceLock<Vec<Regex>> = OnceLock::new();

/// Compile all risk patterns exactly once.
fn risk_patterns() -> &'static [Regex] {
    RISK_PATTERNS.get_or_init(|| {
        RISK_PATTERN_LIST
            .iter()
            .map(|p| Regex::new(p).expect("invalid risk pattern"))
            .collect()
    })
}

/// Classify a command as [`RiskLevel::High`] if it matches any
/// destructive pattern, otherwise [`RiskLevel::Safe`].
pub fn analyze_risk(cmd_string: &str) -> RiskLevel {
    if risk_patterns().iter().any(|re| re.is_match(cmd_string)) {
        RiskLevel::High
    } else {
        RiskLevel::Safe
    }
}

/// Destructive command patterns.
///
/// Matches are substring-based (not anchored), so a destructive command
/// nested inside a longer line (e.g. `sudo rm -rf /`) is still caught.
const RISK_PATTERN_LIST: &[&str] = &[
    // rm -rf targeting a root-relative path (e.g. `rm -rf /`, `rm -fr /*`)
    r"\brm\s+-[a-zA-Z]*[rRfF][a-zA-Z]*\s+/\S*",
    // git force push (flags may appear after branches: `git push origin -f`)
    r"\bgit\s+push\s+(?:[^\s]+\s+)*(?:--force\w*|-f\b)",
    // DROP TABLE / DROP DATABASE (case-insensitive per spec)
    r"(?i)\bdrop\s+(?:table|database)\b",
    // Filesystem format
    r"\bmkfs\b",
    // Disk overwrite
    r"\bdd\s+if=",
];

#[cfg(test)]
mod tests {
    use super::*;

    // ── find_project_root ────────────────────────────────────────────

    #[test]
    fn finds_project_root_from_deep_subdirectory() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();

        // Create the .git directory at the project root.
        std::fs::create_dir_all(root.join(".git")).unwrap();

        // Nested structure several levels deep.
        let nested = root.join("src").join("modules").join("network").join("tcp");
        std::fs::create_dir_all(&nested).unwrap();

        assert_eq!(find_project_root(&nested), Some(root.to_path_buf()));
    }

    #[test]
    fn returns_project_root_when_called_from_root() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();

        std::fs::create_dir_all(root.join(".git")).unwrap();

        assert_eq!(find_project_root(root), Some(root.to_path_buf()));
    }

    #[test]
    fn returns_none_outside_git_repo() {
        let tmp = tempfile::tempdir().unwrap();
        let nested = tmp.path().join("a").join("b").join("c");
        std::fs::create_dir_all(&nested).unwrap();

        assert_eq!(find_project_root(&nested), None);
    }

    #[test]
    fn respects_nearest_git_boundary() {
        let tmp = tempfile::tempdir().unwrap();
        let outer = tmp.path();
        let inner = outer.join("vendor").join("inner-proj");

        // Two nested git repos — must resolve to the INNER one.
        std::fs::create_dir_all(outer.join(".git")).unwrap();
        std::fs::create_dir_all(inner.join(".git")).unwrap();

        assert_eq!(find_project_root(&inner), Some(inner.to_path_buf()));
    }

    // ── detect_stacks ────────────────────────────────────────────────

    #[test]
    fn detects_known_marker_files() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();

        std::fs::write(root.join("Cargo.toml"), "").unwrap();
        std::fs::write(root.join("package.json"), "{}").unwrap();
        std::fs::write(root.join("Dockerfile"), "").unwrap();
        std::fs::write(root.join("pyproject.toml"), "").unwrap();

        let stacks = detect_stacks(root);
        assert_eq!(stacks, vec!["rust", "node", "docker", "python"]);
    }

    #[test]
    fn python_markers_are_deduplicated() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();

        std::fs::write(root.join("pyproject.toml"), "").unwrap();
        std::fs::write(root.join("requirements.txt"), "").unwrap();

        let stacks = detect_stacks(root);
        assert_eq!(stacks, vec!["python"]);
    }

    #[test]
    fn detects_go_project() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();

        std::fs::write(root.join("go.mod"), "").unwrap();

        assert_eq!(detect_stacks(root), vec!["go"]);
    }

    #[test]
    fn empty_for_unknown_project() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(detect_stacks(tmp.path()).is_empty());
    }

    // ── analyze_risk ─────────────────────────────────────────────────

    #[test]
    fn destructive_commands_are_high_risk() {
        use RiskLevel::High;

        assert_eq!(analyze_risk("rm -rf /"), High);
        assert_eq!(analyze_risk("rm -fr /*"), High);
        assert_eq!(analyze_risk("sudo rm -rf /"), High);
        assert_eq!(analyze_risk("git push -f"), High);
        assert_eq!(analyze_risk("git push --force"), High);
        assert_eq!(analyze_risk("git push origin --force"), High);
        assert_eq!(analyze_risk("DROP TABLE users;"), High);
        assert_eq!(analyze_risk("drop database acme"), High);
        assert_eq!(analyze_risk("mkfs.ext4 /dev/sdb1"), High);
        assert_eq!(analyze_risk("dd if=/dev/zero of=/dev/sda"), High);
    }

    #[test]
    fn benign_commands_are_safe() {
        use RiskLevel::Safe;

        assert_eq!(analyze_risk("rm file.txt"), Safe);
        assert_eq!(analyze_risk("rm -f build.log"), Safe);
        assert_eq!(analyze_risk("git push"), Safe);
        assert_eq!(analyze_risk("git push origin main"), Safe);
        assert_eq!(analyze_risk("select * from table;"), Safe);
        assert_eq!(analyze_risk("docker build -t app ."), Safe);
    }
}
