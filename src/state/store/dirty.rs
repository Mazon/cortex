//! Dirty flag tracking and streaming cache management on AppState.

use crate::state::types::*;

impl AppState {
    // ─── Dirty Flag ──────────────────────────────────────────────────────

    /// Mark a specific task as needing to be persisted on the next save.
    /// Also sets the global dirty flag so the persistence loop knows to run.
    pub fn mark_task_dirty(&mut self, task_id: &str) {
        self.dirty_flags.dirty_tasks.insert(task_id.to_string());
        self.dirty_flags.mark_dirty();
    }

    /// Evict stale entries from the streaming render cache.
    ///
    /// Removes cached lines whose key no longer has a corresponding live
    /// session — either a main task in `self.tasks` (keyed by `task_id`)
    /// or a drilled-down subagent in `self.session_tracker.subagent_session_data` (keyed
    /// by `session_id`).  If the cache still exceeds `max_entries` after
    /// that, an arbitrary half is evicted (HashMap has no ordering, so
    /// iteration order is not guaranteed).
    pub fn prune_streaming_cache(&mut self, max_entries: usize) {
        // Remove entries whose backing session no longer exists.
        // Main sessions are keyed by task_id; subagent sessions by session_id.
        self.session_tracker
            .cached_streaming_lines
            .retain(|key, _| {
                self.tasks.contains_key(key)
                    || self.session_tracker.subagent_session_data.contains_key(key)
            });

        // Also evict subagent session data for sessions whose parent task no longer exists
        self.session_tracker
            .subagent_session_data
            .retain(|session_id, _| {
                self.session_tracker
                    .subagent_to_parent
                    .contains_key(session_id)
            });

        // If still too large, remove an arbitrary half (first N/2 entries by
        // iteration order — HashMap order is not meaningful)
        if self.session_tracker.cached_streaming_lines.len() > max_entries {
            let to_remove = self.session_tracker.cached_streaming_lines.len() / 2;
            let keys: Vec<String> = self
                .session_tracker
                .cached_streaming_lines
                .keys()
                .take(to_remove)
                .cloned()
                .collect();
            for key in keys {
                self.session_tracker.cached_streaming_lines.remove(&key);
            }
        }
    }
}
