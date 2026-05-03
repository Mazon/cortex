//! Project CRUD handlers — create, rename, delete, switch, set working directory.

use super::super::App;

/// Quit the application.
pub fn handle_quit(app: &mut App) {
    app.should_quit = true;
}

/// Toggle the help overlay.
pub fn handle_help_toggle(app: &mut App) {
    let mut state = app.state.lock().unwrap_or_else(|e| e.into_inner());
    state.ui.mode = crate::state::types::AppMode::Help;
}

/// Switch to the previous project.
pub fn handle_prev_project(app: &mut App) {
    switch_project_offset(app, -1);
}

/// Switch to the next project.
pub fn handle_next_project(app: &mut App) {
    switch_project_offset(app, 1);
}

/// Open the new project directory prompt.
pub fn handle_new_project(app: &mut App) {
    let mut state = app.state.lock().unwrap_or_else(|e| e.into_inner());
    state.open_new_project_directory();
}

/// Open the project rename prompt.
pub fn handle_rename_project(app: &mut App) {
    let mut state = app.state.lock().unwrap_or_else(|e| e.into_inner());
    state.open_project_rename();
}

/// Open the set working directory prompt.
pub fn handle_set_working_directory(app: &mut App) {
    let mut state = app.state.lock().unwrap_or_else(|e| e.into_inner());
    state.open_set_working_directory();
}

/// Delete the active project — abort all sessions, remove from registry.
pub fn handle_delete_project(app: &mut App) {
    // Collect project ID, name, and session IDs while holding the lock.
    let (project_id, project_name, sessions_to_abort) = {
        let state = app.state.lock().unwrap_or_else(|e| e.into_inner());
        match state.project_registry.active_project_id.as_ref() {
            Some(pid) => {
                let name = state
                    .project_registry
                    .projects
                    .iter()
                    .find(|p| &p.id == pid)
                    .map(|p| p.name.clone())
                    .unwrap_or_else(|| pid.clone());
                // Gather all session IDs for this project's tasks
                let session_ids: Vec<String> = state
                    .tasks
                    .values()
                    .filter(|t| t.project_id == *pid)
                    .filter_map(|t| t.session_id.clone())
                    .collect();
                (Some(pid.clone()), name, session_ids)
            }
            None => (None, String::new(), Vec::new()),
        }
    };

    let Some(project_id) = project_id else {
        let mut state = app.state.lock().unwrap_or_else(|e| e.into_inner());
        state.set_notification(
            "No active project to delete".to_string(),
            crate::state::types::NotificationVariant::Info,
            2000,
        );
        return;
    };

    // Abort all active sessions asynchronously using the client
    // (which we still have at this point).
    if let Some(client) = app.opencode_clients.get(&project_id).cloned() {
        tokio::spawn(async move {
            for sid in &sessions_to_abort {
                if let Err(_e) = client.abort_session(sid).await {}
            }
        });
    }

    // Now safe to remove the client
    app.opencode_clients.remove(&project_id);

    let mut state = app.state.lock().unwrap_or_else(|e| e.into_inner());
    state.remove_project(&project_id);

    // If there are remaining projects, select the first one.
    if let Some(id) = state
        .project_registry
        .projects
        .first()
        .map(|p| p.id.clone())
    {
        state.select_project(&id);
    }

    state.set_notification(
        format!("Project \"{}\" deleted", project_name),
        crate::state::types::NotificationVariant::Info,
        3000,
    );

    // If the user just deleted the last project, show a prominent notification.
    if state.project_registry.projects.is_empty() {
        state.set_notification(
            "All projects deleted. Press Ctrl+N to create a new one.".to_string(),
            crate::state::types::NotificationVariant::Info,
            10000,
        );
    }
}

/// Switch to the previous/next project by an offset (-1 or +1).
/// Wraps around at the boundaries.
fn switch_project_offset(app: &mut App, direction: i32) {
    let mut state = app.state.lock().unwrap_or_else(|e| e.into_inner());
    let len = state.project_registry.projects.len();
    if len <= 1 {
        return;
    }
    let current_idx = state
        .project_registry
        .active_project_id
        .as_ref()
        .and_then(|id| {
            state
                .project_registry
                .projects
                .iter()
                .position(|p| &p.id == id)
        })
        .unwrap_or(0);
    let new_idx = (current_idx as i32 + direction).rem_euclid(len as i32) as usize;
    let new_id = state.project_registry.projects[new_idx].id.clone();
    state.select_project(&new_id);
}
