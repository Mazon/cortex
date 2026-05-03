//! Reports mode key handler — toggle reports, navigate tasks/commits, open task detail.

use super::super::App;
use super::super::utils;

/// Toggle the reports view on/off.
pub fn handle_reports_toggle(app: &mut App) {
    let mut state = app.state.lock().unwrap_or_else(|e| e.into_inner());
    if state.ui.mode == crate::state::types::AppMode::Reports {
        // Already in reports — close it
        state.ui.mode = crate::state::types::AppMode::Normal;
        state.ui.reports = None;
    } else {
        // Enter reports mode
        state.ui.mode = crate::state::types::AppMode::Reports;
        drop(state);
        crate::tui::reports::load_reports_data(&app.state);
    }
}

/// Handle key events in Reports mode.
pub fn handle_reports_key(app: &mut App, key: crossterm::event::KeyEvent) {
    use crate::state::types::FocusedPanel;
    use crate::state::types::ReportsFocusedPane;
    use crossterm::event::KeyCode;

    match key.code {
        KeyCode::Esc | KeyCode::Char('q') => {
            let mut state = app.state.lock().unwrap_or_else(|e| e.into_inner());
            state.ui.mode = crate::state::types::AppMode::Normal;
            state.ui.reports = None;
        }
        KeyCode::Tab => {
            // Toggle focused_pane between Tasks and Commits
            let mut state = app.state.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(ref mut reports) = state.ui.reports {
                reports.focused_pane = match reports.focused_pane {
                    ReportsFocusedPane::Tasks => ReportsFocusedPane::Commits,
                    ReportsFocusedPane::Commits => ReportsFocusedPane::Tasks,
                };
            }
        }
        KeyCode::Enter => {
            // Open task detail for the selected task (only when tasks pane is focused)
            let result: Option<(String, bool, Option<String>)> = {
                let mut state = app.state.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(ref reports) = state.ui.reports {
                    if reports.focused_pane == ReportsFocusedPane::Tasks {
                        if let Some(tid) = reports.task_ids.get(reports.selected_task_index) {
                            let tid = tid.clone();
                            let (reviewable, wd) = {
                                let task = state.tasks.get(&tid);
                                let reviewable = task.map(|t| {
                                    matches!(
                                        t.agent_status,
                                        crate::state::types::AgentStatus::Complete
                                            | crate::state::types::AgentStatus::Ready
                                    )
                                }).unwrap_or(false);
                                let wd = task.and_then(|t| {
                                    state
                                        .project_registry
                                        .projects
                                        .iter()
                                        .find(|p| p.id == t.project_id)
                                        .filter(|p| !p.working_directory.is_empty())
                                        .map(|p| p.working_directory.clone())
                                });
                                (reviewable, wd)
                            };
                            // Exit reports mode
                            state.ui.mode = crate::state::types::AppMode::Normal;
                            state.ui.reports = None;
                            // Set focused task
                            state.ui.focused_task_id = Some(tid.clone());
                            // Update kanban focused index for the task's column
                            if let Some(task) = state.tasks.get(&tid) {
                                let col_id = task.column.0.clone();
                                if let Some(task_ids) = state.kanban.columns.get(&col_id) {
                                    if let Some(idx) = task_ids.iter().position(|id| id == &tid) {
                                        state.kanban.focused_task_index.insert(col_id.clone(), idx);
                                    }
                                }
                                state.ui.focused_column = col_id;
                            }
                            // Open task detail
                            state.open_task_detail(&tid);
                            state.ui.diff_review_source = Some(FocusedPanel::TaskDetail);
                            Some((tid, reviewable, wd))
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                } else {
                    None
                }
            };

            // Async load changed files for reviewable tasks
            if let Some((tid, reviewable, wd)) = result {
                if reviewable {
                    if let Some(wd) = wd {
                        let state = app.state.clone();
                        tokio::task::spawn_blocking(move || {
                            let numstat = std::process::Command::new("git")
                                .args(["diff", "--numstat", "HEAD"])
                                .current_dir(&wd)
                                .output();
                            let name_status = std::process::Command::new("git")
                                .args(["diff", "--name-status", "HEAD"])
                                .current_dir(&wd)
                                .output();

                            let files =
                                if let (Ok(ns_out), Ok(ns_stat)) = (numstat, name_status) {
                                    if ns_out.status.success() && ns_stat.status.success() {
                                        utils::parse_changed_files(&ns_out.stdout, &ns_stat.stdout)
                                    } else {
                                        Vec::new()
                                    }
                                } else {
                                    Vec::new()
                                };

                            let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
                            if state.ui.viewing_task_id.as_deref() == Some(&tid) {
                                state.ui.changed_files = if files.is_empty() {
                                    None
                                } else {
                                    Some(files)
                                };
                                state.ui.selected_changed_file_index = 0;
                                state.mark_render_dirty();
                            }
                        });
                    }
                }
            }
        }
        KeyCode::Char('j') | KeyCode::Down => {
            let mut state = app.state.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(ref mut reports) = state.ui.reports {
                match reports.focused_pane {
                    ReportsFocusedPane::Tasks => {
                        crate::tui::reports::scroll_tasks(reports, 1);
                    }
                    ReportsFocusedPane::Commits => {
                        crate::tui::reports::scroll_reports(reports, 1);
                    }
                }
            }
        }
        KeyCode::Char('k') | KeyCode::Up => {
            let mut state = app.state.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(ref mut reports) = state.ui.reports {
                match reports.focused_pane {
                    ReportsFocusedPane::Tasks => {
                        crate::tui::reports::scroll_tasks(reports, -1);
                    }
                    ReportsFocusedPane::Commits => {
                        crate::tui::reports::scroll_reports(reports, -1);
                    }
                }
            }
        }
        _ => {}
    }
}
