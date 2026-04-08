//! Orchestration engine — config-driven task progression rules.

use std::sync::{Arc, Mutex};

use crate::config::types::{ColumnsConfig, OpenCodeConfig};
use crate::opencode::client::OpenCodeClient;
use crate::state::types::{AgentStatus, AppState, KanbanColumn, TaskAgentType};

/// Called when a task is moved to a new column.
/// Starts an agent if the column has one configured.
pub fn on_task_moved(
    task_id: &str,
    to_column: &KanbanColumn,
    state: &Arc<Mutex<AppState>>,
    client: &OpenCodeClient,
    columns_config: &ColumnsConfig,
    opencode_config: &OpenCodeConfig,
) {
    if let Some(agent) = columns_config.agent_for_column(&to_column.0) {
        start_agent(task_id, &agent, state, client, opencode_config);
    }
}

/// Start an agent for a task.
fn start_agent(
    task_id: &str,
    agent: &str,
    state: &Arc<Mutex<AppState>>,
    client: &OpenCodeClient,
    opencode_config: &OpenCodeConfig,
) {
    // Update status immediately for UI feedback
    {
        let mut s = state.lock().unwrap();
        s.update_task_agent_status(task_id, AgentStatus::Running);
    }

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
        // Build prompt from task
        let (prompt, session_id, task_id_clone) = {
            let mut s = state.lock().unwrap();
            let task = match s.tasks.get(&task_id) {
                Some(t) => t.clone(),
                None => {
                    tracing::warn!("Task {} not found for agent start", task_id);
                    return;
                }
            };

            let prompt = OpenCodeClient::build_prompt_for_agent(&task, &agent, None);
            (prompt, task.session_id.clone(), task.id.clone())
        };

        // Create session if needed
        let sid = if let Some(ref existing_sid) = session_id {
            existing_sid.clone()
        } else {
            match client.create_session().await {
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
                    tracing::error!("Failed to create session: {}", e);
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

        // Send prompt
        match client
            .send_prompt(&sid, &prompt, Some(&agent), model.as_deref())
            .await
        {
            Ok(_) => {
                tracing::info!("Prompt sent to agent '{}' for task {}", agent, task_id_clone);
            }
            Err(e) => {
                tracing::error!("Failed to send prompt: {}", e);
                let mut s = state.lock().unwrap();
                s.set_task_error(
                    &task_id_clone,
                    format!("Failed to send prompt: {}", e),
                );
            }
        }
    });
}

/// Called when an agent completes (from SSE SessionIdle).
/// Auto-progresses if the column configures it.
pub fn on_agent_completed(
    task_id: &str,
    state: &mut AppState,
    columns_config: &ColumnsConfig,
) {
    let column = state
        .tasks
        .get(task_id)
        .map(|t| t.column.clone());
    if let Some(col) = column {
        if let Some(target) = columns_config.auto_progress_for(&col.0) {
            tracing::info!(
                "Auto-progressing task {} from {} to {}",
                task_id,
                col.0,
                target
            );
            state.move_task(task_id, KanbanColumn(target));
        }
    }
}
