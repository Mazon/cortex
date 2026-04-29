//! Diff review view — displays git diff output for completed "do" tasks.
//!
//! This module provides:
//! - `parse_git_diff()` — parses raw `git diff` output into structured `DiffFile` entries
//! - `render_diff_review()` — renders a full-screen diff viewer with file navigation

use crate::config::types::ThemeConfig;
use crate::state::types::{AppState, DiffFile, DiffLine, DiffLineKind, DiffReviewState};
use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

/// Maximum diff output size (1 MB). Output exceeding this is truncated.
const MAX_DIFF_SIZE: usize = 1_048_576;

/// Render the diff review view in the given area.
///
/// Shows one file at a time with navigation controls. The header displays
/// the task number, file index, and file path. The body shows the diff
/// with color-coded additions (green), removals (red), context (dim white),
/// and hunk headers (blue). The footer shows keybinding hints.
pub fn render_diff_review(f: &mut Frame, area: Rect, state: &mut AppState, theme: &ThemeConfig) {
    let review = match &state.ui.diff_review {
        Some(r) => r,
        None => return,
    };

    if area.height < 4 || area.width < 20 {
        return;
    }

    // Error state
    if let Some(ref error) = review.error {
        let error_para = Paragraph::new(Span::styled(
            error.as_str(),
            Style::default().fg(Color::Red),
        ))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Red))
                .title(Span::styled(
                    format!(" Review Changes — Task #{} ", review.task_number),
                    Style::default()
                        .fg(Color::Rgb(140, 144, 170))
                        .add_modifier(Modifier::BOLD),
                ))
                .title_alignment(Alignment::Center),
        );
        f.render_widget(error_para, area);
        return;
    }

    // Empty state — no files changed
    if review.files.is_empty() {
        let empty_para = Paragraph::new(Span::styled(
            "No changes to review",
            Style::default().fg(Color::Rgb(140, 144, 170)),
        ))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Rgb(60, 64, 80)))
                .title(Span::styled(
                    format!(" Review Changes — Task #{} ", review.task_number),
                    Style::default()
                        .fg(Color::Rgb(140, 144, 170))
                        .add_modifier(Modifier::BOLD),
                ))
                .title_alignment(Alignment::Center),
        );
        f.render_widget(empty_para, area);
        return;
    }

    // Layout: title bar (1 row) | diff content | footer (1 row)
    let v_constraints = [
        Constraint::Length(1), // Title bar
        Constraint::Min(0),    // Diff content
        Constraint::Length(1), // Footer
    ];
    let v_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints(v_constraints)
        .split(area);

    // ── Title bar ─────────────────────────────────────────────────────────
    let file_idx = review
        .selected_file_index
        .min(review.files.len().saturating_sub(1));
    let file = &review.files[file_idx];
    let total_files = review.files.len();

    let mut title_spans: Vec<Span<'_>> = vec![
        Span::styled(
            format!(" Review Changes — Task #{} ", review.task_number),
            Style::default()
                .fg(Color::Rgb(140, 144, 170))
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(" │ ", Style::default().fg(Color::Rgb(45, 48, 62))),
    ];

    // File badges (new, deleted, binary, renamed)
    if file.is_new {
        title_spans.push(Span::styled(
            "new ",
            Style::default().fg(theme.done_color()),
        ));
    }
    if file.is_deleted {
        title_spans.push(Span::styled(
            "deleted ",
            Style::default().fg(theme.error_color()),
        ));
    }
    if file.is_binary {
        title_spans.push(Span::styled("binary ", Style::default().fg(Color::Yellow)));
    }
    if file.is_renamed {
        title_spans.push(Span::styled(
            "renamed ",
            Style::default().fg(Color::Rgb(140, 144, 170)),
        ));
    }

    // File path
    let display_path = if file.is_renamed {
        if let Some(ref old) = file.old_path {
            format!("{} → {}", old, file.path)
        } else {
            file.path.clone()
        }
    } else {
        file.path.clone()
    };

    title_spans.push(Span::styled(
        display_path,
        Style::default().fg(Color::White),
    ));

    // Additions/deletions counts
    if file.additions > 0 || file.deletions > 0 {
        title_spans.push(Span::styled(
            format!("  +{} ", file.additions),
            Style::default().fg(theme.done_color()),
        ));
        title_spans.push(Span::styled(
            format!("-{} ", file.deletions),
            Style::default().fg(theme.error_color()),
        ));
    }

    // File index indicator
    title_spans.push(Span::styled(
        " ",
        Style::default().fg(Color::Rgb(45, 48, 62)),
    ));
    title_spans.push(Span::styled(
        format!("File {}/{}", file_idx + 1, total_files),
        Style::default().fg(Color::Rgb(100, 104, 120)),
    ));

    let title_para =
        Paragraph::new(Line::from(title_spans)).style(Style::default().bg(Color::Rgb(30, 34, 50)));
    f.render_widget(title_para, v_layout[0]);

    // ── Diff content ──────────────────────────────────────────────────────
    let content_area = v_layout[1];
    if file.is_binary {
        let binary_msg = Paragraph::new(Span::styled(
            "Binary file — cannot display diff",
            Style::default().fg(Color::Rgb(140, 144, 170)),
        ))
        .alignment(Alignment::Center);
        f.render_widget(binary_msg, content_area);
    } else {
        render_diff_content(f, content_area, file, review.scroll_offset);
    }

    // ── Footer ────────────────────────────────────────────────────────────
    let key = Style::default()
        .fg(Color::Rgb(140, 144, 170))
        .add_modifier(Modifier::BOLD);
    let desc = Style::default().fg(Color::Rgb(90, 94, 110));
    let pipe = Style::default().fg(Color::Rgb(45, 48, 62));

    let footer_spans: Vec<Span<'_>> = vec![
        Span::styled("Tab", key),
        Span::styled("/", pipe),
        Span::styled("]", key),
        Span::styled(" next file  ", desc),
        Span::styled("Shift+Tab", key),
        Span::styled("/", pipe),
        Span::styled("[", key),
        Span::styled(" prev file  ", desc),
        Span::styled("↑↓", key),
        Span::styled("/", pipe),
        Span::styled("j/k", key),
        Span::styled(" scroll  ", desc),
        Span::styled("Esc", key),
        Span::styled(" back", desc),
    ];

    let footer_para =
        Paragraph::new(Line::from(footer_spans)).style(Style::default().bg(Color::Rgb(30, 34, 50)));
    f.render_widget(footer_para, v_layout[2]);
}

/// Render the diff lines for a single file within the given area.
fn render_diff_content(f: &mut Frame, area: Rect, file: &DiffFile, scroll_offset: usize) {
    let content_height = area.height as usize;
    if content_height == 0 {
        return;
    }

    let _total_lines = file.lines.len();

    // Build all lines first, then slice for visible area
    let visible_lines: Vec<Line<'_>> = file
        .lines
        .iter()
        .enumerate()
        .skip(scroll_offset)
        .take(content_height)
        .map(|(_i, line)| build_styled_line(line))
        .collect();

    let diff_para = Paragraph::new(visible_lines).wrap(Wrap { trim: false });
    f.render_widget(diff_para, area);
}

/// Build a styled `Line` from a `DiffLine`.
fn build_styled_line(line: &DiffLine) -> Line<'_> {
    match &line.kind {
        DiffLineKind::Context => {
            let prefix = format!(
                "{:>4} {:>4} ",
                line.old_line_no.unwrap_or(0),
                line.new_line_no.unwrap_or(0)
            );
            Line::from(vec![
                Span::styled(prefix, Style::default().fg(Color::Rgb(60, 64, 80))),
                Span::styled(" ", Style::default().fg(Color::Rgb(60, 64, 80))),
                Span::styled(
                    &line.content,
                    Style::default().fg(Color::Rgb(160, 164, 180)),
                ),
            ])
        }
        DiffLineKind::Addition => {
            let new_no = line.new_line_no.unwrap_or(0);
            let prefix = format!("     {:>4} ", new_no);
            Line::from(vec![
                Span::styled(prefix, Style::default().fg(Color::Rgb(40, 80, 40))),
                Span::styled("+", Style::default().fg(Color::Rgb(80, 180, 80))),
                Span::styled(
                    &line.content,
                    Style::default().fg(Color::Rgb(120, 220, 120)),
                ),
            ])
        }
        DiffLineKind::Removal => {
            let old_no = line.old_line_no.unwrap_or(0);
            let prefix = format!("{:>4}      ", old_no);
            Line::from(vec![
                Span::styled(prefix, Style::default().fg(Color::Rgb(80, 40, 40))),
                Span::styled("-", Style::default().fg(Color::Rgb(220, 80, 80))),
                Span::styled(
                    &line.content,
                    Style::default().fg(Color::Rgb(220, 120, 120)),
                ),
            ])
        }
        DiffLineKind::HunkHeader {
            old_start,
            old_count,
            new_start,
            new_count,
        } => {
            let header = format!(
                "@@ -{},{} +{},{} @@",
                old_start, old_count, new_start, new_count
            );
            Line::from(Span::styled(
                header,
                Style::default().fg(Color::Rgb(100, 149, 237)), // Cornflower blue
            ))
        }
        DiffLineKind::NoNewlineAtEndOfFile => Line::from(Span::styled(
            &line.content,
            Style::default().fg(Color::Rgb(120, 80, 160)),
        )),
    }
}

// ─── Git Diff Parser ──────────────────────────────────────────────────────

/// Parse raw `git diff` output into a list of `DiffFile` entries.
///
/// Handles: new files, deleted files, renames, binary files, normal diffs,
/// and "no newline at end of file" markers.
///
/// Output exceeding `MAX_DIFF_SIZE` bytes is truncated with a warning file.
pub fn parse_git_diff(output: &str) -> Vec<DiffFile> {
    let mut files: Vec<DiffFile> = Vec::new();
    let mut current_file: Option<DiffFile> = None;

    // Track line numbers within hunks
    let mut old_line_no: u32 = 0;
    let mut new_line_no: u32 = 0;

    // Truncate large output
    let output = if output.len() > MAX_DIFF_SIZE {
        let truncated = &output[..MAX_DIFF_SIZE];
        files.push(DiffFile {
            path: "[TRUNCATED]".to_string(),
            old_path: None,
            additions: 0,
            deletions: 0,
            is_new: false,
            is_deleted: false,
            is_binary: true,
            is_renamed: false,
            lines: vec![DiffLine {
                kind: DiffLineKind::Context,
                content: format!(
                    "Diff output exceeds {} bytes and was truncated. Some changes may not be shown.",
                    MAX_DIFF_SIZE
                ),
                old_line_no: None,
                new_line_no: None,
            }],
        });
        truncated
    } else {
        output
    };

    for raw_line in output.lines() {
        // New file diff header
        if let Some(path) = raw_line.strip_prefix("diff --git ") {
            // Finalize previous file
            if let Some(f) = current_file.take() {
                files.push(f);
            }
            // Extract the target path from "a/path b/path"
            let path = extract_path_from_git_header(path);
            current_file = Some(DiffFile {
                path,
                old_path: None,
                additions: 0,
                deletions: 0,
                is_new: false,
                is_deleted: false,
                is_binary: false,
                is_renamed: false,
                lines: Vec::new(),
            });
            continue;
        }

        // If we're not in a file diff, skip
        let file = match &mut current_file {
            Some(f) => f,
            None => continue,
        };

        // Old file mode (detect new files)
        if raw_line.starts_with("new file mode") {
            file.is_new = true;
            continue;
        }
        if raw_line.starts_with("deleted file mode") {
            file.is_deleted = true;
            continue;
        }

        // Binary files
        if raw_line == "Binary files /dev/null differ" {
            file.is_binary = true;
            file.is_new = true;
            continue;
        }
        if raw_line.starts_with("Binary files ") && raw_line.ends_with(" differ") {
            file.is_binary = true;
            continue;
        }

        // Rename
        if raw_line.starts_with("rename from ") {
            file.old_path = Some(raw_line["rename from ".len()..].to_string());
            file.is_renamed = true;
            continue;
        }
        if raw_line.starts_with("rename to ") {
            file.path = raw_line["rename to ".len()..].to_string();
            continue;
        }

        // "---" line (old file path)
        if raw_line.starts_with("--- ") {
            // If path is /dev/null, it's a new file
            if raw_line.contains("/dev/null") {
                file.is_new = true;
            } else if let Some(ref _old_path) = file.old_path {
                // Already have old_path from rename
            } else if !file.is_renamed {
                // Extract path from --- a/path
                let p = raw_line
                    .strip_prefix("--- a/")
                    .or_else(|| raw_line.strip_prefix("--- "))
                    .unwrap_or("");
                if !p.is_empty() && p != "/dev/null" {
                    file.old_path = Some(p.to_string());
                }
            }
            continue;
        }

        // "+++" line (new file path)
        if raw_line.starts_with("+++ ") {
            if raw_line.contains("/dev/null") {
                file.is_deleted = true;
            }
            continue;
        }

        // Hunk header
        if raw_line.starts_with("@@ ") {
            if let Some((old_start, old_count, new_start, new_count)) = parse_hunk_header(raw_line)
            {
                old_line_no = old_start;
                new_line_no = new_start;
                file.lines.push(DiffLine {
                    kind: DiffLineKind::HunkHeader {
                        old_start,
                        old_count,
                        new_start,
                        new_count,
                    },
                    content: raw_line.to_string(),
                    old_line_no: None,
                    new_line_no: None,
                });
            }
            continue;
        }

        // No newline at end of file
        if raw_line.starts_with("\\ ") {
            file.lines.push(DiffLine {
                kind: DiffLineKind::NoNewlineAtEndOfFile,
                content: raw_line.to_string(),
                old_line_no: None,
                new_line_no: None,
            });
            continue;
        }

        // Context line
        if let Some(content) = raw_line.strip_prefix(' ') {
            file.lines.push(DiffLine {
                kind: DiffLineKind::Context,
                content: content.to_string(),
                old_line_no: Some(old_line_no),
                new_line_no: Some(new_line_no),
            });
            old_line_no += 1;
            new_line_no += 1;
            continue;
        }

        // Addition line
        if let Some(content) = raw_line.strip_prefix('+') {
            file.additions += 1;
            file.lines.push(DiffLine {
                kind: DiffLineKind::Addition,
                content: content.to_string(),
                old_line_no: None,
                new_line_no: Some(new_line_no),
            });
            new_line_no += 1;
            continue;
        }

        // Removal line
        if let Some(content) = raw_line.strip_prefix('-') {
            file.deletions += 1;
            file.lines.push(DiffLine {
                kind: DiffLineKind::Removal,
                content: content.to_string(),
                old_line_no: Some(old_line_no),
                new_line_no: None,
            });
            old_line_no += 1;
            continue;
        }
    }

    // Finalize last file
    if let Some(f) = current_file {
        files.push(f);
    }

    files
}

/// Extract the target file path from a `git diff --git a/path b/path` header.
///
/// The path after "b/" is the new/correct path. Falls back to the full string
/// if parsing fails.
fn extract_path_from_git_header(header: &str) -> String {
    // Header format: "a/path b/path"
    // Split on " b/" to get the new path
    if let Some(pos) = header.find(" b/") {
        header[pos + 3..].to_string()
    } else {
        // Fallback: just return the part after the first space
        header
            .split_whitespace()
            .nth(1)
            .unwrap_or(header)
            .to_string()
    }
}

/// Parse a hunk header line like `@@ -10,6 +10,15 @@ ...` into components.
///
/// Returns `(old_start, old_count, new_start, new_count)`.
fn parse_hunk_header(line: &str) -> Option<(u32, u32, u32, u32)> {
    // Format: @@ -old_start[,old_count] +new_start[,new_count] @@
    let line = line.trim_start_matches('@').trim();
    let parts: Vec<&str> = line.split_whitespace().take(2).collect();
    if parts.len() < 2 {
        return None;
    }

    let old_part = parts[0].strip_prefix('-')?;
    let new_part = parts[1].strip_prefix('+')?;

    let (old_start, old_count) = parse_range(old_part)?;
    let (new_start, new_count) = parse_range(new_part)?;

    Some((old_start, old_count, new_start, new_count))
}

/// Parse a range like "10,6" or "10" into `(start, count)`.
/// If no count is specified, count defaults to 1.
fn parse_range(range: &str) -> Option<(u32, u32)> {
    let mut parts = range.splitn(2, ',');
    let start: u32 = parts.next()?.parse().ok()?;
    let count: u32 = parts.next().and_then(|s| s.parse().ok()).unwrap_or(1);
    Some((start, count))
}

/// Scroll the diff review view by the given delta.
///
/// Clamps `scroll_offset` to valid bounds for the current file.
/// Returns `true` if the scroll actually changed.
pub fn scroll_diff(state: &mut DiffReviewState, delta: i32) -> bool {
    let file_idx = state
        .selected_file_index
        .min(state.files.len().saturating_sub(1));
    if let Some(file) = state.files.get(file_idx) {
        let total = file.lines.len();
        let max_offset = total.saturating_sub(1);
        let current = state.scroll_offset as i32;
        let new_offset = (current + delta).clamp(0, max_offset as i32) as usize;
        if new_offset != state.scroll_offset {
            state.scroll_offset = new_offset;
            return true;
        }
    }
    false
}

/// Navigate to the next file in the diff review view.
///
/// Wraps around to the first file. Resets scroll offset.
pub fn next_file(state: &mut DiffReviewState) {
    if state.files.is_empty() {
        return;
    }
    state.selected_file_index = (state.selected_file_index + 1) % state.files.len();
    state.scroll_offset = 0;
}

/// Navigate to the previous file in the diff review view.
///
/// Wraps around to the last file. Resets scroll offset.
pub fn prev_file(state: &mut DiffReviewState) {
    if state.files.is_empty() {
        return;
    }
    if state.selected_file_index == 0 {
        state.selected_file_index = state.files.len() - 1;
    } else {
        state.selected_file_index -= 1;
    }
    state.scroll_offset = 0;
}

// ─── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_simple_diff() {
        let output = r#"diff --git a/src/main.rs b/src/main.rs
index abc1234..def5678 100644
--- a/src/main.rs
+++ b/src/main.rs
@@ -1,4 +1,6 @@
 fn main() {
+    println!("hello");
+    println!("world");
     let x = 1;
-    let y = 2;
+    let y = 3;
 }
"#;
        let files = parse_git_diff(output);
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, "src/main.rs");
        assert_eq!(files[0].additions, 3);
        assert_eq!(files[0].deletions, 1);
        assert!(!files[0].is_new);
        assert!(!files[0].is_deleted);
        assert!(!files[0].is_binary);

        // Check line types
        let kinds: Vec<&DiffLineKind> = files[0].lines.iter().map(|l| &l.kind).collect();
        assert!(matches!(kinds[0], DiffLineKind::HunkHeader { .. }));
        assert!(matches!(kinds[1], DiffLineKind::Context { .. }));
        assert!(matches!(kinds[2], DiffLineKind::Addition));
        assert!(matches!(kinds[3], DiffLineKind::Addition));
        assert!(matches!(kinds[4], DiffLineKind::Context));
        assert!(matches!(kinds[5], DiffLineKind::Removal));
        assert!(matches!(kinds[6], DiffLineKind::Addition));
        assert!(matches!(kinds[7], DiffLineKind::Context));
    }

    #[test]
    fn parse_new_file() {
        let output = r#"diff --git a/src/new_file.rs b/src/new_file.rs
new file mode 100644
index 0000000..abc1234
--- /dev/null
+++ b/src/new_file.rs
@@ -0,0 +1,3 @@
+fn new_func() {
+    println!("new");
+}
"#;
        let files = parse_git_diff(output);
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, "src/new_file.rs");
        assert!(files[0].is_new);
        assert_eq!(files[0].additions, 3);
        assert_eq!(files[0].deletions, 0);
    }

    #[test]
    fn parse_deleted_file() {
        let output = r#"diff --git a/src/old_file.rs b/src/old_file.rs
deleted file mode 100644
index abc1234..0000000
--- a/src/old_file.rs
+++ /dev/null
@@ -1,2 +0,0 @@
-fn old() {
-}
"#;
        let files = parse_git_diff(output);
        assert_eq!(files.len(), 1);
        assert!(files[0].is_deleted);
        assert_eq!(files[0].additions, 0);
        assert_eq!(files[0].deletions, 2);
    }

    #[test]
    fn parse_renamed_file() {
        let output = r#"diff --git a/src/old_name.rs b/src/new_name.rs
similarity index 100%
rename from src/old_name.rs
rename to src/new_name.rs
"#;
        let files = parse_git_diff(output);
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, "src/new_name.rs");
        assert_eq!(files[0].old_path.as_deref(), Some("src/old_name.rs"));
        assert!(files[0].is_renamed);
    }

    #[test]
    fn parse_binary_file() {
        let output = r#"diff --git a/image.png b/image.png
Binary files /dev/null differ
"#;
        let files = parse_git_diff(output);
        assert_eq!(files.len(), 1);
        assert!(files[0].is_binary);
        assert!(files[0].is_new);
    }

    #[test]
    fn parse_no_newline_at_end() {
        let output = r#"diff --git a/file.txt b/file.txt
--- a/file.txt
+++ b/file.txt
@@ -1,2 +1,2 @@
 line1
-line2
+line2_modified
\ No newline at end of file
"#;
        let files = parse_git_diff(output);
        assert_eq!(files.len(), 1);
        let last = files[0].lines.last().unwrap();
        assert!(matches!(last.kind, DiffLineKind::NoNewlineAtEndOfFile));
    }

    #[test]
    fn parse_multiple_files() {
        let output = r#"diff --git a/file1.rs b/file1.rs
--- a/file1.rs
+++ b/file1.rs
@@ -1,2 +1,2 @@
-old1
+new1
diff --git a/file2.rs b/file2.rs
--- a/file2.rs
+++ b/file2.rs
@@ -1,2 +1,2 @@
-old2
+new2
"#;
        let files = parse_git_diff(output);
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].path, "file1.rs");
        assert_eq!(files[1].path, "file2.rs");
    }

    #[test]
    fn parse_empty_diff() {
        let files = parse_git_diff("");
        assert!(files.is_empty());
    }

    #[test]
    fn parse_hunk_header_single_line() {
        // @@ -1 +1 @@ (no count specified, defaults to 1)
        let output = r#"diff --git a/f.txt b/f.txt
--- a/f.txt
+++ b/f.txt
@@ -1 +1 @@
-old
+new
"#;
        let files = parse_git_diff(output);
        assert_eq!(files.len(), 1);
        if let DiffLineKind::HunkHeader {
            old_start,
            old_count,
            new_start,
            new_count,
        } = &files[0].lines[0].kind
        {
            assert_eq!(*old_start, 1);
            assert_eq!(*old_count, 1);
            assert_eq!(*new_start, 1);
            assert_eq!(*new_count, 1);
        } else {
            panic!("Expected HunkHeader");
        }
    }

    #[test]
    fn parse_range_with_count() {
        assert_eq!(parse_range("10,6"), Some((10, 6)));
    }

    #[test]
    fn parse_range_without_count() {
        assert_eq!(parse_range("10"), Some((10, 1)));
    }

    #[test]
    fn extract_path_from_header() {
        assert_eq!(
            extract_path_from_git_header("a/src/main.rs b/src/main.rs"),
            "src/main.rs"
        );
    }

    #[test]
    fn next_file_wraps_around() {
        let mut state = DiffReviewState {
            files: vec![
                DiffFile {
                    path: "a.rs".into(),
                    old_path: None,
                    additions: 0,
                    deletions: 0,
                    is_new: false,
                    is_deleted: false,
                    is_binary: false,
                    is_renamed: false,
                    lines: Vec::new(),
                },
                DiffFile {
                    path: "b.rs".into(),
                    old_path: None,
                    additions: 0,
                    deletions: 0,
                    is_new: false,
                    is_deleted: false,
                    is_binary: false,
                    is_renamed: false,
                    lines: Vec::new(),
                },
            ],
            selected_file_index: 1,
            scroll_offset: 5,
            error: None,
            task_number: 1,
        };
        next_file(&mut state);
        assert_eq!(state.selected_file_index, 0);
        assert_eq!(state.scroll_offset, 0);
    }

    #[test]
    fn prev_file_wraps_around() {
        let mut state = DiffReviewState {
            files: vec![
                DiffFile {
                    path: "a.rs".into(),
                    old_path: None,
                    additions: 0,
                    deletions: 0,
                    is_new: false,
                    is_deleted: false,
                    is_binary: false,
                    is_renamed: false,
                    lines: Vec::new(),
                },
                DiffFile {
                    path: "b.rs".into(),
                    old_path: None,
                    additions: 0,
                    deletions: 0,
                    is_new: false,
                    is_deleted: false,
                    is_binary: false,
                    is_renamed: false,
                    lines: Vec::new(),
                },
            ],
            selected_file_index: 0,
            scroll_offset: 5,
            error: None,
            task_number: 1,
        };
        prev_file(&mut state);
        assert_eq!(state.selected_file_index, 1);
        assert_eq!(state.scroll_offset, 0);
    }

    #[test]
    fn scroll_diff_clamps_to_bounds() {
        let mut state = DiffReviewState {
            files: vec![DiffFile {
                path: "a.rs".into(),
                old_path: None,
                additions: 1,
                deletions: 1,
                is_new: false,
                is_deleted: false,
                is_binary: false,
                is_renamed: false,
                lines: vec![
                    DiffLine {
                        kind: DiffLineKind::Context,
                        content: "x".into(),
                        old_line_no: Some(1),
                        new_line_no: Some(1),
                    },
                    DiffLine {
                        kind: DiffLineKind::Addition,
                        content: "y".into(),
                        old_line_no: None,
                        new_line_no: Some(2),
                    },
                ],
            }],
            selected_file_index: 0,
            scroll_offset: 0,
            error: None,
            task_number: 1,
        };

        // Scroll down past the end — should clamp to max
        assert!(scroll_diff(&mut state, 10));
        assert_eq!(state.scroll_offset, 1);

        // Scroll up past the start — should clamp to 0
        assert!(scroll_diff(&mut state, -10));
        assert_eq!(state.scroll_offset, 0);

        // No-op scroll at boundary
        assert!(!scroll_diff(&mut state, -1));
        assert_eq!(state.scroll_offset, 0);
    }
}
