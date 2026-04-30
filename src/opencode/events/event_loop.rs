//! SSE event loop with reconnection logic.

use futures::StreamExt;
use std::sync::{Arc, Mutex};

use crate::config::types::{ColumnsConfig, OpenCodeConfig};
use crate::opencode::client::OpenCodeClient;
use crate::opencode::sse::SseStreamError;
use crate::orchestration::engine::{on_task_moved, AgentCompletionAction, AutoProgressAction};
use crate::state::types::{AgentStatus, AppState};

use super::DEFAULT_SSE_MAX_RETRIES;

/// Internal wrapper to unify auto-progress and queued-prompt actions
/// for the event loop's deferred execution block.
enum DeferredCompletionAction {
    /// Auto-progress: move task to new column and start a new agent.
    AutoProgress {
        action: AutoProgressAction,
        previous_agent: Option<String>,
    },
    /// Send a queued follow-up prompt to the existing session.
    SendQueuedPrompt {
        task_id: String,
        prompt: String,
        session_id: String,
        agent_type: String,
    },
}

/// Run the SSE event loop for an OpenCode server shared by one or more projects.
/// This is spawned as a tokio task per unique server URL.
///
/// The `shutdown` receiver is watched so the loop can exit cleanly when the
/// app is shutting down, instead of relying solely on task cancellation via
/// `abort()`.
///
/// `project_ids` identifies all projects sharing this server URL. Connection
/// state changes (connected, reconnecting, permanently_disconnected) are
/// propagated to every project in the list so that the status bar stays
/// consistent across multi-project setups.
pub async fn sse_event_loop(
    client: OpenCodeClient,
    state: Arc<Mutex<AppState>>,
    columns_config: ColumnsConfig,
    opencode_config: OpenCodeConfig,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
    project_ids: Vec<String>,
) {
    let base_backoff_ms: u64 = 2000;
    let mut backoff_power: u32 = 0; // 2^backoff_power * base_backoff_ms + jitter
    let mut reconnect_attempt: u32 = 0;

    // Add per-project jitter to avoid thundering herd when a shared server goes
    // down.  A simple deterministic hash of the first project ID produces a
    // stable 0–500 ms offset so different server groups spread their reconnect
    // attempts evenly without adding a random dependency.
    let jitter: u64 = (project_ids
        .first()
        .map(|pid| pid.bytes())
        .unwrap_or_else(|| "".bytes())
        .fold(0u64, |acc, b| acc.wrapping_mul(31).wrapping_add(b as u64)))
        % 501;

    // Effective max retries: use config value, but treat 0 as "use default"
    // to avoid accidentally retrying forever.
    let max_retries = if opencode_config.sse_max_retries == 0 {
        DEFAULT_SSE_MAX_RETRIES
    } else {
        opencode_config.sse_max_retries
    };

    loop {
        // Check shutdown before each connection attempt.
        if *shutdown.borrow() {
            return;
        }

        match client.subscribe_to_events().await {
            Ok(stream) => {
                tracing::debug!("SSE stream established (server: {})", client.base_url());
                // Snapshot reconnect count before resetting — used below for
                // recovery diagnostics.
                let was_reconnecting = reconnect_attempt > 0;
                backoff_power = 0; // Reset backoff on successful connection
                reconnect_attempt = 0; // Reset consecutive failure counter on success
                let mut stream = stream;
                let mut first_event_received = false;

                loop {
                    tokio::select! {
                        event_result = stream.next() => {
                            let Some(event_result) = event_result else {
                                // Stream closed by the server — break to reconnect.
                                // Don't set reconnecting here; the outer loop's
                                // grace period will handle it.
                                tracing::debug!(
                                    "SSE stream closed by server (clean close, will reconnect)"
                                );
                                break;
                            };

                            let event = match event_result {
                                Ok(e) => e,
                                Err(e) => {
                                    match &e {
                                        SseStreamError::Json(json_err) => {
                                            let msg = json_err.to_string();
                                            if msg.contains("unknown variant") || msg.contains("missing field") {
                                                // Unknown event type or structurally expected field missing from
                                                // the server payload (e.g. FileDiff.before for new files).
                                                // The stream is still healthy — skip silently.
                                            } else {
                                                tracing::debug!("Skipping malformed SSE event: {}", msg);
                                            }
                                        }
                                        SseStreamError::Connection(msg) => {
                                            // Connection error — stream is dead, break to reconnect.
                                            // Don't set reconnecting here; the outer loop's
                                            // grace period will handle it.
                                            tracing::debug!(
                                                "SSE connection error (will reconnect): {}",
                                                msg
                                            );
                                            break;
                                        }
                                    }
                                    continue;
                                }
                            };

                            // Mark connected only after the first successful event —
                            // avoids a brief "connected" flash on short-lived streams
                            // that return 200 but close before delivering data.
                            if !first_event_received {
                                first_event_received = true;
                                if was_reconnecting {
                                    tracing::debug!(
                                        "SSE reconnected successfully after prior failures; \
                                         events missed during disconnection may cause stale state"
                                    );
                                }
                                {
                                    let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
                                    for pid in &project_ids {
                                        state.set_project_connected(pid, true);
                                    }
                                    if was_reconnecting {
                                        state.set_notification(
                                            "Connection restored".to_string(),
                                            crate::state::types::NotificationVariant::Success,
                                            3000,
                                        );
                                    }
                                }
                            }

                            let (action, finalize_session_id, finalize_task_id) = {
                                let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
                                let (action, finalize_session_id) =
                                    super::process_event(&event, &mut state, &client, &columns_config);
                                // Handle the completion action: set Running status for
                                // auto-progress, or prepare queued follow-up prompts.
                                let (deferred, finalize_task_id) = match action {
                                    Some(AgentCompletionAction::AutoProgress(a)) => {
                                        let previous_agent = state.tasks.get(&a.task_id)
                                            .and_then(|t| t.agent_type.clone());
                                        // Clear old session mapping immediately so stale SessionStatus
                                        // events for the old session can't find this task and overwrite
                                        // the Running status we're about to set.
                                        let old_session_id = state.tasks.get(&a.task_id)
                                            .and_then(|t| t.session_id.clone());
                                        if let Some(old_sid) = old_session_id {
                                            state.session_tracker.session_to_task.remove(&old_sid);
                                            // Keep the session_id on the task for now so start_agent()
                                            // can detect the agent change and create a fresh session.
                                            // The mapping is what matters for event routing.
                                        }
                                        // Only set Running if task is not already Ready (e.g., planning→running
                                        // should preserve Ready status until the do agent actually starts).
                                        let current_status = state.tasks.get(&a.task_id)
                                            .map(|t| t.agent_status.clone());
                                        if !matches!(current_status, Some(AgentStatus::Ready)) {
                                            state.update_task_agent_status(&a.task_id, AgentStatus::Running);
                                        }
                                        state.set_task_agent_type(&a.task_id, Some(a.agent.clone()));
                                        // Reset write tracking for the new agent session
                                        if let Some(task) = state.tasks.get_mut(&a.task_id) {
                                            task.had_write_operations = false;
                                        }
                                        // Return the task_id for finalization since we just broke the
                                        // session→task lookup that the finalize logic depends on.
                                        let finalize_task_id = Some(a.task_id.clone());
                                        (
                                            Some(DeferredCompletionAction::AutoProgress {
                                                action: a,
                                                previous_agent,
                                            }),
                                            finalize_task_id,
                                        )
                                    }
                                    Some(AgentCompletionAction::SendQueuedPrompt {
                                        task_id,
                                        prompt,
                                        session_id,
                                        agent_type,
                                    }) => {
                                        // Send the queued follow-up prompt — no column move needed
                                        let finalize_task_id = Some(task_id.clone());
                                        (
                                            Some(DeferredCompletionAction::SendQueuedPrompt {
                                                task_id,
                                                prompt,
                                                session_id,
                                                agent_type,
                                            }),
                                            finalize_task_id,
                                        )
                                    }
                                    None => (None, None),
                                };
                                (deferred, finalize_session_id, finalize_task_id)
                            };

                            // Execute the completion action after the MutexGuard is dropped.
                            // This must happen after dropping the lock to avoid deadlock.
                            if let Some(action) = action {
                                match action {
                                    DeferredCompletionAction::AutoProgress {
                                        action: a,
                                        previous_agent,
                                    } => {
                                        on_task_moved(
                                            &a.task_id,
                                            &a.target_column,
                                            &state,
                                            &client,
                                            &columns_config,
                                            &opencode_config,
                                            previous_agent,
                                        );
                                    }
                                    DeferredCompletionAction::SendQueuedPrompt {
                                        task_id,
                                        prompt,
                                        session_id,
                                        agent_type,
                                    } => {
                                        crate::orchestration::engine::send_follow_up_prompt(
                                            &task_id,
                                            &prompt,
                                            &session_id,
                                            &agent_type,
                                            &state,
                                            &client,
                                            &opencode_config,
                                        );
                                    }
                                }
                            }

                            // Finalize streaming text into persistent message history
                            // when a session completes or goes idle.
                            if let Some(session_id) = finalize_session_id {
                                // Look up the task_id while we can, but the actual
                                // fetch must happen after the lock is released.
                                // Use finalize_task_id if available (auto-progression may
                                // have cleared the session→task mapping to prevent stale events).
                                let task_id = finalize_task_id.or_else(|| {
                                    let s = state.lock().unwrap_or_else(|e| e.into_inner());
                                    s.get_task_id_by_session(&session_id)
                                        .map(|tid| tid.to_string())
                                });
                                if let Some(task_id) = task_id {
                                    let client_clone = client.clone();
                                    let state_clone = state.clone();
                                    tokio::spawn(async move {
                                        // Check if there's streaming text to finalize
                                        let needs_finalize = {
                                            let s = state_clone.lock().unwrap_or_else(|e| e.into_inner());
                                            s.session_tracker.task_sessions.get(&task_id)
                                                .is_some_and(|ts| ts.streaming_text.is_some())
                                        };
                                        if !needs_finalize {
                                            tracing::debug!(
                                                task_id = %task_id,
                                                session_id = %session_id,
                                                "Skipping finalization — streaming_text already cleared (plan captured from streaming buffer)"
                                            );
                                            return;
                                        }

                                        match client_clone.fetch_session_messages(&session_id).await {
                                            Ok(messages) => {
                                                let mut s = state_clone.lock().unwrap_or_else(|e| e.into_inner());
                                                s.finalize_session_streaming(&task_id, messages);
                                            }
                                            Err(e) => {
                                                tracing::debug!("Failed to fetch session messages for finalization (streaming text preserved): {}", e);
                                                // Don't clear streaming_text — it's the only copy of the agent's output.
                                                // It will be cleaned up when a new session starts.
                                            }
                                        }
                                    });
                                }
                            }
                        }
                        result = shutdown.changed() => {
                            if result.is_err() || *shutdown.borrow() {
                                return;
                            }
                        }
                    }
                }
            }
            Err(e) => {
                // Initial connection failure — don't set reconnecting here.
                // The outer loop's grace period will handle it.
                tracing::debug!(
                    "SSE connection failed (attempt {}): {}",
                    reconnect_attempt + 1,
                    e
                );
            }
        }

        // Check if we've exceeded the max retry limit.
        if reconnect_attempt >= max_retries {
            tracing::debug!(
                "SSE max retries reached ({}), entering slow-retry mode (projects: {:?})",
                max_retries,
                project_ids
            );
            {
                let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
                for pid in &project_ids {
                    state.set_project_permanently_disconnected(pid);
                }
                state.mark_render_dirty();
            }
            // Enter slow-retry mode instead of giving up permanently.
            // The permanently_disconnected state is still set (red indicator)
            // but the loop keeps trying at a very slow rate so the app
            // recovers automatically when the server comes back online.
            tokio::select! {
                _ = tokio::time::sleep(std::time::Duration::from_secs(60)) => {
                    tracing::debug!(
                        "SSE slow-retry: resetting after permanent disconnect cooldown"
                    );
                    reconnect_attempt = 0;
                    backoff_power = 0;
                    continue;
                }
                _ = shutdown.changed() => {
                    if *shutdown.borrow() {
                        return;
                    }
                }
            }
        }

        // Exponential backoff with max 30s, but also break on shutdown.
        reconnect_attempt += 1;

        // Compute fresh backoff: base * 2^power + jitter. Jitter is always
        // 0–500 ms regardless of retry count, avoiding accumulation from
        // exponential doubling.
        let backoff_ms = (base_backoff_ms * 2u64.pow(backoff_power)).min(30_000) + jitter;

        // Grace period: sleep before updating the reconnecting indicator.
        // This prevents a yellow "reconnecting" flash for transient connection
        // blips (e.g., a single read-timeout that reconnects quickly). The
        // user continues to see "connected" (green) during this window. If
        // reconnection succeeds on the next loop iteration, the reconnecting
        // flag is never set and the yellow indicator never appears.
        tokio::select! {
            _ = tokio::time::sleep(std::time::Duration::from_millis(backoff_ms)) => {}
            _ = shutdown.changed() => {
                if *shutdown.borrow() {
                    return;
                }
            }
        }

        // Update reconnecting state after grace period has elapsed.
        // Propagate to all projects sharing this server URL.
        {
            let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
            for pid in &project_ids {
                state.set_project_reconnecting(pid, true);
                state.set_project_reconnect_attempt(pid, reconnect_attempt);
            }
            if reconnect_attempt == 1 {
                state.set_notification(
                    "SSE connection lost — reconnecting...".to_string(),
                    crate::state::types::NotificationVariant::Warning,
                    4000,
                );
            }
            state.mark_render_dirty();
        }
        tracing::debug!(
            "SSE reconnecting (attempt {}, backoff {}ms, projects: {:?})",
            reconnect_attempt,
            backoff_ms,
            project_ids
        );
        backoff_power += 1;
    }
}
