//! Non-interactive output formatting for `hs`.
//!
//! Used when the user passes `--print` or when stdout is not a terminal
//! (piped output). Renders ranked results as a compact, colored table —
//! commands belonging to the *current* project are highlighted so the
//! "what worked *here*?" signal survives plain-text output.

use std::io::{self, IsTerminal as _};

use comfy_table::modifiers::UTF8_ROUND_CORNERS;
use comfy_table::presets::UTF8_FULL;
use comfy_table::{Cell, Color, Table};

use crate::ranking::RankedCommand;

/// Print a ranked list as a colored table to stdout.
///
/// `current_project_id` (when `Some`) marks rows from *this* project so
/// they stand out from global/other commands. Colors are only emitted
/// when stdout is a terminal, so piped output stays clean.
pub fn print_results_table(results: &[RankedCommand], current_project_id: Option<i64>) {
    if results.is_empty() {
        println!("No matching commands in history.");
        return;
    }

    let colored = io::stdout().is_terminal();

    let mut table = Table::new();
    table
        .load_preset(UTF8_FULL)
        .apply_modifier(UTF8_ROUND_CORNERS)
        .set_header(["#", "Command", "Score", "Runs", "Success", "Last Run"]);

    for (i, r) in results.iter().enumerate() {
        let in_project = current_project_id.is_some() && r.project_id == current_project_id;
        let total = r.success_count + r.fail_count;
        let success_rate = if total == 0 {
            "–".to_string()
        } else {
            format!("{:.0}%", 100.0 * r.success_count as f64 / total as f64)
        };

        let rank_marker = if in_project { "→" } else { " " };
        let cells: Vec<Cell> = vec![
            Cell::new(format!("{rank_marker}{}", i + 1)),
            Cell::new(&r.cmd_string),
            Cell::new(format!("{:.2}", r.final_score)),
            Cell::new(total),
            Cell::new(success_rate),
            Cell::new(short_date(r.last_executed_at.as_deref())),
        ];

        let cells = if colored {
            let fg = if in_project {
                Color::Green
            } else {
                Color::DarkGrey
            };
            cells.into_iter().map(|cell| cell.fg(fg)).collect()
        } else {
            cells
        };

        table.add_row(cells);
    }

    println!("{table}");
    println!(
        "{}",
        if colored {
            "\x1b[2m→ commands from the current project\x1b[0m"
        } else {
            "→ commands from the current project"
        }
    );
}

/// Trim an RFC 3339 timestamp to `YYYY-MM-DD HH:MM` (UTC).
///
/// Falls back to the raw string if it cannot be parsed (e.g. `None` → `–`).
fn short_date(ts: Option<&str>) -> String {
    match ts {
        None => "–".to_string(),
        Some(raw) => chrono::DateTime::parse_from_rfc3339(raw)
            .map(|dt| dt.format("%Y-%m-%d %H:%M").to_string())
            .unwrap_or_else(|_| raw.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(project_id: Option<i64>, score: f64) -> RankedCommand {
        RankedCommand {
            command_id: 1,
            cmd_string: "cargo test --all".to_string(),
            project_id,
            final_score: score,
            success_count: 8,
            fail_count: 2,
            last_executed_at: Some("2026-09-08T10:11:12Z".to_string()),
        }
    }

    #[test]
    fn short_date_trims_to_minute_precision() {
        assert_eq!(short_date(Some("2026-09-08T10:11:12Z")), "2026-09-08 10:11");
        assert_eq!(short_date(None), "–");
    }

    #[test]
    fn empty_results_prints_message_without_panicking() {
        print_results_table(&[], None);
    }

    #[test]
    fn table_renders_mixed_projects() {
        let rows = vec![
            sample(Some(3), 4.0),
            sample(Some(9), 2.0),
            sample(None, 1.0),
        ];
        print_results_table(&rows, Some(3));
    }
}
