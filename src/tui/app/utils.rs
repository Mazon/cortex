//! Shared utilities extracted from duplicated code in app.rs.
//!
//! - `parse_changed_files`: git diff parsing (was duplicated in
//!   `handle_open_task_detail` and `handle_reports_key`).
//! - `resolve_question_with_reassess`: question-resolution logic (was
//!   duplicated in `handle_normal_key` and `handle_modal_confirm`).

use crate::opencode::client::OpenCodeClient;
use crate::state::types::{ChangedFileInfo, FileChangeStatus};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// Parse the output of `git diff --numstat HEAD` and `git diff --name-status HEAD`
/// into a list of [`ChangedFileInfo`] entries.
///
/// This was duplicated in `handle_open_task_detail` (~line 1630) and
/// `handle_reports_key` (~line 3091). Extracted to avoid further divergence.
pub fn parse_changed_files(
    numstat_stdout: &[u8],
    name_status_stdout: &[u8],
) -> Vec<ChangedFileInfo> {
    let mut files = Vec::new();

    // Parse name-status into a map: path -> status + old_path
    let mut status_map: HashMap<String, (FileChangeStatus, Option<String>)> = HashMap::new();
    for line in String::from_utf8_lossy(name_status_stdout).lines() {
        let parts: Vec<&str> = line.splitn(3, '\t').collect();
        if parts.len() >= 2 {
            let status = match parts[0] {
                "A" => FileChangeStatus::Added,
                "M" => FileChangeStatus::Modified,
                "D" => FileChangeStatus::Deleted,
                "R" => FileChangeStatus::Renamed,
                "C" => FileChangeStatus::Copied,
                _ => FileChangeStatus::Modified,
            };
            let old_path =
                if (status == FileChangeStatus::Renamed || status == FileChangeStatus::Copied)
                    && parts.len() >= 3
                {
                    Some(parts[1].to_string())
                } else {
                    None
                };
            let path = if old_path.is_some() {
                parts.get(2).unwrap_or(&parts[1]).to_string()
            } else {
                parts[1].to_string()
            };
            status_map.insert(path, (status, old_path));
        }
    }

    // Parse numstat into counts
    let mut count_map: HashMap<String, (u32, u32)> = HashMap::new();
    for line in String::from_utf8_lossy(numstat_stdout).lines() {
        let parts: Vec<&str> = line.splitn(3, '\t').collect();
        if parts.len() >= 3 {
            let adds: u32 = parts[0].parse().unwrap_or(0);
            let dels: u32 = parts[1].parse().unwrap_or(0);
            // Binary files show "-\t-\tpath"
            let real_adds = if parts[0] == "-" { 0 } else { adds };
            let real_dels = if parts[1] == "-" { 0 } else { dels };
            count_map.insert(parts[2].to_string(), (real_adds, real_dels));
        }
    }

    // Merge: use status_map as the source of truth for paths
    for (path, (status, old_path)) in &status_map {
        let (additions, deletions) = count_map.get(path).copied().unwrap_or((0, 0));
        files.push(ChangedFileInfo {
            path: path.clone(),
            old_path: old_path.clone(),
            status: status.clone(),
            additions,
            deletions,
        });
    }

    files
}

/// Resolve a question and optionally trigger task reassessment / auto-progression.
///
/// This was duplicated in `handle_normal_key` (answering questions via digit keys)
/// and `handle_modal_confirm` (answering questions via modal).
pub fn resolve_question_with_reassess(
    state: Arc<Mutex<crate::state::types::AppState>>,
    client: OpenCodeClient,
    question_id: String,
    session_id: String,
    answer: String,
    task_id: String,
    columns_config: crate::config::types::ColumnsConfig,
    opencode_config: crate::config::types::OpenCodeConfig,
) {
    let answer_preview = answer.chars().take(30).collect::<String>();
    tokio::spawn(async move {
        match client
            .resolve_question(&session_id, &question_id, &answer)
            .await
        {
            Ok(()) => {
                let mut s = state.lock().unwrap_or_else(|e| e.into_inner());
                s.resolve_question_request(&task_id, &question_id);

                // Check if task should transition out of Question status
                let needs_reassess = s.should_reassess_after_question(&task_id);
                if needs_reassess {
                    let (status, should_progress) =
                        crate::opencode::events::determine_completion_status(&mut s, &task_id);
                    s.update_task_agent_status(&task_id, status);

                    if should_progress {
                        let action = crate::orchestration::engine::on_agent_completed(
                            &task_id,
                            &mut s,
                            &columns_config,
                        );
                        if let Some(a) = action {
                            match a {
                                crate::orchestration::engine::AgentCompletionAction::AutoProgress(
                                    ap,
                                ) => {
                                    let col = ap.target_column.clone();
                                    let tid_clone = task_id.clone();
                                    drop(s);
                                    crate::orchestration::engine::on_task_moved(
                                        &tid_clone,
                                        &col,
                                        &state,
                                        &client,
                                        &columns_config,
                                        &opencode_config,
                                        None,
                                    );
                                }
                                crate::orchestration::engine::AgentCompletionAction::SendQueuedPrompt {
                                    task_id: qp_tid,
                                    prompt: qp_prompt,
                                    session_id: qp_sid,
                                    agent_type: qp_agent,
                                } => {
                                    drop(s);
                                    crate::orchestration::engine::send_follow_up_prompt(
                                        &qp_tid,
                                        &qp_prompt,
                                        &qp_sid,
                                        &qp_agent,
                                        &state,
                                        &client,
                                        &opencode_config,
                                    );
                                }
                            }
                        }
                    }
                }

                let mut s = state.lock().unwrap_or_else(|e| e.into_inner());
                crate::tui::app::permission::sync_modal_after_resolve(&mut s, &task_id);
                s.set_notification(
                    format!("Answered: {}", answer_preview),
                    crate::state::types::NotificationVariant::Success,
                    3000,
                );
                s.mark_render_dirty();
            }
            Err(e) => {
                tracing::error!("Failed to resolve question {}: {}", question_id, e);
                let mut s = state.lock().unwrap_or_else(|e| e.into_inner());
                s.set_notification(
                    format!("Failed to answer question: {}", e),
                    crate::state::types::NotificationVariant::Error,
                    5000,
                );
                s.mark_render_dirty();
            }
        }
    });
}
