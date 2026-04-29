//! Reports view — project statistics and recent git commits.
//!
//! This module provides:
//! - `render_reports()` — two-pane layout: stats (left) + commits (right)
//! - `load_reports_data()` — asynchronously loads git log + computes task stats
//! - `scroll_reports()` — scroll the commit list up/down

use crate::config::types::ThemeConfig;
use crate::state::types::{AppState, GitCommit, ReportsState, TaskStats};
use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

/// Maximum number of commits to display.
const MAX_COMMITS: usize = 30;

// ─── Public API ───────────────────────────────────────────────────────────

/// Render the reports view as a full-screen two-pane overlay.
///
/// Left pane: task statistics (total, completed, failed, running, avg/max/min time, completion rate).
/// Right pane: scrollable list of the last 30 git commits.
pub fn render_reports(f: &mut Frame, area: Rect, state: &mut AppState, theme: &ThemeConfig) {
    let reports = match &state.ui.reports {
        Some(r) => r,
        None => return,
    };

    // Clear the entire area
    f.render_widget(Clear, area);

    if area.height < 6 || area.width < 40 {
        return;
    }

    // Error state
    if let Some(ref error) = reports.error {
        let error_para = Paragraph::new(Span::styled(error.as_str(), Style::default().fg(Color::Red)))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::Red))
                    .title(Span::styled(
                        " Reports ",
                        Style::default()
                            .fg(Color::Rgb(140, 144, 170))
                            .add_modifier(Modifier::BOLD),
                    ))
                    .title_alignment(Alignment::Center),
            );
        f.render_widget(error_para, area);
        return;
    }

    // Layout: title bar (1) | content area | footer (1)
    let v_constraints = [
        Constraint::Length(1), // Title bar
        Constraint::Min(0),    // Content
        Constraint::Length(1), // Footer
    ];
    let v_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints(v_constraints)
        .split(area);

    // Title bar
    let title = Paragraph::new(Span::styled(
        " Reports ",
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    ))
    .alignment(Alignment::Left);
    f.render_widget(title, v_layout[0]);

    // Content area: vertical split into stats (left) and commits (right)
    let h_constraints = [
        Constraint::Percentage(35), // Stats pane (left)
        Constraint::Min(0),         // Commits pane (right)
    ];
    let h_layout = Layout::default()
        .direction(Direction::Horizontal)
        .constraints(h_constraints)
        .split(v_layout[1]);

    // Left pane: Task Statistics
    render_stats_pane(f, h_layout[0], &reports.stats, theme);

    // Right pane: Git Commits
    render_commits_pane(f, h_layout[1], reports);

    // Footer with keybinding hints
    let footer_text = Span::styled(
        " j/k: scroll  Esc/q: close ",
        Style::default().fg(Color::Rgb(140, 144, 170)),
    );
    let footer = Paragraph::new(footer_text).alignment(Alignment::Center);
    f.render_widget(footer, v_layout[2]);
}

/// Load reports data asynchronously: run `git log` and compute task stats,
/// then store the result in `state.ui.reports`.
///
/// Call this when entering Reports mode. The git command runs on a blocking
/// thread to avoid freezing the UI.
pub fn load_reports_data(state: &std::sync::Arc<std::sync::Mutex<AppState>>) {
    let state_clone = state.clone();

    // Extract working directory and task data while holding the lock
    let working_dir = {
        let mut state = state.lock().unwrap_or_else(|e| e.into_inner());

        // Initialize the reports state
        state.ui.reports = Some(ReportsState {
            commits: Vec::new(),
            stats: TaskStats::default(),
            selected_index: 0,
            scroll_offset: 0,
            error: None,
        });

        let working_dir = state
            .project_registry
            .active_project_id
            .as_ref()
            .and_then(|pid| {
                state
                    .project_registry
                    .projects
                    .iter()
                    .find(|p| &p.id == pid)
                    .map(|p| p.working_directory.clone())
                    .filter(|wd| !wd.is_empty())
            });

        // Compute stats from in-memory tasks
        let total = state.tasks.len();
        let completed = state
            .tasks
            .values()
            .filter(|t| t.column.0 == "done" || t.column.0 == "review")
            .count();
        let failed = state
            .tasks
            .values()
            .filter(|t| {
                matches!(
                    t.agent_status,
                    crate::state::types::AgentStatus::Error
                )
            })
            .count();
        let running = state
            .tasks
            .values()
            .filter(|t| {
                matches!(
                    t.agent_status,
                    crate::state::types::AgentStatus::Running
                        | crate::state::types::AgentStatus::Pending
                )
            })
            .count();

        let completion_rate = if total > 0 {
            (completed as f64 / total as f64) * 100.0
        } else {
            0.0
        };

        // Calculate task durations for tasks in the "done" column
        let durations: Vec<f64> = state
            .tasks
            .values()
            .filter(|t| t.column.0 == "done")
            .filter_map(|t| {
                let start = t.created_at;
                let end = t.entered_column_at;
                if start > 0 && end > 0 && end > start {
                    Some((end - start) as f64)
                } else {
                    None
                }
            })
            .collect();

        let avg_time = if !durations.is_empty() {
            durations.iter().sum::<f64>() / durations.len() as f64
        } else {
            0.0
        };
        let max_time = durations.iter().copied().fold(0.0_f64, f64::max);
        let min_time = durations.iter().copied().fold(f64::MAX, f64::min);
        let min_time = if durations.is_empty() { 0.0 } else { min_time };

        let stats = TaskStats {
            total_tasks: total,
            completed_tasks: completed,
            failed_tasks: failed,
            running_tasks: running,
            avg_time_secs: avg_time,
            max_time_secs: max_time,
            min_time_secs: min_time,
            completion_rate,
        };

        // Store stats immediately
        if let Some(ref mut reports) = state.ui.reports {
            reports.stats = stats;
        }

        state.mark_render_dirty();

        working_dir
    };

    // Spawn git log on a blocking thread
    tokio::task::spawn_blocking(move || {
        let commits_result = match working_dir {
            Some(wd) => run_git_log(&wd),
            None => Err("No working directory configured for this project".to_string()),
        };

        let mut state = state_clone.lock().unwrap_or_else(|e| e.into_inner());

        if let Some(ref mut reports) = state.ui.reports {
            match commits_result {
                Ok(commits) => {
                    reports.commits = commits;
                    reports.error = None;
                }
                Err(e) => {
                    reports.error = Some(e);
                }
            }
        }

        state.mark_render_dirty();
    });
}

/// Scroll the commit list by `direction` lines (-1 = up, +1 = down).
pub fn scroll_reports(state: &mut ReportsState, direction: i32) {
    let total = state.commits.len();
    if total == 0 {
        return;
    }

    let new_index = (state.selected_index as i32 + direction).clamp(0, (total - 1) as i32) as usize;
    state.selected_index = new_index;
}

// ─── Git Log ──────────────────────────────────────────────────────────────

/// Run `git log --pretty=format:... -30` and parse the output.
fn run_git_log(working_dir: &str) -> Result<Vec<GitCommit>, String> {
    // Use explicit format specifiers for robust parsing across git versions.
    // %h = abbreviated hash, %s = subject, %an = author name,
    // %ar = relative date, %at = author timestamp (unix).
    let output = std::process::Command::new("git")
        .args([
            "log",
            &format!("-{}", MAX_COMMITS),
            "--pretty=format:%h%x09%s%x09%an%x09%ar%x09%at",
        ])
        .current_dir(working_dir)
        .output()
        .map_err(|e| format!("Failed to run git log: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("git log failed: {}", stderr.trim()));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut commits = Vec::new();

    for line in stdout.lines() {
        let parts: Vec<&str> = line.splitn(5, '\t').collect();
        if parts.len() >= 5 {
            let timestamp = parts[4].parse::<i64>().unwrap_or(0);
            commits.push(GitCommit {
                hash: parts[0].to_string(),
                message: parts[1].to_string(),
                author: parts[2].to_string(),
                date: parts[3].to_string(),
                timestamp,
            });
        }
    }

    Ok(commits)
}

// ─── Rendering Helpers ────────────────────────────────────────────────────

/// Render the left pane with task statistics.
fn render_stats_pane(f: &mut Frame, area: Rect, stats: &TaskStats, _theme: &ThemeConfig) {
    let mut lines: Vec<Line> = Vec::new();

    lines.push(Line::from(Span::styled(
        " TASK STATISTICS",
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    )));
    lines.push(Line::from(""));

    let cyan = Style::default().fg(Color::Cyan);
    let white = Style::default().fg(Color::White);
    let green = Style::default().fg(Color::Green);
    let red = Style::default().fg(Color::Red);
    let yellow = Style::default().fg(Color::Yellow);

    lines.push(Line::from(vec![
        Span::styled("  Total Tasks:  ", cyan),
        Span::styled(stats.total_tasks.to_string(), white),
    ]));
    lines.push(Line::from(vec![
        Span::styled("  Completed:    ", cyan),
        Span::styled(stats.completed_tasks.to_string(), green),
    ]));
    lines.push(Line::from(vec![
        Span::styled("  Failed:       ", cyan),
        Span::styled(stats.failed_tasks.to_string(), red),
    ]));
    lines.push(Line::from(vec![
        Span::styled("  Running:      ", cyan),
        Span::styled(stats.running_tasks.to_string(), yellow),
    ]));

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        " DURATION (done tasks)",
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    )));
    lines.push(Line::from(""));

    lines.push(Line::from(vec![
        Span::styled("  Avg time:     ", cyan),
        Span::styled(format_duration(stats.avg_time_secs), white),
    ]));
    lines.push(Line::from(vec![
        Span::styled("  Max time:     ", cyan),
        Span::styled(format_duration(stats.max_time_secs), white),
    ]));
    lines.push(Line::from(vec![
        Span::styled("  Min time:     ", cyan),
        Span::styled(format_duration(stats.min_time_secs), white),
    ]));

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        " COMPLETION",
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    )));
    lines.push(Line::from(""));
    lines.push(Line::from(vec![
        Span::styled("  Done rate:    ", cyan),
        Span::styled(
            format!("{:.1}%", stats.completion_rate),
            if stats.completion_rate >= 70.0 {
                green
            } else if stats.completion_rate >= 40.0 {
                yellow
            } else {
                red
            },
        ),
    ]));

    let stats_para = Paragraph::new(lines).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Rgb(60, 64, 80)))
            .title(Span::styled(
                " Stats ",
                Style::default().fg(Color::Rgb(140, 144, 170)),
            )),
    );

    f.render_widget(stats_para, area);
}

/// Render the right pane with the scrollable commit list.
fn render_commits_pane(f: &mut Frame, area: Rect, reports: &ReportsState) {
    let inner = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Rgb(60, 64, 80)))
        .title(Span::styled(
            format!(" Recent Commits ({}) ", reports.commits.len()),
            Style::default().fg(Color::Rgb(140, 144, 170)),
        ))
        .inner(area);

    if reports.commits.is_empty() {
        let empty = Paragraph::new(Span::styled(
            "No commits found",
            Style::default().fg(Color::Rgb(140, 144, 170)),
        ))
        .alignment(Alignment::Center);
        f.render_widget(empty, inner);
        // Re-render the block
        f.render_widget(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Rgb(60, 64, 80)))
                .title(Span::styled(
                    " Recent Commits (0) ",
                    Style::default().fg(Color::Rgb(140, 144, 170)),
                )),
            area,
        );
        return;
    }

    let visible_height = inner.height as usize;
    if visible_height == 0 {
        return;
    }

    // Adjust scroll offset so selected item is visible
    let selected = reports.selected_index;
    let mut scroll = reports.scroll_offset;
    if selected < scroll {
        scroll = selected;
    } else if selected >= scroll + visible_height {
        scroll = selected - visible_height + 1;
    }

    let visible_commits: Vec<&GitCommit> = reports
        .commits
        .iter()
        .skip(scroll)
        .take(visible_height)
        .collect();

    let mut lines: Vec<Line> = Vec::new();

    for (i, commit) in visible_commits.iter().enumerate() {
        let actual_index = scroll + i;
        let is_selected = actual_index == selected;

        if is_selected {
            lines.push(Line::from(vec![
                Span::styled(" ▶ ", Style::default().fg(Color::Cyan)),
                Span::styled(
                    format!("{} ", commit.hash),
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    truncate_str(&commit.message, 40),
                    Style::default().fg(Color::White),
                ),
            ]));
        } else {
            lines.push(Line::from(vec![
                Span::styled("   ", Style::default()),
                Span::styled(
                    format!("{} ", commit.hash),
                    Style::default().fg(Color::Rgb(140, 144, 170)),
                ),
                Span::styled(
                    truncate_str(&commit.message, 40),
                    Style::default().fg(Color::Rgb(180, 184, 200)),
                ),
            ]));
        }

        // Second line: author + date
        let meta_color = if is_selected {
            Color::Rgb(180, 184, 200)
        } else {
            Color::Rgb(100, 104, 120)
        };
        lines.push(Line::from(vec![
            Span::styled("     ", Style::default()),
            Span::styled(
                truncate_str(&commit.author, 20),
                Style::default().fg(meta_color),
            ),
            Span::styled(
                format!("  {}", commit.date),
                Style::default().fg(meta_color),
            ),
        ]));
    }

    let commits_para = Paragraph::new(lines);
    f.render_widget(commits_para, inner);

    // Re-render the block on top
    f.render_widget(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Rgb(60, 64, 80)))
            .title(Span::styled(
                format!(" Recent Commits ({}) ", reports.commits.len()),
                Style::default().fg(Color::Rgb(140, 144, 170)),
            )),
        area,
    );

    // Scroll indicator
    if reports.commits.len() > visible_height {
        let scroll_ratio = scroll as f64 / (reports.commits.len() - visible_height) as f64;
        let indicator_height = std::cmp::max(1, (visible_height as f64 * (visible_height as f64 / reports.commits.len() as f64)) as u16);
        let max_scroll_pos = inner.height.saturating_sub(indicator_height);
        let indicator_y = inner.y + (scroll_ratio * max_scroll_pos as f64) as u16;

        let indicator = Paragraph::new("█").style(Style::default().fg(Color::Rgb(80, 84, 100)));
        f.render_widget(indicator, Rect::new(inner.x + inner.width.saturating_sub(1), indicator_y, 1, indicator_height));
    }
}

// ─── Utility Functions ────────────────────────────────────────────────────

/// Format a duration in seconds to a human-readable string.
fn format_duration(secs: f64) -> String {
    if secs <= 0.0 {
        return "—".to_string();
    }
    let total_secs = secs as u64;
    let hours = total_secs / 3600;
    let minutes = (total_secs % 3600) / 60;
    let seconds = total_secs % 60;

    if hours > 0 {
        format!("{}h {}m", hours, minutes)
    } else if minutes > 0 {
        format!("{}m {}s", minutes, seconds)
    } else {
        format!("{}s", seconds)
    }
}

/// Truncate a string to `max_len` characters, appending "..." if truncated.
fn truncate_str(s: &str, max_len: usize) -> String {
    if s.chars().count() <= max_len {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max_len - 3).collect();
        format!("{}...", truncated)
    }
}
