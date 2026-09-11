//! Non-interactive output formatting for `hs`.
//!
//! Used when the user passes `--print` or when stdout is not a terminal
//! (piped output). Renders ranked results as a compact, colored table —
//! commands belonging to the *current* project are highlighted so the
//! "what worked *here*?" signal survives plain-text output.

use std::io::{self, IsTerminal as _};

use comfy_table::modifiers::UTF8_ROUND_CORNERS;
use comfy_table::presets::UTF8_FULL;
use comfy_table::{Cell, CellAlignment, Color, Table};

use crate::models::PinnedCommand;
use crate::ranking::RankedCommand;

/// Maximum characters for a command shown in the one-line table/list.
pub const MAX_DISPLAY_CHARS: usize = 64;

/// Truncate `text` to at most `max_chars` display characters, appending
/// `…` when cut. Never returns a string longer than `max_chars` — this is
/// the guard against pathological 5,000-character commands exploding the
/// CLI table (and, via the TUI list, the terminal).
pub fn ellipsize(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let cut: String = text.chars().take(max_chars.saturating_sub(1)).collect();
    format!("{cut}…")
}

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

        // Pinned commands get a dedicated marker prefix so they are
        // recognizable in plain-text (piped) output too.
        let pin_marker = if r.is_pinned { "📌 " } else { "" };
        let rank_marker = if in_project { "→" } else { " " };
        let cells: Vec<Cell> = vec![
            Cell::new(format!("{rank_marker}{}", i + 1)),
            Cell::new(format!(
                "{pin_marker}{}",
                ellipsize(&r.cmd_string, MAX_DISPLAY_CHARS)
            )),
            Cell::new(format!("{:.2}", r.final_score)).set_alignment(CellAlignment::Right),
            Cell::new(total).set_alignment(CellAlignment::Right),
            Cell::new(success_rate).set_alignment(CellAlignment::Right),
            Cell::new(short_date(r.last_executed_at.as_deref()))
                .set_alignment(CellAlignment::Right),
        ];

        let cells = if colored {
            let fg = if r.is_pinned {
                Color::Yellow
            } else if in_project {
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

    let has_pinned = results.iter().any(|r| r.is_pinned);
    let has_current = results
        .iter()
        .any(|r| current_project_id.is_some() && r.project_id == current_project_id);

    let mut legend: Vec<String> = Vec::new();
    if has_pinned {
        legend.push(if colored {
            "\x1b[1;33m📌 pinned command\x1b[0m".to_string()
        } else {
            "📌 pinned command".to_string()
        });
    }
    if has_current {
        legend.push(if colored {
            "\x1b[2m→ commands from the current project\x1b[0m".to_string()
        } else {
            "→ commands from the current project".to_string()
        });
    }
    if !legend.is_empty() {
        println!("{}", legend.join("   "));
    }
}

/// Print the pinned-command list as a table, or a quiet message when empty.
///
/// Columns: `ID`, `Command`, `Project`, `Pinned At`. The command column is
/// ellipsized like search results so long pins cannot blow up the layout.
pub fn print_pins_table(pins: &[PinnedCommand]) {
    if pins.is_empty() {
        println!("No pinned commands.");
        return;
    }

    let mut table = Table::new();
    table
        .load_preset(UTF8_FULL)
        .apply_modifier(UTF8_ROUND_CORNERS)
        .set_header(["ID", "Command", "Project", "Pinned At"]);

    for pin in pins {
        table.add_row([
            Cell::new(pin.command_id),
            Cell::new(ellipsize(&pin.cmd_string, MAX_DISPLAY_CHARS)),
            Cell::new(pin.project_path.as_deref().unwrap_or("–")),
            Cell::new(pin.pinned_at.format("%Y-%m-%d %H:%M").to_string()),
        ]);
    }

    println!("{table}");
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
            is_pinned: false,
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
    fn ellipsize_truncates_only_when_needed() {
        assert_eq!(ellipsize("short", 10), "short");
        assert_eq!(ellipsize("12345", 5), "12345");
        assert_eq!(ellipsize("1234567890", 5), "1234…");
        assert_eq!(ellipsize("1234567", 3), "12…");
        // Character-aware: never splits a multibyte char.
        assert_eq!(ellipsize("héllo wörld", 6), "héllo…");
    }

    #[test]
    fn pathological_long_command_does_not_overflow_table() {
        let mut cmd = String::with_capacity(5000);
        for _ in 0..5000 {
            cmd.push('x');
        }
        let truncated = ellipsize(&cmd, MAX_DISPLAY_CHARS);
        assert_eq!(truncated.chars().count(), MAX_DISPLAY_CHARS);
        assert!(truncated.ends_with('…'));

        let rows = vec![RankedCommand {
            command_id: 1,
            cmd_string: cmd,
            project_id: None,
            final_score: 1.0,
            success_count: 1,
            fail_count: 0,
            last_executed_at: Some("2026-09-08T10:11:12Z".to_string()),
            is_pinned: false,
        }];
        // Rendering must not panic or emit the full command.
        print_results_table(&rows, None);
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

    #[test]
    fn table_flags_pinned_commands() {
        let mut pinned = sample(Some(3), 4.0);
        pinned.is_pinned = true;
        let rows = vec![pinned, sample(Some(9), 2.0)];
        print_results_table(&rows, Some(3));
    }

    #[test]
    fn empty_pins_prints_notice() {
        print_pins_table(&[]);
    }

    #[test]
    fn pins_table_renders_all_columns() {
        let pinned_at = chrono::DateTime::parse_from_rfc3339("2026-09-09T10:11:12Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let pins = vec![
            PinnedCommand {
                command_id: 7,
                cmd_string: "cargo deploy --prod".to_string(),
                project_path: Some("/repo/app".to_string()),
                pinned_at,
            },
            PinnedCommand {
                command_id: 3,
                cmd_string: "echo standalone".to_string(),
                project_path: None,
                pinned_at,
            },
        ];
        print_pins_table(&pins);
    }
}
