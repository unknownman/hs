//! Recall & ranking engine — the mathematical heart of `hs`.
//!
//! Answers the core product question: *"What worked here before?"*
//! Given a flood of raw candidates from the database (up to ~500), this
//! module turns them into an ordered, context-aware list via a
//! **multiplicative scoring model**:
//!
//! ```text
//! final_score = base_bm25 × success × project × recency × frequency
//! ```
//!
//! * `base_bm25` — full-text relevance (FTS5 `bm25`, inverted so higher
//!   = better; sentinel `1.0` when there is no text query).
//! * `success`    — reliability: commands that usually work rank higher.
//! * `project`    — context: commands from the *current* project get a
//!   massive boost, so a global search still surfaces what you did here.
//! * `recency`    — freshness: what you ran recently is more relevant.
//! * `frequency`  — log-bandwidth: heavily reused commands are trusted.

#![allow(dead_code)]

use chrono::{DateTime, Duration, Utc};
use std::cmp::Ordering;

/// All user intent that shapes a search — the parameters for candidate
/// retrieval **and** ranking.
#[derive(Debug, Clone, PartialEq)]
pub struct SearchContext {
    /// Free-text query. `None` = "show me the best commands for this
    /// context" (no FTS relevance, pure behavioral ranking).
    pub query: Option<String>,
    /// The project the user is currently in (from capture/context).
    /// `None` when running outside any known project.
    pub current_project_id: Option<i64>,
    /// If `true`, ignore `current_project_id` for hard filtering but
    /// still use it for the soft project boost during ranking.
    pub global: bool,
    /// Only commands with zero failures (maps to `--ok`).
    pub ok_only: bool,
    /// Only commands that have failed at least once (maps to `--failed`).
    pub failed_only: bool,
    /// Only commands executed since `now` minus this SQLite relative-time
    /// modifier (maps to `--last`). Examples: `"-30 minutes"`,
    /// `"-1 hours"`, `"-2 days"`. Passed as a *parameterized* value
    /// (never interpolated) to prevent SQL injection.
    pub time_window: Option<String>,
}

/// A single row returned by [`crate::db::repository::Store::fetch_candidates`].
#[derive(Debug, Clone, PartialEq)]
pub struct RawCandidate {
    /// `commands.id`
    pub command_id: i64,
    /// The literal (redacted) command string.
    pub cmd_string: String,
    /// Owning project, `None` for commands run outside a project.
    pub project_id: Option<i64>,
    /// Executions with exit code 0.
    pub success_count: i64,
    /// Executions with non-zero exit code.
    pub fail_count: i64,
    /// When the command was last executed.
    pub last_executed_at: Option<DateTime<Utc>>,
    /// Raw FTS5 `bm25` score (`1.0` sentinel when no query was used).
    pub bm25: f64,
    /// Whether this command is pinned by the user.
    pub is_pinned: bool,
}

/// A candidate after final scoring, ready for the UI.
#[derive(Debug, Clone, PartialEq)]
pub struct RankedCommand {
    /// `commands.id`
    pub command_id: i64,
    /// The literal (redacted) command string.
    pub cmd_string: String,
    /// Owning project, `None` for commands run outside a project.
    /// Lets the UI group "current project" vs "global/other".
    pub project_id: Option<i64>,
    /// Final multiplicative score — sort by this, descending.
    pub final_score: f64,
    /// Executions with exit code 0.
    pub success_count: i64,
    /// Executions with non-zero exit code.
    pub fail_count: i64,
    /// When the command was last executed (RFC 3339).
    pub last_executed_at: Option<String>,
    /// Whether this command is pinned by the user.
    pub is_pinned: bool,
}

/// Rank raw candidates into a final descending-sorted list.
///
/// `current_project_id` is the soft-boost context (pass
/// `ctx.current_project_id` for consistency with retrieval).
pub fn rank_commands(
    candidates: Vec<RawCandidate>,
    current_project_id: Option<i64>,
) -> Vec<RankedCommand> {
    let mut ranked: Vec<RankedCommand> = candidates
        .into_iter()
        .map(|c| RankedCommand {
            final_score: score(&c, current_project_id),
            command_id: c.command_id,
            cmd_string: c.cmd_string,
            project_id: c.project_id,
            success_count: c.success_count,
            fail_count: c.fail_count,
            last_executed_at: c.last_executed_at.map(|dt| dt.to_rfc3339()),
            is_pinned: c.is_pinned,
        })
        .collect();

    ranked.sort_by(|a, b| {
        b.final_score
            .partial_cmp(&a.final_score)
            .unwrap_or(Ordering::Equal)
    });
    ranked
}

/// Total runs = successes + failures.
fn total_runs(c: &RawCandidate) -> i64 {
    c.success_count + c.fail_count
}

/// Normalize the raw FTS5 value into a positive "bigger = better" base.
///
/// SQLite's `bm25()` returns *negative* scores (less negative = better
/// match), so we invert them. The `1.0` sentinel (no text query) passes
/// through untouched so ranking is driven purely by behavior.
fn base_score(bm25: f64) -> f64 {
    if bm25 <= 0.0 { -bm25 } else { bm25 }
}

/// Reliability factor: reliability improves trust in a command.
///
/// * success rate > 0.8 → × 1.2
/// * success rate < 0.5 → × 0.5
/// * otherwise          → × 1.0
fn success_multiplier(c: &RawCandidate) -> f64 {
    let total = total_runs(c);
    if total == 0 {
        return 1.0;
    }
    let rate = c.success_count as f64 / total as f64;
    if rate > 0.8 {
        1.2
    } else if rate < 0.5 {
        0.5
    } else {
        1.0
    }
}

/// Context factor: commands from the current project are × 2.0.
fn project_multiplier(c: &RawCandidate, current_project_id: Option<i64>) -> f64 {
    match current_project_id {
        Some(pid) if c.project_id == Some(pid) => 2.0,
        _ => 1.0,
    }
}

/// Freshness factor based on when the command last ran.
///
/// * last 24 hours  → × 1.5
/// * last 7 days    → × 1.2
/// * older 90 days+ → × 0.8
/// * otherwise      → × 1.0
/// * never recorded → × 1.0 (neutral, can't judge age)
fn recency_multiplier(c: &RawCandidate) -> f64 {
    let Some(last) = c.last_executed_at else {
        return 1.0;
    };
    let age = Utc::now().signed_duration_since(last);
    if age < Duration::hours(24) {
        1.5
    } else if age < Duration::days(7) {
        1.2
    } else if age >= Duration::days(90) {
        0.8
    } else {
        1.0
    }
}

/// Repetition factor: 1 + log10(runs) × 0.1.
///
/// Diminishing returns — 1 run → 1.0, 10 runs → 1.1, 100 runs → 1.2.
fn frequency_multiplier(c: &RawCandidate) -> f64 {
    let total = total_runs(c);
    if total <= 0 {
        1.0
    } else {
        1.0 + (total as f64).log10() * 0.1
    }
}

/// Pin factor: pinned commands are boosted × 100 so they always surface
/// to the top of results, regardless of other scoring factors.
fn pin_multiplier(c: &RawCandidate) -> f64 {
    if c.is_pinned { 100.0 } else { 1.0 }
}

/// The full multiplicative score for one candidate.
fn score(c: &RawCandidate, current_project_id: Option<i64>) -> f64 {
    base_score(c.bm25)
        * success_multiplier(c)
        * project_multiplier(c, current_project_id)
        * recency_multiplier(c)
        * frequency_multiplier(c)
        * pin_multiplier(c)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    /// Build a behavioural baseline with every field identical except
    /// the ones the test wants to exercise.
    fn base_candidate() -> RawCandidate {
        RawCandidate {
            command_id: 1,
            cmd_string: "cargo build".to_string(),
            project_id: Some(7),
            success_count: 5,
            fail_count: 0,
            last_executed_at: Some(Utc::now()),
            bm25: -1.0,
            is_pinned: false,
        }
    }

    fn score_of(c: &RawCandidate, project: Option<i64>) -> f64 {
        score(c, project)
    }

    #[test]
    fn inverts_bm25_so_better_match_outscores() {
        let mut good = base_candidate();
        good.bm25 = -3.0; // better FTS match
        let mut weak = base_candidate();
        weak.bm25 = -2.0;

        let ranked = rank_commands(vec![weak, good.clone()], None);
        assert!(ranked[0].final_score > ranked[1].final_score);
        assert_eq!(ranked[0].command_id, good.command_id);
    }

    #[test]
    fn success_rate_drives_factor_in_both_directions() {
        let now = Utc::now();
        let reliable = RawCandidate {
            command_id: 1,
            cmd_string: "deploy".into(),
            project_id: Some(7),
            success_count: 50,
            fail_count: 1, // rate ≈ 0.98 > 0.8
            last_executed_at: Some(now),
            bm25: -1.0,
            is_pinned: false,
        };
        let unreliable = RawCandidate {
            command_id: 2,
            cmd_string: "deploy --danger".into(),
            project_id: Some(7),
            success_count: 1,
            fail_count: 9, // rate = 0.1 < 0.5
            last_executed_at: Some(now),
            bm25: -1.0,
            is_pinned: false,
        };

        let a = score_of(&reliable, None);
        let b = score_of(&unreliable, None);
        // 51 vs 10 runs → frequency ≈ (1.064)/(1.020) ≈ 1.043; success
        // factor ratio 1.2/0.5 = 2.4 → reliable must win comfortably.
        assert!(a / b > 2.0, "ratio was {a}/{b}");
    }

    #[test]
    fn project_boost_prefers_current_project() {
        let in_ctx = RawCandidate {
            project_id: Some(7),
            ..base_candidate()
        };
        let other = RawCandidate {
            command_id: 2,
            cmd_string: "deploy another".into(),
            project_id: Some(99),
            ..base_candidate()
        };

        let ranked = rank_commands(vec![other, in_ctx.clone()], Some(7));
        assert!(ranked[0].final_score > ranked[1].final_score);
        assert_eq!(ranked[0].command_id, 1);
        // Exact ratio = 2.0 (identical everything else).
        assert!(
            (ranked[0].final_score / ranked[1].final_score - 2.0).abs() < 1e-9,
            "expected 2.0x boost"
        );
    }

    #[test]
    fn recency_bands_multiply_scores() {
        let now = Utc::now();
        let mk = |id: i64, age: Duration| RawCandidate {
            command_id: id,
            cmd_string: format!("cmd {id}"),
            last_executed_at: Some(now - age),
            ..base_candidate()
        };

        let recent = mk(1, Duration::hours(2)); // × 1.5
        let week = mk(2, Duration::days(3)); // × 1.2
        let ancient = mk(3, Duration::days(100)); // × 0.8

        let ranked = rank_commands(vec![ancient.clone(), week.clone(), recent.clone()], None);
        assert_eq!(ranked[0].command_id, recent.command_id);
        assert_eq!(ranked[1].command_id, week.command_id);
        assert_eq!(ranked[2].command_id, ancient.command_id);

        // All else identical → ratios are exactly 1.5, 1.2, 0.8.
        let r = score_of(&recent, None);
        let w = score_of(&week, None);
        let a = score_of(&ancient, None);
        assert!((r / w - 1.25).abs() < 1e-9);
        assert!((w / a - 1.5).abs() < 1e-9);
    }

    #[test]
    fn frequency_follows_log10() {
        let now = Utc::now();
        let hot = RawCandidate {
            command_id: 1,
            cmd_string: "hot".into(),
            success_count: 100,
            fail_count: 0,
            last_executed_at: Some(now),
            bm25: -1.0,
            ..base_candidate()
        };
        let cold = RawCandidate {
            command_id: 2,
            cmd_string: "cold".into(),
            success_count: 2,
            fail_count: 0,
            last_executed_at: Some(now),
            bm25: -1.0,
            ..base_candidate()
        };

        let ranked = rank_commands(vec![cold.clone(), hot.clone()], None);
        assert_eq!(ranked[0].command_id, hot.command_id);

        let expected_ratio = (1.0 + 2.0 * 0.1) / (1.0 + f64::log10(2.0) * 0.1);
        let ratio = score_of(&hot, None) / score_of(&cold, None);
        assert!((ratio - expected_ratio).abs() < 1e-9);
    }

    #[test]
    fn no_data_fields_are_neutral() {
        let unknown = RawCandidate {
            command_id: 1,
            success_count: 0,
            fail_count: 0,
            last_executed_at: None,
            bm25: -1.0,
            ..base_candidate()
        };

        // bm25 -1.0 → base 1.0; every multiplier neutral → score 1.0.
        assert_eq!(score_of(&unknown, None), 1.0);
    }

    #[test]
    fn rank_is_stable_and_empty_input_empty_output() {
        assert!(rank_commands(vec![], None).is_empty());
        let now = Utc::now();
        let one = RawCandidate {
            last_executed_at: Some(Utc.with_ymd_and_hms(2020, 1, 1, 0, 0, 0).unwrap()),
            ..base_candidate()
        };
        let two = RawCandidate {
            command_id: 2,
            last_executed_at: Some(now),
            ..base_candidate()
        };
        let ranked = rank_commands(vec![one, two], None);
        assert_eq!(ranked.len(), 2);
    }

    #[test]
    fn pinned_command_with_zero_runs_outranks_unpinned_with_fifty_runs() {
        let now = Utc::now();
        let pinned = RawCandidate {
            command_id: 1,
            cmd_string: "pinned cmd".into(),
            success_count: 0,
            fail_count: 0,
            last_executed_at: Some(now),
            bm25: -1.0,
            is_pinned: true,
            ..base_candidate()
        };
        let unpinned = RawCandidate {
            command_id: 2,
            cmd_string: "popular unpinned cmd".into(),
            success_count: 50,
            fail_count: 0,
            last_executed_at: Some(now),
            bm25: -1.0,
            is_pinned: false,
            ..base_candidate()
        };

        let ranked = rank_commands(vec![unpinned.clone(), pinned.clone()], None);
        assert_eq!(ranked[0].command_id, 1, "pinned must be first");
        assert!(ranked[0].is_pinned);
        // Pin boost ×100 overwhelms the frequency boost from 50 runs.
        assert!(
            ranked[0].final_score > ranked[1].final_score,
            "pinned (0 runs) must outrank unpinned (50 runs): {} vs {}",
            ranked[0].final_score,
            ranked[1].final_score
        );
    }

    #[test]
    fn pin_boost_is_exactly_100x() {
        let now = Utc::now();
        let base = RawCandidate {
            last_executed_at: Some(now),
            ..base_candidate()
        };
        let mut pinned = base.clone();
        pinned.is_pinned = true;
        let mut unpinned = base;
        unpinned.command_id = 2;

        let ratio = score_of(&pinned, None) / score_of(&unpinned, None);
        assert!(
            (ratio - 100.0).abs() < 1e-9,
            "pin boost must be exactly 100x, got {ratio}"
        );
    }
}
