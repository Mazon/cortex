//! Orchestration engine — config-driven task progression rules.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::Semaphore;

use crate::config::types::{ColumnsConfig, OpenCodeConfig};
use crate::error::AppError;
use crate::opencode::client::OpenCodeClient;
use crate::state::types::{AgentStatus, AppState, KanbanColumn, ReviewStatus};

/// Maximum backoff delay cap (30 seconds).
const MAX_BACKOFF_DELAY: Duration = Duration::from_secs(30);

/// Per-project semaphores for limiting concurrent agent sessions.
/// Uses `LazyLock` for thread-safe lazy initialization.
static AGENT_SEMAPHORES: std::sync::LazyLock<std::sync::Mutex<HashMap<String, Arc<Semaphore>>>> =
    std::sync::LazyLock::new(|| std::sync::Mutex::new(HashMap::new()));

/// Get or create a semaphore for the given project.
fn get_agent_semaphore(project_id: &str, max_concurrent: usize) -> Arc<Semaphore> {
    let mut map = AGENT_SEMAPHORES.lock().unwrap_or_else(|e| e.into_inner());
    map.entry(project_id.to_string())
        .or_insert_with(|| Arc::new(Semaphore::new(max_concurrent)))
        .clone()
}

/// Classify an [`anyhow::Error`] as retryable or not.
///
/// This is the **primary** retry-decision function used by [`retry_with_backoff`].
///
/// # Classification strategy
///
/// 1. **Structured path** — If the error carries an [`AppError`] (via
///    `downcast_ref`), we delegate to [`AppError::is_retryable`] which matches
///    on enum variants rather than string content.
/// 2. **Fallback heuristic** — For plain `anyhow::Error` values that originate
///    from the SDK (which still wraps errors in `anyhow::anyhow!`), we parse
///    the message for HTTP status codes in `(NNN)` form.  This heuristic is
///    intentionally conservative: anything that doesn't look like a non-retryable
///    4xx is assumed retryable.
///
/// A `tracing::debug!` is emitted on each classification so operators can
/// verify correct behaviour in the logs.
fn is_retryable(error: &anyhow::Error) -> bool {
    // 1. Try the structured path first.
    if let Some(app_err) = error.downcast_ref::<AppError>() {
        let retryable = app_err.is_retryable();
        tracing::debug!(
            retryable,
            error_kind = std::any::type_name::<AppError>(),
            "Retry classification (structured)"
        );
        return retryable;
    }

    // 2. Fallback: heuristic for SDK-originated anyhow errors.
    let msg = error.to_string();
    for token in msg.split_whitespace() {
        if let Some(code_str) = token.strip_prefix('(').and_then(|s| s.strip_suffix(')')) {
            if let Ok(code) = code_str.parse::<u16>() {
                // Only interpret valid HTTP status codes (100–599).
                // Skip arbitrary numbers like (42), (999), (0) that may appear
                // in error messages but are not HTTP status codes.
                if !(100..=599).contains(&code) {
                    continue;
                }
                // Non-retryable: 4xx client errors (except 429 Too Many Requests)
                if (400..429).contains(&code) || (430..500).contains(&code) {
                    tracing::debug!(
                        http_status = code,
                        "Retry classification (heuristic): NOT retryable — client error"
                    );
                    return false;
                }
                // 429 (rate limit) and 5xx are retryable
                tracing::debug!(
                    http_status = code,
                    "Retry classification (heuristic): retryable"
                );
                return true;
            }
        }
    }

    // No status code found in the message — assume retryable (transient network
    // errors often lack an HTTP status in the SDK's error message).
    tracing::debug!(
        "Retry classification (heuristic): retryable — no HTTP status found in message"
    );
    true
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
                    tracing::warn!(
                        attempt = attempt + 1,
                        error = %e,
                        "Operation failed with non-retryable error — aborting retries"
                    );
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

    let last_error = last_error.expect("retry_with_backoff called with max_attempts >= 1");
    tracing::error!(
        max_attempts,
        error = %last_error,
        "All retry attempts exhausted"
    );
    Err(last_error)
}

/// Called when a task is moved to a new column.
/// Starts an agent if the column has one configured.
/// Respects the circuit breaker — if tripped for the project,
/// notifies the user and skips agent start.
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
        task_id,
        to_column.0,
        agent,
        previous_agent
    );
    if let Some(agent) = agent {
        // Check circuit breaker before starting agent
        let project_id = {
            let s = state.lock().unwrap_or_else(|e| e.into_inner());
            s.tasks.get(task_id).map(|t| t.project_id.clone())
        };

        if let Some(ref pid) = project_id {
            let s = state.lock().unwrap_or_else(|e| e.into_inner());
            if s.project_registry
                .is_circuit_breaker_tripped(pid, opencode_config.circuit_breaker_threshold)
            {
                if !s.project_registry.is_circuit_breaker_half_open(
                    pid,
                    opencode_config.circuit_breaker_cooldown_secs,
                ) {
                    // Still in cooldown — skip
                    let failure_count = s
                        .project_registry
                        .circuit_breaker_failures
                        .get(pid)
                        .copied()
                        .unwrap_or(0);
                    drop(s);
                    tracing::warn!(
                        task_id = %task_id,
                        project_id = %pid,
                        consecutive_failures = failure_count,
                        threshold = opencode_config.circuit_breaker_threshold,
                        cooldown_secs = opencode_config.circuit_breaker_cooldown_secs,
                        "Circuit breaker tripped — skipping agent start (cooldown active)"
                    );
                    // Note: tracing layer also captures this warning automatically
                    let mut s = state.lock().unwrap_or_else(|e| e.into_inner());
                    s.set_notification(
                        format!(
                            "Circuit breaker tripped ({} consecutive failures) — auto-progression paused. Press Ctrl+R to retry.",
                            failure_count
                        ),
                        crate::state::types::NotificationVariant::Error,
                        8000,
                    );
                    return;
                }
                // Half-open: allow ONE probe attempt. If it fails,
                // record_agent_failure will keep the breaker tripped.
                // If it succeeds, record_agent_success will reset the breaker.
                drop(s);
                tracing::info!(
                    task_id = %task_id,
                    project_id = %pid,
                    "Circuit breaker half-open — allowing probe attempt"
                );
            }
        }

        start_agent(
            task_id,
            &agent,
            state,
            client,
            opencode_config,
            previous_agent,
            project_id,
        );
    }
}

/// Start an agent for a task.
/// Acquires a concurrency-limited permit before creating a session,
/// preventing too many concurrent OpenCode sessions per project.
#[tracing::instrument(skip(state, client, opencode_config), fields(task_id, agent))]
fn start_agent(
    task_id: &str,
    agent: &str,
    state: &Arc<Mutex<AppState>>,
    client: &OpenCodeClient,
    opencode_config: &OpenCodeConfig,
    previous_agent: Option<String>,
    project_id: Option<String>,
) {
    // Log the full dispatch decision for diagnostics
    {
        let s = state.lock().unwrap_or_else(|e| e.into_inner());
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
    let circuit_breaker_threshold = opencode_config.circuit_breaker_threshold;

    // Check if the agent has a specific model configured
    let agent_model = {
        let s = state.lock().unwrap_or_else(|e| e.into_inner());
        s.tasks
            .get(&task_id)
            .map(|_| {
                opencode_config
                    .agents
                    .get(&agent)
                    .and_then(|a| a.model.clone())
            })
            .flatten()
    };

    // Determine concurrency limit from config
    let max_concurrent = opencode_config.max_concurrent_agents;
    let pid_for_semaphore = project_id.clone().unwrap_or_default();

    // Get the semaphore BEFORE spawning so we can move it into the async block.
    let agent_semaphore = get_agent_semaphore(&pid_for_semaphore, max_concurrent);

    tokio::spawn(async move {
        // Acquire a concurrency-limited permit before starting the agent.
        // This prevents overwhelming the OpenCode server with too many
        // concurrent sessions. The permit is held for the lifetime of
        // the agent session (until this future completes).
        let _permit = match agent_semaphore.acquire().await {
            Ok(permit) => {
                tracing::debug!(
                    task_id = %task_id,
                    agent = %agent,
                    "Acquired agent start permit (max_concurrent={})",
                    max_concurrent
                );
                permit
            }
            Err(e) => {
                tracing::error!(
                    task_id = %task_id,
                    agent = %agent,
                    "Agent semaphore closed: {}",
                    e
                );
                return;
            }
        };

        // Build prompt from task WHILE holding the lock to prevent stale data
        let (prompt, session_id, task_id_clone) = {
            let s = state.lock().unwrap_or_else(|e| e.into_inner());
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
                    )
                    .await
                    {
                        Ok(Ok(true)) => {}
                        Ok(Ok(false)) => {
                            tracing::warn!("Abort returned false for old session {}", old_sid)
                        }
                        Ok(Err(e)) => {
                            tracing::warn!("Failed to abort old session {}: {}", old_sid, e)
                        }
                        Err(_) => {
                            tracing::warn!("Timeout aborting old session {} after 10s", old_sid)
                        }
                    }
                }

                // Clear the old session mapping and create a new one
                {
                    let mut s = state.lock().unwrap_or_else(|e| e.into_inner());
                    s.set_task_session_id(&task_id_clone, None);
                }
                match retry_with_backoff(3, Duration::from_millis(500), || client.create_session())
                    .await
                {
                    Ok(session) => {
                        let sid = session.id.clone();
                        let mut s = state.lock().unwrap_or_else(|e| e.into_inner());
                        s.set_task_session_id(&task_id_clone, Some(sid.clone()));
                        // Clear stale streaming state from the old agent session.
                        // This must happen AFTER set_task_session_id so the finalize
                        // task (if still running) won't race with the clear.
                        // The finalize task reads streaming_text; by the time we
                        // reach here, the synchronous extract_plan_output in
                        // process_event (SessionIdle handler) has already captured
                        // plan_output from streaming_text, so clearing is safe.
                        s.clear_session_data(&task_id_clone);
                        sid
                    }
                    Err(e) => {
                        tracing::error!(
                            task_id = %task_id_clone,
                            agent = %agent,
                            "Failed to create session after retries: {}",
                            e
                        );
                        // Note: tracing layer also captures this error automatically
                        let mut s = state.lock().unwrap_or_else(|e| e.into_inner());
                        s.set_task_error(
                            &task_id_clone,
                            format!("Failed to create session: {}", e),
                        );
                        if let Some(ref pid) = project_id {
                            let tripped = s
                                .project_registry
                                .record_agent_failure(pid, circuit_breaker_threshold);
                            if tripped {
                                s.set_notification(
                                    format!(
                                        "Circuit breaker tripped ({} consecutive failures) — auto-progression paused.",
                                        circuit_breaker_threshold
                                    ),
                                    crate::state::types::NotificationVariant::Error,
                                    8000,
                                );
                            }
                        }
                        return;
                    }
                }
            } else {
                existing_sid.clone()
            }
        } else {
            match retry_with_backoff(3, Duration::from_millis(500), || client.create_session())
                .await
            {
                Ok(session) => {
                    let sid = session.id.clone();
                    // Store session ID and clear any stale streaming data
                    {
                        let mut s = state.lock().unwrap_or_else(|e| e.into_inner());
                        s.set_task_session_id(&task_id_clone, Some(sid.clone()));
                        s.clear_session_data(&task_id_clone);
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
                    // Note: tracing layer also captures this error automatically
                    let mut s = state.lock().unwrap_or_else(|e| e.into_inner());
                    s.set_task_error(&task_id_clone, format!("Failed to create session: {}", e));
                    // Record circuit breaker failure
                    if let Some(ref pid) = project_id {
                        let tripped = s
                            .project_registry
                            .record_agent_failure(pid, circuit_breaker_threshold);
                        if tripped {
                            s.set_notification(
                                format!(
                                    "Circuit breaker tripped ({} consecutive failures) — auto-progression paused.",
                                    circuit_breaker_threshold
                                ),
                                crate::state::types::NotificationVariant::Error,
                                8000,
                            );
                        }
                    }
                    return;
                }
            }
        };

        // Determine model
        let model = agent_model
            .as_deref()
            .map(|m| {
                if m.contains('/') {
                    m.to_string()
                } else {
                    let provider = model_provider.as_deref().unwrap_or("z.ai");
                    format!("{}/{}", provider, m)
                }
            })
            .or_else(|| {
                let provider = model_provider.as_deref().unwrap_or("z.ai");
                Some(format!("{}/{}", provider, model_id))
            });

        // Send prompt (with retry to tolerate transient HTTP errors)
        match retry_with_backoff(3, Duration::from_millis(500), || {
            client.send_prompt(&sid, &prompt, Some(&agent), model.as_deref())
        })
        .await
        {
            Ok(_) => {
                // Record circuit breaker success (reset consecutive failures)
                if let Some(ref pid) = project_id {
                    let mut s = state.lock().unwrap_or_else(|e| e.into_inner());
                    s.project_registry.record_agent_success(pid);
                    // Set active_prompt on the session for the pinned header
                    if let Some(ref sid) = session_id {
                        if let Some(session) = s.session_tracker.task_sessions.get_mut(sid) {
                            session.active_prompt = Some(prompt.clone());
                            session.render_version += 1;
                        }
                    }
                }
            }
            Err(e) => {
                tracing::error!(
                    task_id = %task_id_clone,
                    agent = %agent,
                    session_id = %sid,
                    "Failed to send prompt after retries: {}",
                    e
                );
                // Note: tracing layer also captures this error automatically
                let mut s = state.lock().unwrap_or_else(|e| e.into_inner());
                s.set_task_error(&task_id_clone, format!("Failed to send prompt: {}", e));
                // Record circuit breaker failure
                if let Some(ref pid) = project_id {
                    let tripped = s
                        .project_registry
                        .record_agent_failure(pid, circuit_breaker_threshold);
                    if tripped {
                        s.set_notification(
                            format!(
                                "Circuit breaker tripped ({} consecutive failures) — auto-progression paused.",
                                circuit_breaker_threshold
                            ),
                            crate::state::types::NotificationVariant::Error,
                            8000,
                        );
                    }
                }
            }
        }
    });
}

/// Action returned by `on_agent_completed` for the caller to execute
/// after releasing the MutexGuard.
#[derive(Debug, Clone)]
pub struct AutoProgressAction {
    pub task_id: String,
    pub target_column: KanbanColumn,
    pub agent: String,
}

/// Action returned when an agent completes — either auto-progress to
/// the next column or send a queued follow-up prompt.
#[derive(Debug, Clone)]
pub enum AgentCompletionAction {
    /// Auto-progress the task to the next column and start a new agent.
    AutoProgress(AutoProgressAction),
    /// Send a queued follow-up prompt to the existing session.
    SendQueuedPrompt {
        task_id: String,
        prompt: String,
        session_id: String,
        agent_type: String,
    },
}

/// Send a queued follow-up prompt to an existing agent session.
///
/// This is called when an agent completes and a queued prompt was waiting.
/// Unlike `start_agent`, this reuses the existing session.
pub fn send_follow_up_prompt(
    task_id: &str,
    prompt: &str,
    session_id: &str,
    agent_type: &str,
    state: &Arc<Mutex<AppState>>,
    client: &OpenCodeClient,
    opencode_config: &OpenCodeConfig,
) {
    // Set active_prompt on the session for the pinned header
    {
        let mut s = state.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(session) = s.session_tracker.task_sessions.get_mut(session_id) {
            session.active_prompt = Some(prompt.to_string());
            session.render_version += 1;
        }
        s.update_task_agent_status(task_id, AgentStatus::Running);
    }

    let state = state.clone();
    let client = client.clone();
    let task_id = task_id.to_string();
    let session_id = session_id.to_string();
    let agent = agent_type.to_string();
    let prompt = prompt.to_string();
    let model_id = opencode_config.model.id.clone();
    let model_provider = opencode_config.model.provider.clone();

    // Check if the agent has a specific model configured
    let agent_model = opencode_config
        .agents
        .get(&agent)
        .and_then(|a| a.model.clone());

    tokio::spawn(async move {
        let model = agent_model
            .as_deref()
            .map(|m| {
                if m.contains('/') {
                    m.to_string()
                } else {
                    let provider = model_provider.as_deref().unwrap_or("z.ai");
                    format!("{}/{}", provider, m)
                }
            })
            .or_else(|| {
                let provider = model_provider.as_deref().unwrap_or("z.ai");
                Some(format!("{}/{}", provider, model_id))
            });

        match retry_with_backoff(3, Duration::from_millis(500), || {
            client.send_prompt(&session_id, &prompt, Some(&agent), model.as_deref())
        })
        .await
        {
            Ok(_) => {
                tracing::debug!(
                    "Follow-up prompt sent: task={}, session={}",
                    task_id,
                    session_id
                );
            }
            Err(e) => {
                tracing::error!(
                    task_id = %task_id,
                    session_id = %session_id,
                    "Failed to send follow-up prompt: {}",
                    e
                );
                let mut s = state.lock().unwrap_or_else(|e| e.into_inner());
                s.set_task_error(&task_id, format!("Failed to send follow-up prompt: {}", e));
            }
        }
    });
}

/// Called when an agent completes (from SSE SessionIdle).
/// Checks for queued follow-up prompts first — if one exists, returns
/// a `SendQueuedPrompt` action instead of auto-progressing.
/// Otherwise, auto-progresses if the column configures it and returns
/// an action for the caller to start the target column's agent.
pub fn on_agent_completed(
    task_id: &str,
    state: &mut AppState,
    columns_config: &ColumnsConfig,
) -> Option<AgentCompletionAction> {
    // Check for queued prompt first — takes priority over auto-progression
    let queued = state
        .tasks
        .get(task_id)
        .and_then(|t| t.queued_prompt.clone());

    if let Some(prompt) = queued {
        // Clear the queue
        if let Some(task) = state.tasks.get_mut(task_id) {
            task.queued_prompt = None;
        }

        // Get session info
        let (session_id, agent_type) = state
            .tasks
            .get(task_id)
            .map(|t| (t.session_id.clone(), t.agent_type.clone()))
            .unwrap_or((None, None));

        if let (Some(session_id), Some(agent_type)) = (session_id, agent_type) {
            // Keep the task in its current column — don't auto-progress
            return Some(AgentCompletionAction::SendQueuedPrompt {
                task_id: task_id.to_string(),
                prompt,
                session_id,
                agent_type,
            });
        }
        // No session — fall through to auto-progression
    }

    // Normal auto-progression logic
    let column = state.tasks.get(task_id).map(|t| t.column.clone());
    if let Some(col) = column {
        if let Some(target) = columns_config.auto_progress_for(&col.0) {
            state.move_task(task_id, KanbanColumn(target.clone()));

            // Check if target column has an agent configured
            if let Some(agent) = columns_config.agent_for_column(&target) {
                return Some(AgentCompletionAction::AutoProgress(AutoProgressAction {
                    task_id: task_id.to_string(),
                    target_column: KanbanColumn(target),
                    agent,
                }));
            } else {
                // Target column has no agent — mark task as Complete ("done")
                state.update_task_agent_status(task_id, AgentStatus::Complete);
            }
        } else {
            // No auto-progression configured.
            // Fallback: if the task is in a non-terminal column (not "review" or "done")
            // and a "review" column exists, automatically move it there so completed
            // work doesn't clutter the board. This handles any column where the user's
            // config doesn't explicitly set auto_progress_to.
            if col.0 != "review"
                && col.0 != "done"
                && columns_config
                    .definitions
                    .iter()
                    .any(|c| c.id == "review")
            {
                tracing::info!(
                    task_id = %task_id,
                    from_column = %col.0,
                    "Column has no auto_progress_to configured — falling back to review column"
                );
                state.move_task(task_id, KanbanColumn("review".to_string()));

                if let Some(agent) = columns_config.agent_for_column("review") {
                    return Some(AgentCompletionAction::AutoProgress(AutoProgressAction {
                        task_id: task_id.to_string(),
                        target_column: KanbanColumn("review".to_string()),
                        agent,
                    }));
                } else {
                    // Review column exists but has no agent — mark Complete
                    state.update_task_agent_status(task_id, AgentStatus::Complete);
                    if let Some(task) = state.tasks.get_mut(task_id) {
                        task.review_status = ReviewStatus::AwaitingDecision;
                    }
                }
            } else if col.0 == "review" {
                // If this is the review column, mark the task as awaiting human decision.
                if let Some(task) = state.tasks.get_mut(task_id) {
                    task.review_status = ReviewStatus::AwaitingDecision;
                }
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
            ..Default::default()
        };
        state.add_project(project);
        state.project_registry.active_project_id = Some("proj-1".to_string());

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
            planning_context: None,
            pending_description: None,
            queued_prompt: None,
            pending_permission_count: 0,
            pending_question_count: 0,
            review_status: crate::state::types::ReviewStatus::Pending,
            had_write_operations: false,
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
        match action.unwrap() {
            AgentCompletionAction::AutoProgress(a) => {
                assert_eq!(a.task_id, task_id);
                assert_eq!(a.target_column.0, "running");
                assert_eq!(a.agent, "do");
            }
            AgentCompletionAction::SendQueuedPrompt { .. } => panic!("Expected AutoProgress"),
        }
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
    fn no_auto_progress_when_not_configured_non_running() {
        // A task in a non-running column without auto_progress_to should stay put
        let (mut state, task_id) = make_state_with_task_in_column("custom");

        // Config where "custom" has no auto_progress_to and no review column
        let mut columns_config = ColumnsConfig {
            definitions: vec![ColumnConfig {
                id: "custom".to_string(),
                display_name: None,
                visible: true,
                agent: Some("do".to_string()),
                auto_progress_to: None,
            }],
            visible_ids: Vec::new(),
        };
        columns_config.finalize();

        let action = on_agent_completed(&task_id, &mut state, &columns_config);

        // Task should stay in "custom"
        assert_eq!(state.tasks.get(&task_id).unwrap().column.0, "custom");
        assert!(state
            .kanban
            .columns
            .get("custom")
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
        match action.unwrap() {
            AgentCompletionAction::AutoProgress(a) => assert_eq!(a.agent, "do"),
            AgentCompletionAction::SendQueuedPrompt { .. } => panic!("Expected AutoProgress"),
        }

        // Second completion: running → review
        let action = on_agent_completed(&task_id, &mut state, &columns_config);
        assert_eq!(state.tasks.get(&task_id).unwrap().column.0, "review");
        match action.unwrap() {
            AgentCompletionAction::AutoProgress(a) => assert_eq!(a.agent, "reviewer"),
            AgentCompletionAction::SendQueuedPrompt { .. } => panic!("Expected AutoProgress"),
        }

        // Third completion: review stays (no auto_progress_to)
        let action = on_agent_completed(&task_id, &mut state, &columns_config);
        assert_eq!(state.tasks.get(&task_id).unwrap().column.0, "review");
        assert!(action.is_none());
    }

    #[test]
    fn auto_progress_to_column_without_agent_sets_complete_status() {
        let (mut state, task_id) = make_state_with_task_in_column("running");

        // Config: running → review, but review has NO agent
        let mut columns_config = ColumnsConfig {
            definitions: vec![
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
                    agent: None, // No agent on target
                    auto_progress_to: None,
                },
            ],
            visible_ids: Vec::new(),
        };
        columns_config.finalize();

        // Set status to Complete (simulating what process_session_idle does
        // when it sees has_auto_progress=true — keeps Complete instead of Ready)
        state.update_task_agent_status(&task_id, AgentStatus::Complete);

        let action = on_agent_completed(&task_id, &mut state, &columns_config);

        // Task should be in "review" column
        assert_eq!(state.tasks.get(&task_id).unwrap().column.0, "review");
        // No action returned (target has no agent)
        assert!(action.is_none());
        // Status should be Complete ("done")
        assert_eq!(
            state.tasks.get(&task_id).unwrap().agent_status,
            AgentStatus::Complete
        );
    }

    // ── Running → review fallback ─────────────────────────────────────────

    #[test]
    fn auto_progress_falls_back_to_review_for_running_column() {
        // When a task in "running" has no auto_progress_to configured,
        // it should automatically fall back to moving to "review" if
        // that column exists.
        let (mut state, task_id) = make_state_with_task_in_column("running");

        // Config: "running" has NO auto_progress_to, but "review" column exists
        let mut columns_config = ColumnsConfig {
            definitions: vec![
                ColumnConfig {
                    id: "running".to_string(),
                    display_name: None,
                    visible: true,
                    agent: Some("do".to_string()),
                    auto_progress_to: None, // No explicit auto-progression
                },
                ColumnConfig {
                    id: "review".to_string(),
                    display_name: None,
                    visible: true,
                    agent: Some("reviewer-alpha".to_string()),
                    auto_progress_to: None,
                },
            ],
            visible_ids: Vec::new(),
        };
        columns_config.finalize();

        let action = on_agent_completed(&task_id, &mut state, &columns_config);

        // Task should have moved to "review"
        assert_eq!(state.tasks.get(&task_id).unwrap().column.0, "review");
        assert!(state
            .kanban
            .columns
            .get("review")
            .unwrap()
            .contains(&task_id));
        assert!(!state
            .kanban
            .columns
            .get("running")
            .unwrap()
            .contains(&task_id));
        // Should return an action to start the reviewer agent
        assert!(action.is_some());
        match action.unwrap() {
            AgentCompletionAction::AutoProgress(a) => {
                assert_eq!(a.task_id, task_id);
                assert_eq!(a.target_column.0, "review");
                assert_eq!(a.agent, "reviewer-alpha");
            }
            AgentCompletionAction::SendQueuedPrompt { .. } => panic!("Expected AutoProgress"),
        }
    }

    #[test]
    fn auto_progress_no_fallback_when_explicitly_configured() {
        // When auto_progress_to IS explicitly configured for "running",
        // the fallback should NOT apply — explicit config takes priority.
        let (mut state, task_id) = make_state_with_task_in_column("running");

        // Config: "running" explicitly auto-progresses to "custom-target"
        let mut columns_config = ColumnsConfig {
            definitions: vec![
                ColumnConfig {
                    id: "running".to_string(),
                    display_name: None,
                    visible: true,
                    agent: Some("do".to_string()),
                    auto_progress_to: Some("custom-target".to_string()), // Explicit
                },
                ColumnConfig {
                    id: "review".to_string(),
                    display_name: None,
                    visible: true,
                    agent: Some("reviewer-alpha".to_string()),
                    auto_progress_to: None,
                },
                ColumnConfig {
                    id: "custom-target".to_string(),
                    display_name: None,
                    visible: true,
                    agent: Some("custom-agent".to_string()),
                    auto_progress_to: None,
                },
            ],
            visible_ids: Vec::new(),
        };
        columns_config.finalize();

        let action = on_agent_completed(&task_id, &mut state, &columns_config);

        // Task should move to "custom-target" (explicit config), NOT "review"
        assert_eq!(
            state.tasks.get(&task_id).unwrap().column.0,
            "custom-target"
        );
        match action.unwrap() {
            AgentCompletionAction::AutoProgress(a) => {
                assert_eq!(a.agent, "custom-agent");
            }
            AgentCompletionAction::SendQueuedPrompt { .. } => panic!("Expected AutoProgress"),
        }
    }

    #[test]
    fn auto_progress_falls_back_to_review_for_custom_column() {
        // When a task in a custom (non-running) column has no auto_progress_to
        // configured, it should automatically fall back to moving to "review"
        // if that column exists.
        let (mut state, task_id) = make_state_with_task_in_column("custom");

        // Config: "custom" has NO auto_progress_to, but "review" column exists
        let mut columns_config = ColumnsConfig {
            definitions: vec![
                ColumnConfig {
                    id: "custom".to_string(),
                    display_name: None,
                    visible: true,
                    agent: Some("do".to_string()),
                    auto_progress_to: None, // No explicit auto-progression
                },
                ColumnConfig {
                    id: "review".to_string(),
                    display_name: None,
                    visible: true,
                    agent: Some("reviewer-alpha".to_string()),
                    auto_progress_to: None,
                },
            ],
            visible_ids: Vec::new(),
        };
        columns_config.finalize();

        let action = on_agent_completed(&task_id, &mut state, &columns_config);

        // Task should have moved to "review"
        assert_eq!(state.tasks.get(&task_id).unwrap().column.0, "review");
        assert!(state
            .kanban
            .columns
            .get("review")
            .unwrap()
            .contains(&task_id));
        assert!(!state
            .kanban
            .columns
            .get("custom")
            .unwrap()
            .contains(&task_id));
        // Should return an action to start the reviewer agent
        assert!(action.is_some());
        match action.unwrap() {
            AgentCompletionAction::AutoProgress(a) => {
                assert_eq!(a.task_id, task_id);
                assert_eq!(a.target_column.0, "review");
                assert_eq!(a.agent, "reviewer-alpha");
            }
            AgentCompletionAction::SendQueuedPrompt { .. } => panic!("Expected AutoProgress"),
        }
    }

    #[test]
    fn auto_progress_no_fallback_for_done_column() {
        // When a task in "done" column completes, it should stay in "done"
        // and NOT be moved to "review" (preventing infinite loops).
        let (mut state, task_id) = make_state_with_task_in_column("done");

        // Config: "done" and "review" columns exist
        let mut columns_config = ColumnsConfig {
            definitions: vec![
                ColumnConfig {
                    id: "done".to_string(),
                    display_name: None,
                    visible: true,
                    agent: None,
                    auto_progress_to: None,
                },
                ColumnConfig {
                    id: "review".to_string(),
                    display_name: None,
                    visible: true,
                    agent: Some("reviewer-alpha".to_string()),
                    auto_progress_to: None,
                },
            ],
            visible_ids: Vec::new(),
        };
        columns_config.finalize();

        let action = on_agent_completed(&task_id, &mut state, &columns_config);

        // Task should stay in "done"
        assert_eq!(state.tasks.get(&task_id).unwrap().column.0, "done");
        assert!(state
            .kanban
            .columns
            .get("done")
            .unwrap()
            .contains(&task_id));
        // No action returned
        assert!(action.is_none());
    }

    #[test]
    fn auto_progress_no_fallback_when_review_missing() {
        // When "running" has no auto_progress_to AND there is no "review"
        // column, the task should stay in "running" (no crash).
        let (mut state, task_id) = make_state_with_task_in_column("running");

        // Config: "running" with no auto_progress_to, no "review" column at all
        let mut columns_config = ColumnsConfig {
            definitions: vec![ColumnConfig {
                id: "running".to_string(),
                display_name: None,
                visible: true,
                agent: Some("do".to_string()),
                auto_progress_to: None,
            }],
            visible_ids: Vec::new(),
        };
        columns_config.finalize();

        let action = on_agent_completed(&task_id, &mut state, &columns_config);

        // Task should stay in "running" — no review column to fall back to
        assert_eq!(state.tasks.get(&task_id).unwrap().column.0, "running");
        assert!(state
            .kanban
            .columns
            .get("running")
            .unwrap()
            .contains(&task_id));
        // No action
        assert!(action.is_none());
    }

    #[test]
    fn auto_progress_fallback_to_review_without_agent() {
        // When "running" falls back to "review" but "review" has no agent,
        // the task should move to "review" with Complete status and
        // AwaitingDecision review status.
        let (mut state, task_id) = make_state_with_task_in_column("running");

        let mut columns_config = ColumnsConfig {
            definitions: vec![
                ColumnConfig {
                    id: "running".to_string(),
                    display_name: None,
                    visible: true,
                    agent: Some("do".to_string()),
                    auto_progress_to: None,
                },
                ColumnConfig {
                    id: "review".to_string(),
                    display_name: None,
                    visible: true,
                    agent: None, // No agent on review
                    auto_progress_to: None,
                },
            ],
            visible_ids: Vec::new(),
        };
        columns_config.finalize();

        let action = on_agent_completed(&task_id, &mut state, &columns_config);

        // Task should be in "review"
        assert_eq!(state.tasks.get(&task_id).unwrap().column.0, "review");
        // No action (review has no agent)
        assert!(action.is_none());
        // Status should be Complete
        assert_eq!(
            state.tasks.get(&task_id).unwrap().agent_status,
            AgentStatus::Complete
        );
        // Review status should be AwaitingDecision
        assert_eq!(
            state.tasks.get(&task_id).unwrap().review_status,
            ReviewStatus::AwaitingDecision
        );
    }

    // ── Review status on agent completion ────────────────────────────────

    #[test]
    fn review_agent_completion_sets_awaiting_decision() {
        let (mut state, task_id) = make_state_with_task_in_column("review");

        // Config: review column has an agent but no auto-progression
        let mut columns_config = ColumnsConfig {
            definitions: vec![
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
                    agent: Some("reviewer-alpha".to_string()),
                    auto_progress_to: None, // Terminal column — no auto-progress
                },
            ],
            visible_ids: Vec::new(),
        };
        columns_config.finalize();

        let action = on_agent_completed(&task_id, &mut state, &columns_config);

        // Task should stay in "review" column
        assert_eq!(state.tasks.get(&task_id).unwrap().column.0, "review");
        // No auto-progress action
        assert!(action.is_none());
        // Review status should be AwaitingDecision
        assert_eq!(
            state.tasks.get(&task_id).unwrap().review_status,
            ReviewStatus::AwaitingDecision
        );
    }

    #[test]
    fn non_review_terminal_column_does_not_set_awaiting_decision() {
        let (mut state, task_id) = make_state_with_task_in_column("running");

        // Config: running has agent but no auto-progression
        let mut columns_config = ColumnsConfig {
            definitions: vec![ColumnConfig {
                id: "running".to_string(),
                display_name: None,
                visible: true,
                agent: Some("do".to_string()),
                auto_progress_to: None,
            }],
            visible_ids: Vec::new(),
        };
        columns_config.finalize();

        let action = on_agent_completed(&task_id, &mut state, &columns_config);

        // Task should stay in "running" column
        assert_eq!(state.tasks.get(&task_id).unwrap().column.0, "running");
        // No action
        assert!(action.is_none());
        // Review status should still be Pending (not in review column)
        assert_eq!(
            state.tasks.get(&task_id).unwrap().review_status,
            ReviewStatus::Pending
        );
    }

    // ── Retry heuristic ───────────────────────────────────────────────────

    #[test]
    fn retry_heuristic_skips_non_http_codes() {
        // Error messages containing numbers that are not valid HTTP status codes
        // should be treated as retryable (fallback to true), not misclassified.
        let err = anyhow::anyhow!("connection refused on port (42)");
        assert!(is_retryable(&err), "Non-HTTP code (42) should be retryable");

        let err = anyhow::anyhow!("timeout after (999) attempts");
        assert!(
            is_retryable(&err),
            "Non-HTTP code (999) should be retryable"
        );

        let err = anyhow::anyhow!("error code (0)");
        assert!(is_retryable(&err), "Non-HTTP code (0) should be retryable");

        // Valid HTTP codes should still work
        let err = anyhow::anyhow!("request failed (500)");
        assert!(is_retryable(&err), "HTTP 500 should be retryable");

        let err = anyhow::anyhow!("bad request (400)");
        assert!(!is_retryable(&err), "HTTP 400 should NOT be retryable");

        let err = anyhow::anyhow!("rate limited (429)");
        assert!(is_retryable(&err), "HTTP 429 should be retryable");
    }
}
