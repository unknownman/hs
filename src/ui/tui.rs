//! Interactive terminal UI for `hs`.
//!
//! A `ratatui`-based, dependency-free-of-ratatui-on-the-hot-path browser:
//! a scrollable ranked list on top, and a preview panel below showing
//! the full command, its stats, and its risk classification.
//!
//! Keys: `↑`/`↓` navigate · `PageUp`/`PageDown` jump ±10 rows ·
//! `Home`/`End` jump to first/last · `Enter` run · `Esc`/`Ctrl-C` abort.
//!
//! `ratatui::init()`/`restore()` manage raw mode + the alternate screen
//! and (via the installed panic hook) guarantee the terminal is restored
//! even if rendering panics.

use std::io;

use ratatui::layout::{Alignment, Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::{Frame, Terminal};

use crate::context::{RiskLevel, analyze_risk};
use crate::error::HsError;
use crate::ranking::RankedCommand;
use crate::ui::format::{MAX_DISPLAY_CHARS, ellipsize};

/// Section headers shown between logically grouped command rows.
const HEADER_CURRENT: &str = "── Current project ─────────────────────────";
const HEADER_GLOBAL: &str = "── Global / other ──────────────────────────";

/// One visual row in the main list.
#[derive(Debug, PartialEq)]
enum Row {
    /// Section header (not selectable).
    Header(&'static str),
    /// A command result (index into the ranked slice).
    Command(usize),
}

/// Run the interactive browser and return the chosen command string.
///
/// * `Ok(Some(cmd))` — user pressed `Enter` on a row.
/// * `Ok(None)` — user aborted with `Esc`/`Ctrl-C`.
pub fn run(
    results: &[RankedCommand],
    current_project_id: Option<i64>,
) -> Result<Option<String>, HsError> {
    if results.is_empty() {
        return Ok(None);
    }

    let terminal = ratatui::init();
    let outcome = event_loop(terminal, results, current_project_id);
    ratatui::restore();
    outcome
}

fn event_loop(
    mut terminal: Terminal<ratatui::backend::CrosstermBackend<io::Stdout>>,
    results: &[RankedCommand],
    current_project_id: Option<i64>,
) -> Result<Option<String>, HsError> {
    use crossterm::event::{Event, KeyCode, KeyModifiers, read};

    let rows = build_rows(results, current_project_id);
    let selection_slots = rows
        .iter()
        .map(|r| match r {
            Row::Command(idx) => Some(*idx),
            Row::Header(_) => None,
        })
        .collect::<Vec<_>>();

    let mut state = ListState::default();
    state.select(Some(next_row(&selection_slots, None, 1)));

    // No ticker: `hs` has no background clock or progress bar, so the loop
    // blocks on `read()` instead of poll-spinning. The draw lands at the
    // top so every returned/ignored event (keys, paste) and every
    // `Event::Resize` naturally triggers a repaint with 0% idle CPU.
    loop {
        terminal.draw(|frame| {
            render(frame, results, current_project_id, &rows, &mut state);
        })?;

        // The only event worth acting on is a key. Any other event —
        // Resize, Paste, Focus — needs no handling of its own: the loop
        // spins back to the top and repaints with the new geometry.
        if let Event::Key(key) = read()? {
            match key.code {
                KeyCode::Down => {
                    state.select(Some(next_row(&selection_slots, state.selected(), 1)))
                }
                KeyCode::Up => state.select(Some(next_row(&selection_slots, state.selected(), -1))),
                KeyCode::PageDown => {
                    state.select(Some(next_row(&selection_slots, state.selected(), 10)))
                }
                KeyCode::PageUp => {
                    state.select(Some(next_row(&selection_slots, state.selected(), -10)))
                }
                KeyCode::Home => state.select(Some(next_row(&selection_slots, None, 1))),
                KeyCode::End => state.select(Some(next_row(&selection_slots, Some(0), -1))),
                KeyCode::Enter => {
                    if let Some(idx) = state.selected().and_then(|row| selection_slots[row]) {
                        return Ok(Some(results[idx].cmd_string.clone()));
                    }
                }
                KeyCode::Esc => return Ok(None),
                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    return Ok(None);
                }
                _ => {}
            }
        }
    }
}

/// Wrap-around, header-skipping row navigation.
///
/// The initial jump uses `delta` (so `PageDown` = +10, `PageUp` = -10,
/// wrap-around from `End` etc.), but once the landing is on a
/// `Row::Header` slot the collision-resolution loop advances one position
/// at a time (`delta.signum()`) instead of re-applying the full jump —
/// otherwise a page jump that lands on a header would leap again by 10,
/// skipping visible commands or even moving backwards.
fn next_row(slots: &[Option<usize>], current: Option<usize>, delta: i32) -> usize {
    let n = slots.len();
    if n == 0 || !slots.iter().any(Option::is_some) {
        return 0;
    }
    let mut next = (current.unwrap_or(0) as i64 + delta as i64).rem_euclid(n as i64) as usize;
    let step = delta.signum() as i64;
    while slots[next].is_none() {
        next = (next as i64 + step).rem_euclid(n as i64) as usize;
    }
    next
}

/// Build the visual rows: "Current project" commands first (bold/green),
/// then "Global / other", each group introduced by a header.
fn build_rows(results: &[RankedCommand], current_project_id: Option<i64>) -> Vec<Row> {
    let mut current: Vec<usize> = Vec::new();
    let mut other: Vec<usize> = Vec::new();
    for (i, r) in results.iter().enumerate() {
        if current_project_id.is_some() && r.project_id == current_project_id {
            current.push(i);
        } else {
            other.push(i);
        }
    }

    let mut rows: Vec<Row> = Vec::with_capacity(results.len() + 2);
    let mut push_group = |label: &'static str, indices: &[usize]| {
        if indices.is_empty() {
            return;
        }
        rows.push(Row::Header(label));
        rows.extend(indices.iter().map(|i| Row::Command(*i)));
    };
    push_group(HEADER_CURRENT, &current);
    push_group(HEADER_GLOBAL, &other);

    if rows.is_empty() {
        rows.push(Row::Command(0));
    }
    rows
}

fn render(
    frame: &mut Frame,
    results: &[RankedCommand],
    current_project_id: Option<i64>,
    rows: &[Row],
    state: &mut ListState,
) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(62), Constraint::Percentage(38)])
        .split(frame.area());

    // ── Main: the ranked list ────────────────────────────────────────
    let list_items: Vec<ListItem> = rows
        .iter()
        .map(|row| match row {
            Row::Header(label) => ListItem::new(*label).style(
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Row::Command(idx) => {
                let r = &results[*idx];
                let in_project = current_project_id.is_some() && r.project_id == current_project_id;
                let style = if r.is_pinned {
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD)
                } else if in_project {
                    Style::default()
                        .fg(Color::Green)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::White)
                };
                // One-line list cell: truncate pathological commands to a
                // safe display width. The preview panel below shows the
                // full text (wrapped). Pinned commands are prefixed with
                // a pin glyph so they stand out from the crowd.
                let label = if r.is_pinned {
                    format!("📌 {}", ellipsize(&r.cmd_string, MAX_DISPLAY_CHARS))
                } else {
                    ellipsize(&r.cmd_string, MAX_DISPLAY_CHARS)
                };
                ListItem::new(label).style(style)
            }
        })
        .collect();

    let list = List::new(list_items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" hs — What worked here before? "),
        )
        .highlight_style(Style::default().bg(Color::Blue).fg(Color::Black))
        .highlight_symbol("▶ ");
    frame.render_stateful_widget(list, chunks[0], state);

    // ── Preview: details for the highlighted command ─────────────────
    let selected = state
        .selected()
        .and_then(|row| match rows[row] {
            Row::Command(idx) => Some(idx),
            Row::Header(_) => None,
        })
        .and_then(|idx| results.get(idx));

    let preview = match selected {
        Some(r) => {
            let risk = analyze_risk(&r.cmd_string);
            let total = r.success_count + r.fail_count;
            let rate = if total == 0 {
                "–".to_string()
            } else {
                format!("{:.0}%", 100.0 * r.success_count as f64 / total as f64)
            };
            let risk_color = match risk {
                RiskLevel::Safe => Color::Green,
                RiskLevel::High => Color::Red,
            };
            let risk_label = match risk {
                RiskLevel::Safe => "SAFE",
                RiskLevel::High => "HIGH RISK",
            };

            let lines = vec![
                ratatui::text::Line::from(format!(
                    "{}ID: {}  ·  Score {:.2}  ·  {} run(s)  ·  success rate {}  ·  last: {}",
                    if r.is_pinned { " 📌 PINNED  " } else { "" },
                    r.command_id,
                    r.final_score,
                    total,
                    rate,
                    r.last_executed_at.as_deref().unwrap_or("never"),
                )),
                ratatui::text::Line::from(""),
                ratatui::text::Line::from(r.cmd_string.as_str()),
                ratatui::text::Line::from(""),
                ratatui::text::Line::from(ratatui::text::Span::styled(
                    format!("  {risk_label}  "),
                    Style::default().fg(Color::Black).bg(risk_color),
                )),
            ];
            Paragraph::new(lines)
                .block(Block::default().borders(Borders::ALL).title(" Preview "))
                .wrap(Wrap { trim: true })
        }
        None => Paragraph::new("Select a command with ↑/↓, then press Enter to run it.")
            .block(Block::default().borders(Borders::ALL).title(" Preview ")),
    };
    frame.render_widget(preview, chunks[1]);

    // ── Footer hints ─────────────────────────────────────────────────
    let hints =
        Paragraph::new("↑/↓ move   PgUp/PgDn ±10   Home/End jump   Enter run   Esc / Ctrl-C exit")
            .alignment(Alignment::Center)
            .style(Style::default().fg(Color::DarkGray));
    frame.render_widget(hints, chunks[1]);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(cmd: &str, project_id: Option<i64>) -> RankedCommand {
        RankedCommand {
            command_id: 1,
            cmd_string: cmd.to_string(),
            project_id,
            final_score: 1.0,
            success_count: 3,
            fail_count: 0,
            last_executed_at: Some("2026-09-08T10:11:12Z".to_string()),
            is_pinned: false,
        }
    }

    #[test]
    fn next_row_skips_headers_and_wraps() {
        // rows: header, cmd0, header, cmd1, cmd2
        let slots = vec![None, Some(0), None, Some(1), Some(2)];
        assert_eq!(next_row(&slots, Some(1), 1), 3); // skip header
        assert_eq!(next_row(&slots, Some(4), 1), 1); // wrap to cmd0
        assert_eq!(next_row(&slots, Some(1), -1), 4); // wrap backwards
        assert_eq!(next_row(&slots, Some(3), -1), 1);
    }

    #[test]
    fn next_row_handles_empty_and_all_headers() {
        assert_eq!(next_row(&[], Some(0), 1), 0);
        assert_eq!(next_row(&[None, None], None, 1), 0);
    }

    #[test]
    fn next_row_page_jump_resolves_headers_by_single_steps() {
        // Array of 20 slots. Index 11 is a header.
        let mut slots = vec![Some(0); 20];
        slots[11] = None;

        // Start at index 1. Jump by 10 (PageDown).
        // Initial jump lands on 11 (the header).
        // It should step by +1 to index 12, NOT jump by 10 again to 21%20=1.
        assert_eq!(next_row(&slots, Some(1), 10), 12);

        // Start at index 1. Jump by -10 (PageUp).
        // Initial jump lands on -9 % 20 = 11 (the header).
        // It should step by -1 to index 10, NOT jump by -10 again.
        assert_eq!(next_row(&slots, Some(1), -10), 10);
    }

    #[test]
    fn build_rows_groups_by_project() {
        let results = vec![
            sample("in proj", Some(3)),
            sample("outside", None),
            sample("other proj", Some(9)),
        ];
        let rows = build_rows(&results, Some(3));

        // header(current) + cmd0 + header(global) + cmd1 + cmd2
        let slots: Vec<Option<usize>> = rows
            .iter()
            .map(|r| match r {
                Row::Command(i) => Some(*i),
                Row::Header(_) => None,
            })
            .collect();
        assert_eq!(slots, vec![None, Some(0), None, Some(1), Some(2)]);

        // Current-project command must be mapped first.
        assert_eq!(
            rows[1],
            Row::Command(0),
            "current project command should lead the list"
        );
    }

    #[test]
    fn empty_results_returns_none_without_panicking() {
        assert_eq!(run(&[], None).unwrap(), None);
    }

    #[test]
    fn renders_without_panicking() {
        use ratatui::backend::TestBackend;

        let mut selected = sample("cargo build --release", Some(3));
        selected.command_id = 7;
        let results = vec![selected, sample("docker build -t app .", None)];
        let rows = build_rows(&results, Some(3));
        let mut state = ListState::default();
        state.select(Some(1)); // first command row

        let mut terminal = ratatui::Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal
            .draw(|frame| render(frame, &results, Some(3), &rows, &mut state))
            .unwrap();

        // Both groups + both commands must be drawn somewhere.
        let text: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(text.contains("cargo build --release"), "list must render");
        assert!(text.contains("docker build -t app ."), "list must render");
        assert!(
            text.contains("Current project"),
            "section header must render"
        );
        assert!(text.contains("SAFE"), "preview risk badge must render");
        assert!(
            text.contains("ID: 7"),
            "preview must expose the command id, got: {text:?}"
        );
    }

    #[test]
    fn renders_pinned_marker_in_list_and_preview() {
        use ratatui::backend::TestBackend;

        let mut pinned = sample("cargo deploy --prod", Some(3));
        pinned.is_pinned = true;
        let results = vec![pinned, sample("echo hello", None)];
        let rows = build_rows(&results, Some(3));
        let mut state = ListState::default();
        state.select(Some(1)); // first command row

        let mut terminal = ratatui::Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal
            .draw(|frame| render(frame, &results, Some(3), &rows, &mut state))
            .unwrap();

        let text: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        // Both the list cell and the preview header flag the pin.
        assert!(
            text.contains("PINNED"),
            "preview must show the PINNED badge, got: {text:?}"
        );
        assert!(
            text.contains("ID: 1"),
            "preview must expose the pinned command's id, got: {text:?}"
        );
    }

    /// Phase 8 hardening: a 5,000-character multiline command must render
    /// without panicking — truncated to the ellipsis in the list cell
    /// while still drawing safely.
    #[test]
    fn renders_5000_char_multiline_command_without_overflow() {
        use ratatui::backend::TestBackend;

        let mut cmd = String::new();
        for i in 0..5000 {
            cmd.push(if i % 100 == 0 { '\n' } else { 'a' });
        }
        let results = vec![sample(&cmd, Some(3))];
        let rows = build_rows(&results, Some(3));
        let mut state = ListState::default();
        state.select(Some(1)); // select the (only) command row

        let mut terminal = ratatui::Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal
            .draw(|frame| render(frame, &results, Some(3), &rows, &mut state))
            .unwrap();

        let buffer = terminal.backend().buffer().content();
        // The list cell is ellipsized: the buffer must NOT echo 5000 chars
        // anywhere. First list row is the Preview panel's wrapped command —
        // even wrapped, it can only span the 24-row screen.
        assert!(
            buffer.len() < 5000,
            "5,000-char command must never be echoed verbatim into the buffer"
        );
        let text: String = buffer.iter().map(|cell| cell.symbol()).collect();
        assert!(text.contains('…'), "list cell must show the ellipsis");
        assert!(
            text.contains("Score"),
            "preview must show the stat line, got: {text:?}"
        );
        assert!(
            text.contains("ID: 1"),
            "preview must expose the command id even for huge commands, got: {text:?}"
        );
    }
}
