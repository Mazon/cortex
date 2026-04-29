//! Project-related types for the Cortex application.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use super::enums::ProjectStatus;

/// A project in the sidebar.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CortexProject {
    /// Unique identifier (UUID v4).
    pub id: String,
    /// Display name shown in the sidebar.
    pub name: String,
    /// Filesystem working directory for the project.
    pub working_directory: String,
    /// Aggregate status derived from task states.
    pub status: ProjectStatus,
    /// Display order position in the sidebar.
    pub position: usize,
    /// Whether the OpenCode server for this project is connected.
    /// Runtime-only — not persisted to the database.
    #[serde(skip)]
    pub connected: bool,
    /// Whether an SSE reconnection is in progress for this project.
    /// Runtime-only — not persisted to the database.
    #[serde(skip)]
    pub reconnecting: bool,
    /// Current reconnection attempt number for this project (0 when not reconnecting).
    /// Runtime-only — not persisted to the database.
    #[serde(skip)]
    pub reconnect_attempt: u32,
    /// Whether max reconnection retries have been exceeded for this project.
    /// This is a runtime-only flag (not persisted) — on app restart the
    /// connection will be retried from scratch.
    #[serde(skip)]
    pub permanently_disconnected: bool,
}

impl CortexProject {
    /// Validate that the working directory is an absolute path, exists, and is
    /// a directory. Returns `Ok(())` on success or an error message on failure.
    pub fn validate_working_directory(&self) -> Result<(), String> {
        let path = std::path::Path::new(&self.working_directory);
        if !path.is_absolute() {
            return Err(format!(
                "Working directory must be an absolute path: {}",
                self.working_directory
            ));
        }
        // Canonicalize to resolve any path traversal components (e.g., `../`)
        match path.canonicalize() {
            Ok(canonical) if canonical.is_dir() => Ok(()),
            Ok(_) => Err(format!(
                "Working directory is not a directory: {}",
                self.working_directory
            )),
            Err(e) => Err(format!(
                "Working directory does not exist: {} ({})",
                self.working_directory, e
            )),
        }
    }
}

impl Default for CortexProject {
    fn default() -> Self {
        Self {
            id: String::new(),
            name: String::new(),
            working_directory: String::new(),
            status: ProjectStatus::Idle,
            position: 0,
            connected: false,
            reconnecting: false,
            reconnect_attempt: 0,
            permanently_disconnected: false,
        }
    }
}

/// Kanban board state — column ordering and task placement.
#[derive(Debug, Clone, Default)]
pub struct KanbanState {
    /// Maps column ID → ordered list of task IDs.
    pub columns: HashMap<String, Vec<String>>,
    /// Currently focused column index among visible columns.
    pub focused_column_index: usize,
    /// Per-column focused task index.
    pub focused_task_index: HashMap<String, usize>,
    /// Horizontal scroll offset — index of the first visible column.
    /// 0 means no scrolling (leftmost column is visible).
    pub kanban_scroll_offset: usize,
}

/// Project registry — manages projects, active project, and task counters.
#[derive(Debug, Clone, Default)]
pub struct ProjectRegistry {
    /// All registered projects.
    pub projects: Vec<CortexProject>,
    /// ID of the currently active project.
    pub active_project_id: Option<String>,
    /// Per-project auto-incrementing task number counters.
    pub task_number_counters: HashMap<String, u32>,
    /// Per-project consecutive agent start failure count.
    /// When this reaches the circuit breaker threshold, auto-progression
    /// is paused for that project.
    pub circuit_breaker_failures: HashMap<String, u32>,
    /// Per-project timestamp (epoch seconds) of when the circuit breaker tripped.
    /// Used for half-open auto-recovery cooldown.
    pub circuit_breaker_tripped_at: HashMap<String, i64>,
}

impl ProjectRegistry {
    /// Get the active project, if one is set and exists.
    pub fn active_project(&self) -> Option<&CortexProject> {
        self.active_project_id
            .as_ref()
            .and_then(|pid| self.projects.iter().find(|p| &p.id == pid))
    }

    /// Get connection state for the active project.
    /// Returns defaults (disconnected, not reconnecting) if no project is active.
    pub fn connection_state(&self) -> (bool, bool, u32, bool) {
        self.active_project()
            .map(|p| {
                (
                    p.connected,
                    p.reconnecting,
                    p.reconnect_attempt,
                    p.permanently_disconnected,
                )
            })
            .unwrap_or((false, false, 0, false))
    }

    /// Whether the active project's server is connected.
    pub fn is_connected(&self) -> bool {
        self.connection_state().0
    }

    /// Whether the active project's server is reconnecting.
    pub fn is_reconnecting(&self) -> bool {
        self.connection_state().1
    }

    /// Reconnection attempt number for the active project.
    pub fn reconnect_attempt(&self) -> u32 {
        self.connection_state().2
    }

    /// Whether the active project has permanently disconnected.
    pub fn is_permanently_disconnected(&self) -> bool {
        self.connection_state().3
    }

    /// Set a project's connection state. No-op if the project doesn't exist.
    pub fn set_project_connected(&mut self, project_id: &str, connected: bool) {
        if let Some(p) = self.projects.iter_mut().find(|p| p.id == project_id) {
            p.connected = connected;
            if connected {
                p.reconnecting = false;
                p.reconnect_attempt = 0;
                p.permanently_disconnected = false;
            }
        }
    }

    /// Set a project's reconnecting state. No-op if the project doesn't exist.
    ///
    /// When `reconnecting` is `true`, `connected` is also set to `false` to keep
    /// the connection state model semantically consistent — a project cannot be
    /// both "connected" and "reconnecting" simultaneously.
    pub fn set_project_reconnecting(&mut self, project_id: &str, reconnecting: bool) {
        if let Some(p) = self.projects.iter_mut().find(|p| p.id == project_id) {
            p.reconnecting = reconnecting;
            if reconnecting {
                p.connected = false;
            }
        }
    }

    /// Set a project's reconnect attempt. No-op if the project doesn't exist.
    pub fn set_project_reconnect_attempt(&mut self, project_id: &str, attempt: u32) {
        if let Some(p) = self.projects.iter_mut().find(|p| p.id == project_id) {
            p.reconnect_attempt = attempt;
        }
    }

    /// Mark a project as permanently disconnected. No-op if the project doesn't exist.
    pub fn set_project_permanently_disconnected(&mut self, project_id: &str) {
        if let Some(p) = self.projects.iter_mut().find(|p| p.id == project_id) {
            p.reconnecting = false;
            p.connected = false;
            p.permanently_disconnected = true;
            p.reconnect_attempt = 0;
        }
    }

    /// Record a successful agent start for a project (resets circuit breaker).
    pub fn record_agent_success(&mut self, project_id: &str) {
        self.circuit_breaker_failures.remove(project_id);
        self.circuit_breaker_tripped_at.remove(project_id);
    }

    /// Record a failed agent start for a project.
    /// Returns `true` if the circuit breaker just tripped (reached threshold).
    pub fn record_agent_failure(&mut self, project_id: &str, threshold: u32) -> bool {
        let count = self
            .circuit_breaker_failures
            .entry(project_id.to_string())
            .or_insert(0);
        *count += 1;
        if *count >= threshold {
            self.circuit_breaker_tripped_at
                .insert(project_id.to_string(), chrono::Utc::now().timestamp());
        }
        *count >= threshold
    }

    /// Check whether the circuit breaker is tripped for a project.
    pub fn is_circuit_breaker_tripped(&self, project_id: &str, threshold: u32) -> bool {
        self.circuit_breaker_failures
            .get(project_id)
            .map(|&c| c >= threshold)
            .unwrap_or(false)
    }

    /// Check if the circuit breaker has cooled down enough for a probe attempt.
    /// Returns `true` if the breaker is tripped AND the cooldown period has elapsed.
    pub fn is_circuit_breaker_half_open(&self, project_id: &str, cooldown_secs: i64) -> bool {
        if !self.circuit_breaker_tripped_at.contains_key(project_id) {
            return false;
        }
        self.circuit_breaker_tripped_at
            .get(project_id)
            .map(|&ts| chrono::Utc::now().timestamp() - ts >= cooldown_secs)
            .unwrap_or(false)
    }

    /// Reset the circuit breaker for a project (manual retry by user).
    pub fn reset_circuit_breaker(&mut self, project_id: &str) {
        self.circuit_breaker_failures.remove(project_id);
        self.circuit_breaker_tripped_at.remove(project_id);
    }
}
