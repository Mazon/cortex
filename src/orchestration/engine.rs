//! Orchestration engine — config-driven task progression rules.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::config::types::{ColumnsConfig, OpenCodeConfig};
use crate::error::AppError;
use crate::opencode::client::OpenCodeClient;
use crate::state::types::{AppState, KanbanColumn};

/// Maximum backoff delay cap (30 seconds).
const MAX_BACKOFF_DELAY: Duration = Duration::from_secs(30);

/// Check if an error is transient and worth retrying.
///
/// Non-retryable errors include client-side HTTP errors (4xx, except 429 rate limit).
/// Server errors (5xx), rate limits (429), and network-level errors are retryable.
fn is_retryable(error: &anyhow::Error) -> bool {
    let msg = error.to_string();
    // Check for HTTP status codes in the error message (SDK wraps errors in anyhow)
    // Non-retryable: 4xx client errors (except 429 Too Many Requests)
    for token in msg.split_whitespace() {
        if let Some(code_str) = token.strip_prefix('(').and_then(|s| s.strip_suffix(')')) {
            if let Ok(code) = code_str.parse::<u16>() {
                if (400..429).contains(&code) || (430..500).contains(&code) {
                    return false;
                }
                // 429 (rate limit) and 5xx are retryable
            }
        }
    }
    true
}

/// Structured retry classification using [`AppError`].
///
/// This is the **intended** replacement for the string-based [`is_retryable`]
/// above.  Once the call-sites in this crate return [`AppResult`] instead of
/// `anyhow::Result`, the legacy function can be removed and this one used
/// directly via [`AppError::is_retryable`].
///
/// Kept as a free function (rather than calling `AppError::is_retryable`)
/// so that the migration path is explicit and easy to grep for.
pub fn is_retryable_app(error: &AppError) -> bool {
    error.is_retryable()
}

/// Retry an async operation with exponential backoff.
///
/// Attempts `operation` up to `max_attempts` times. On each failure the
/// delay doubles starting from `initial_delay` (500 ms → 1 s → 2 s …),
/// capped at 30 seconds. Non-retryable errors (e.g. 4xx HTTP) cause an
/// immediate return without retrying.
/// A `tracing::warn!` is emitted for every retry so operators can see
/// transient hiccups in the logs.
async fn retry_with_backoff<F, Fut, T>(
    max_attempts: usize,
    initial_delay: Duration,
    operation: F,
) -> anyhow::Result<T>
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = anyhow::Result<T>>,
{
    let mut delay = initial_delay;
    let mut last_error = None;

    for attempt in 0..max_attempts {
        match operation().await {
            Ok(result) => return Ok(result),
            Err(e) => {
                if !is_retryable(&e) {
                    return Err(e);
                }
                last_error = Some(e);
                if attempt + 1 < max_attempts {
                    tracing::warn!(
                        attempt = attempt + 1,
                        max_attempts,
                        retry_after_ms = delay.as_millis() as u64,
                        error = %last_error.as_ref().unwrap(),
                        "Operation failed, retrying with exponential backoff"
                    );
                    tokio::time::sleep(delay).await;
                    delay = (delay * 2).min(MAX_BACKOFF_DELAY);
                }
            }
        }
    }

    Err(last_error.expect("retry_with_backoff called with max_attempts >= 1"))
}

/// Called when a task is moved to a new column.
/// Starts an agent if the column has one configured.
pub fn on_task_moved(
    task_id: &str,
    to_column: &KanbanColumn,
    state: &Arc<Mutex<AppState>>,
    client: &OpenCodeClient,
    columns_config: &ColumnsConfig,
    opencode_config: &OpenCodeConfig,
    previous_agent: Option<String>,
) {
    let agent = columns_config.agent_for_column(&to_column.0);
    tracing::debug!(
        "on_task_moved: task={}, to_column={}, resolved_agent={:?}, previous_agent={:?}",
        task_id, to_column.0, agent, previous_agent
    );
    if let Some(agent) = agent {
        start_agent(task_id, &agent, state, client, opencode_config, previous_agent);
    }
}

/// Start an agent for a task.
fn start_agent(
    task_id: &str,
    agent: &str,
    state: &Arc<Mutex<AppState>>,
    client: &OpenCodeClient,
    opencode_config: &OpenCodeConfig,
    previous_agent: Option<String>,
) {
    // Log the full dispatch decision for diagnostics
    {
        let s = state.lock().unwrap();
        let session_id = s.tasks.get(task_id).and_then(|t| t.session_id.clone());
        let agent_changed = previous_agent.as_deref() != Some(agent);
        tracing::debug!(
            "start_agent: task={}, agent={}, previous_agent={:?}, session_id={:?}, agent_changed={}",
            task_id, agent, previous_agent, session_id, agent_changed
        );
    }

    // Status is already set to Running by the caller (app.rs) to close the race window.
    // No need to re-acquire the lock here for status update.

    // Clone what we need, spawn async work
    let state = state.clone();
    let client = client.clone();
    let agent = agent.to_string();
    let task_id = task_id.to_string();
    let model_id = opencode_config.model.id.clone();
    let model_provider = opencode_config.model.provider.clone();

    // Check if the agent has a specific model configured
    let agent_model = {
        let s = state.lock().unwrap();
        s.tasks.get(&task_id).map(|_| {
            opencode_config.agents.get(&agent).and_then(|a| a.model.clone())
        }).flatten()
    };

    tokio::spawn(async move {
        // Build prompt from task WHILE holding the lock to prevent stale data
        let (prompt, session_id, task_id_clone) = {
            let s = state.lock().unwrap();
            let task = match s.tasks.get(&task_id) {
                Some(t) => t,
                None => {
                    return;
                }
            };

            let prompt = OpenCodeClient::build_prompt_for_agent(task, &agent, None);
            (prompt, task.session_id.clone(), task.id.clone())
        };

        // Create session if needed
        let sid = if let Some(ref existing_sid) = session_id {
            // Check if the agent type changed — if so, create a fresh session
            // to avoid cross-contaminating the new agent with old conversation history.
            // We use previous_agent (captured before the lock-set) rather than reading
            // the current task.agent_type, which has already been updated to the new agent.
            let agent_changed = previous_agent.as_deref() != Some(agent.as_str());
            if agent_changed {
                let old_session_id = session_id.clone();

                // Synchronously abort the old session BEFORE creating a new one.
                // This prevents server-side session accumulation that can cause
                // SendRequest errors when concurrent sessions exhaust server resources.
                if let Some(ref old_sid) = old_session_id {
                    match tokio::time::timeout(
                        Duration::from_secs(10),
                        client.abort_session(old_sid),
                    ).await {
                        Ok(Ok(true)) => {},
                        Ok(Ok(false)) => tracing::warn!("Abort returned false for old session {}", old_sid),
                        Ok(Err(e)) => tracing::warn!("Failed to abort old session {}: {}", old_sid, e),
                        Err(_) => tracing::warn!("Timeout aborting old session {} after 10s", old_sid),
                    }
                }

                // Clear the old session mapping and create a new one
                {
                    let mut s = state.lock().unwrap();
                    s.set_task_session_id(&task_id_clone, None);
                }
                match retry_with_backoff(3, Duration::from_millis(500), || client.create_session()).await {
                    Ok(session) => {
                        let sid = session.id.clone();
                        let mut s = state.lock().unwrap();
                        s.set_task_session_id(&task_id_clone, Some(sid.clone()));
                        sid
                    }
                    Err(e) => {
                        tracing::error!(
                            task_id = %task_id_clone,
                            agent = %agent,
                            "Failed to create session after retries: {}",
                            e
                        );
                        let mut s = state.lock().unwrap();
                        s.set_task_error(&task_id_clone, format!("Failed to create session: {}", e));
                        return;
                    }
                }
            } else {
                existing_sid.clone()
            }
        } else {
            match retry_with_backoff(3, Duration::from_millis(500), || client.create_session()).await {
                Ok(session) => {
                    let sid = session.id.clone();
                    // Store session ID
                    {
                        let mut s = state.lock().unwrap();
                        s.set_task_session_id(&task_id_clone, Some(sid.clone()));
                    }
                    sid
                }
                Err(e) => {
                    tracing::error!(
                        task_id = %task_id_clone,
                        agent = %agent,
                        "Failed to create session after retries: {}",
                        e
                    );
                    let mut s = state.lock().unwrap();
                    s.set_task_error(&task_id_clone, format!("Failed to create session: {}", e));
                    return;
                }
            }
        };

        // Determine model
        let model = agent_model.as_deref().map(|m| {
            if m.contains('/') {
                m.to_string()
            } else {
                let provider = model_provider.as_deref().unwrap_or("z.ai");
                format!("{}/{}", provider, m)
            }
        }).or_else(|| {
            let provider = model_provider.as_deref().unwrap_or("z.ai");
            Some(format!("{}/{}", provider, model_id))
        });

        // Send prompt (with retry to tolerate transient HTTP errors)
        match retry_with_backoff(
            3,
            Duration::from_millis(500),
            || client.send_prompt(&sid, &prompt, Some(&agent), model.as_deref()),
        )
        .await
        {
            Ok(_) => {}
            Err(e) => {
                tracing::error!(
                    task_id = %task_id_clone,
                    agent = %agent,
                    session_id = %sid,
                    "Failed to send prompt after retries: {}",
                    e
                );
                let mut s = state.lock().unwrap();
                s.set_task_error(
                    &task_id_clone,
                    format!("Failed to send prompt: {}", e),
                );
            }
        }
    });
}

/// Action returned by `on_agent_completed` for the caller to execute
/// after releasing the MutexGuard.
pub struct AutoProgressAction {
    pub task_id: String,
    pub target_column: KanbanColumn,
    pub agent: String,
}

/// Called when an agent completes (from SSE SessionIdle).
/// Auto-progresses if the column configures it and returns an action
/// for the caller to start the target column's agent (if configured).
pub fn on_agent_completed(
    task_id: &str,
    state: &mut AppState,
    columns_config: &ColumnsConfig,
) -> Option<AutoProgressAction> {
    let column = state
        .tasks
        .get(task_id)
        .map(|t| t.column.clone());
    if let Some(col) = column {
        if let Some(target) = columns_config.auto_progress_for(&col.0) {
            state.move_task(task_id, KanbanColumn(target.clone()));

            // Check if target column has an agent configured
            if let Some(agent) = columns_config.agent_for_column(&target) {
                return Some(AutoProgressAction {
                    task_id: task_id.to_string(),
                    target_column: KanbanColumn(target),
                    agent,
                });
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::types::{ColumnConfig, ColumnsConfig};
    use crate::state::types::*;

    /// Build a minimal `AppState` with a task in a given column.
    fn make_state_with_task_in_column(column: &str) -> (AppState, String) {
        let mut state = AppState::default();
        let project = CortexProject {
            id: "proj-1".to_string(),
            name: "Test Project".to_string(),
            working_directory: "/tmp".to_string(),
            status: ProjectStatus::Idle,
            position: 0,
        };
        state.add_project(project);
        state.active_project_id = Some("proj-1".to_string());

        let task_id = "task-1".to_string();
        let task = CortexTask {
            id: task_id.clone(),
            number: 1,
            title: "Test Task".to_string(),
            description: String::new(),
            column: KanbanColumn(column.to_string()),
            session_id: None,
            agent_type: Some("planning".to_string()),
            agent_status: AgentStatus::Complete,
            entered_column_at: 1000,
            last_activity_at: 1000,
            error_message: None,
            plan_output: None,
            pending_permission_count: 0,
            pending_question_count: 0,
            created_at: 1000,
            updated_at: 1000,
            project_id: "proj-1".to_string(),
        };
        state.tasks.insert(task_id.clone(), task);
        state
            .kanban
            .columns
            .entry(column.to_string())
            .or_default()
            .push(task_id.clone());

        (state, task_id)
    }

    /// Build a `ColumnsConfig` where `from_col` auto-progresses to `to_col`.
    fn make_columns_with_auto_progress(from_col: &str, to_col: &str) -> ColumnsConfig {
        let mut config = ColumnsConfig {
            definitions: vec![
                ColumnConfig {
                    id: from_col.to_string(),
                    display_name: Some(from_col.to_string()),
                    visible: true,
                    agent: Some("planning".to_string()),
                    auto_progress_to: Some(to_col.to_string()),
                },
                ColumnConfig {
                    id: to_col.to_string(),
                    display_name: Some(to_col.to_string()),
                    visible: true,
                    agent: Some("do".to_string()),
                    auto_progress_to: None,
                },
            ],
            visible_ids: Vec::new(),
        };
        config.finalize();
        config
    }

    // ── Auto-progression ────────────────────────────────────────────────

    #[test]
    fn auto_progress_moves_task_to_target_column() {
        let (mut state, task_id) = make_state_with_task_in_column("planning");
        let columns_config = make_columns_with_auto_progress("planning", "running");

        let action = on_agent_completed(&task_id, &mut state, &columns_config);

        // Task should now be in "running"
        assert_eq!(state.tasks.get(&task_id).unwrap().column.0, "running");
        assert!(state
            .kanban
            .columns
            .get("running")
            .unwrap()
            .contains(&task_id));
        assert!(!state
            .kanban
            .columns
            .get("planning")
            .unwrap()
            .contains(&task_id));
        // Should return an action to start the "do" agent
        assert!(action.is_some());
        let action = action.unwrap();
        assert_eq!(action.task_id, task_id);
        assert_eq!(action.target_column.0, "running");
        assert_eq!(action.agent, "do");
    }

    #[test]
    fn auto_progress_updates_entered_column_at() {
        let (mut state, task_id) = make_state_with_task_in_column("planning");
        let columns_config = make_columns_with_auto_progress("planning", "running");

        let old_entered = state.tasks.get(&task_id).unwrap().entered_column_at;
        let _action = on_agent_completed(&task_id, &mut state, &columns_config);

        let new_entered = state.tasks.get(&task_id).unwrap().entered_column_at;
        assert!(new_entered >= old_entered);
    }

    #[test]
    fn no_auto_progress_when_not_configured() {
        let (mut state, task_id) = make_state_with_task_in_column("running");

        // Config where "running" has no auto_progress_to
        let mut columns_config = make_columns_with_auto_progress("planning", "running");
        // Override "running" to have no auto-progression
        columns_config.definitions[1].auto_progress_to = None;

        let action = on_agent_completed(&task_id, &mut state, &columns_config);

        // Task should stay in "running"
        assert_eq!(state.tasks.get(&task_id).unwrap().column.0, "running");
        assert!(state
            .kanban
            .columns
            .get("running")
            .unwrap()
            .contains(&task_id));
        // No action returned since no auto-progression
        assert!(action.is_none());
    }

    #[test]
    fn no_auto_progress_for_nonexistent_task() {
        let (mut state, _task_id) = make_state_with_task_in_column("planning");
        let columns_config = make_columns_with_auto_progress("planning", "running");

        // Should not panic for a nonexistent task
        let action = on_agent_completed("nonexistent-task", &mut state, &columns_config);

        // Original task should still be in planning
        assert!(state
            .kanban
            .columns
            .get("planning")
            .unwrap()
            .contains(&_task_id));
        // No action for nonexistent task
        assert!(action.is_none());
    }

    #[test]
    fn auto_progress_chains_through_multiple_columns() {
        let (mut state, task_id) = make_state_with_task_in_column("planning");

        // Chain: planning → running → review
        let mut columns_config = ColumnsConfig {
            definitions: vec![
                ColumnConfig {
                    id: "planning".to_string(),
                    display_name: None,
                    visible: true,
                    agent: Some("planning".to_string()),
                    auto_progress_to: Some("running".to_string()),
                },
                ColumnConfig {
                    id: "running".to_string(),
                    display_name: None,
                    visible: true,
                    agent: Some("do".to_string()),
                    auto_progress_to: Some("review".to_string()),
                },
                ColumnConfig {
                    id: "review".to_string(),
                    display_name: None,
                    visible: true,
                    agent: Some("reviewer".to_string()),
                    auto_progress_to: None,
                },
            ],
            visible_ids: Vec::new(),
        };
        columns_config.finalize();

        // First completion: planning → running
        let action = on_agent_completed(&task_id, &mut state, &columns_config);
        assert_eq!(state.tasks.get(&task_id).unwrap().column.0, "running");
        assert_eq!(action.unwrap().agent, "do");

        // Second completion: running → review
        let action = on_agent_completed(&task_id, &mut state, &columns_config);
        assert_eq!(state.tasks.get(&task_id).unwrap().column.0, "review");
        assert_eq!(action.unwrap().agent, "reviewer");

        // Third completion: review stays (no auto_progress_to)
        let action = on_agent_completed(&task_id, &mut state, &columns_config);
        assert_eq!(state.tasks.get(&task_id).unwrap().column.0, "review");
        assert!(action.is_none());
    }
}
