//! SSE event processing methods on AppState.
//!
//! Extracted from `store.rs` to separate SSE event handling (deduplication,
//! delta tracking, streaming text management) from pure CRUD and navigation.
//!
//! ## Performance Characteristics
//!
//! **Dedup state (`seen_delta_keys`):** Each `TaskDetailSession` maintains a
//! `HashSet<(String, String)>` of `(message_id, part_id)` pairs seen during
//! the session. This set is bounded by `MAX_SEEN_DELTA_KEYS` (default: 10,000)
//! — when the limit is exceeded, the oldest half is cleared to prevent
//! unbounded memory growth during long-running sessions. The set is fully
//! cleared when a session finalizes (agent completes/idle/error).
//!
//! **Streaming text truncation:** `enforce_streaming_cap()` uses batched
//! truncation — it only triggers `String::drain()` when the buffer exceeds
//! the cap by at least 10%, amortizing the cost of front-truncation. For a
//! 1 MiB cap, this means truncation happens roughly every ~100 KB of new
//! streaming text rather than on every delta.
//!
//! **Lock contention:** Every SSE event acquires the global `AppState` mutex,
//! as does every TUI render tick. The render-dirty flag (`AtomicBool`) avoids
//! acquiring the lock for rendering when nothing has changed. Future work could
//! consider `parking_lot::Mutex` for reduced contention or lock-free approaches
//! for hot paths.

use crate::state::types::*;

use super::store::STREAMING_TEXT_CAP_BYTES;

/// Maximum number of dedup keys retained per session before pruning.
///
/// Each key is a `(message_id, part_id)` tuple (~50-100 bytes). At 10,000
/// entries, this consumes ~1-2 MB per session. Long-running sessions with
/// many message parts (e.g., large file edits, extensive tool use) can
/// accumulate keys rapidly. When the limit is reached, the oldest half is
/// cleared — this is safe because dedup is only needed for *recent* events
/// (the server won't replay events from the beginning of a long session).
const MAX_SEEN_DELTA_KEYS: usize = 10_000;

/// Fraction of the cap that must be exceeded before truncation triggers.
///
/// Set to 10% — the buffer must be at least 110% of the cap before
/// `enforce_streaming_cap()` performs the expensive `String::drain()`.
/// This amortizes the cost of front-truncation, which involves:
/// 1. Finding the UTF-8 boundary (linear scan)
/// 2. Shifting the entire string contents in memory
const TRUNCATION_THRESHOLD_FRACTION: usize = 10;

impl AppState {
    // ─── SSE Processing Helpers ──────────────────────────────────────────

    /// Handle a `MessagePartDelta` SSE event — append text to the
    /// streaming buffer for the corresponding task's session.
    /// When the buffer exceeds `STREAMING_TEXT_CAP_BYTES`, old text
    /// is truncated from the beginning (keeping the most recent content).
    ///
    /// Deduplicates events by tracking `(message_id, part_id)` pairs:
    /// if the same pair is seen again (e.g., after SSE reconnection),
    /// the delta is silently skipped to prevent text duplication.
    ///
    /// If the session_id belongs to a subagent session, the delta is
    /// routed to the subagent's session data in `subagent_session_data`.
    pub fn process_message_part_delta(
        &mut self,
        session_id: &str,
        message_id: &str,
        part_id: &str,
        field: &str,
        delta: &str,
    ) {
        // Route to subagent session data if this is a child session
        if let Some(parent) = self.get_parent_task_for_subagent(session_id) {
            let parent_task_id = parent.to_string();
            self.process_subagent_message_delta(
                session_id,
                &parent_task_id,
                message_id,
                part_id,
                field,
                delta,
            );
            return;
        }

        if let Some(task_id) = self
            .get_task_id_by_session(session_id)
            .map(|s| s.to_string())
        {
            let session = self
                .session_tracker.task_sessions
                .entry(task_id.clone())
                .or_insert_with(|| TaskDetailSession {
                    task_id,
                    ..Default::default()
                });

            // Deduplication: skip if this (message_id, part_id) was already
            // seen for a *previous* part. Consecutive deltas for the same
            // part share the same key and are always accepted (continuation).
            // A different key that's already in the set indicates a replay
            // from SSE reconnection — skip it to prevent text duplication.
            //
            // Defense-in-depth: also reject continuations (same key) when the
            // delta content is *identical* to the last processed content.
            // Two concurrent SSE loops delivering the same event will produce
            // the same key AND the same content, while a true continuation
            // (next chunk of the same streaming part) will always differ.
            let delta_key = (message_id.to_string(), part_id.to_string());
            let is_continuation = session.last_delta_key.as_ref() == Some(&delta_key);
            if !is_continuation && session.seen_delta_keys.contains(&delta_key) {
                // Key was seen before but is NOT the current part — replay.
                return;
            }
            if is_continuation
                && session.last_delta_content.as_deref() == Some(delta)
            {
                // Same key AND identical content — duplicate from a second SSE
                // loop, not a genuine continuation. Skip to prevent doubling.
                return;
            }
            if !is_continuation {
                // New part we haven't seen — record it.
                session.seen_delta_keys.insert(delta_key.clone());
                // Prune if the set has grown beyond the capacity limit.
                // We clear the entire set rather than trying to selectively
                // remove entries because:
                // 1. HashSet iteration order is non-deterministic — we can't
                //    meaningfully identify "oldest" entries.
                // 2. The dedup is best-effort defense against replayed events.
                //    Missing a dedup for an old event just means duplicate text,
                //    which is benign (the content-level dedup in the continuation
                //    check still catches exact duplicates from concurrent loops).
                // 3. The set will quickly repopulate with recent events.
                if session.seen_delta_keys.len() > MAX_SEEN_DELTA_KEYS {
                    // Full eviction at capacity limit. This trades dedup coverage
                    // for bounded memory. In practice, the `last_delta_key` +
                    // `last_delta_content` defense-in-depth checks still catch
                    // replays for the active stream, so the risk window is limited
                    // to interleaved subagent events. Proper LRU eviction would
                    // require an `IndexSet` dependency; given this set repopulates
                    // quickly with recent events, full clear is acceptable.
                    session.seen_delta_keys.clear();
                    tracing::debug!(
                        "Pruned seen_delta_keys for task session (exceeded {} entries)",
                        MAX_SEEN_DELTA_KEYS
                    );
                }
            }
            session.last_delta_key = Some(delta_key);
            session.last_delta_content = Some(delta.to_string());

            // Only append to streaming_text for "text" field deltas.
            // Other field types (e.g., "reasoning") should be handled
            // separately — they must not pollute the text buffer.
            if field != "text" {
                return;
            }

            match &mut session.streaming_text {
                Some(text) => {
                    text.push_str(delta);
                }
                None => {
                    session.streaming_text = Some(delta.to_string());
                }
            }
            // Enforce cap: truncate from the beginning if over limit.
            // Keep the most recent content (tail of the buffer).
            if let Some(ref mut text) = session.streaming_text {
                Self::enforce_streaming_cap(text);
            }
            session.render_version += 1;
        }
    }

    /// Handle a status event for a subagent session.
    /// Updates the subagent's session data in `subagent_session_data`.
    fn process_subagent_status(&mut self, session_id: &str, _parent_task_id: &str, status: &str) {
        // Ensure subagent session data exists
        let entry = self
            .session_tracker.subagent_session_data
            .entry(session_id.to_string())
            .or_insert_with(TaskDetailSession::default);
        entry.session_id = Some(session_id.to_string());

        match status {
            "complete" | "completed" => {
                // Clear dedup tracking when subagent completes.
                entry.seen_delta_keys.clear();
                entry.last_delta_key = None;
                entry.last_delta_content = None;
            }
            "error" => {
                entry.seen_delta_keys.clear();
                entry.last_delta_key = None;
                entry.last_delta_content = None;
            }
            _ => {}
        }

        self.mark_render_dirty();
    }

    /// Handle an idle event for a subagent session.
    fn process_subagent_idle(&mut self, session_id: &str, _parent_task_id: &str) {
        self.mark_subagent_inactive(session_id);
        // Clear dedup tracking for the subagent session.
        if let Some(entry) = self.session_tracker.subagent_session_data.get_mut(session_id) {
            entry.seen_delta_keys.clear();
            entry.last_delta_key = None;
            entry.last_delta_content = None;
        }
        self.mark_render_dirty();
    }

    /// Handle a message delta for a subagent session.
    /// Appends to the subagent's streaming text buffer in `subagent_session_data`.
    /// Deduplicates using `(message_id, part_id)` to prevent replay doubling.
    fn process_subagent_message_delta(
        &mut self,
        session_id: &str,
        _parent_task_id: &str,
        message_id: &str,
        part_id: &str,
        field: &str,
        delta: &str,
    ) {
        let entry = self
            .session_tracker.subagent_session_data
            .entry(session_id.to_string())
            .or_insert_with(TaskDetailSession::default);
        entry.session_id = Some(session_id.to_string());

        // Deduplication: skip if this (message_id, part_id) was already
        // seen for a *previous* part. Consecutive deltas for the same
        // part share the same key and are always accepted (continuation).
        // Also reject continuations with identical content (defense-in-depth
        // against concurrent SSE connections).
        let delta_key = (message_id.to_string(), part_id.to_string());
        let is_continuation = entry.last_delta_key.as_ref() == Some(&delta_key);
        if !is_continuation && entry.seen_delta_keys.contains(&delta_key) {
            return;
        }
        if is_continuation
            && entry.last_delta_content.as_deref() == Some(delta)
        {
            return;
        }
        if !is_continuation {
            entry.seen_delta_keys.insert(delta_key.clone());
            // Prune if the set has grown beyond the capacity limit.
            if entry.seen_delta_keys.len() > MAX_SEEN_DELTA_KEYS {
                entry.seen_delta_keys.clear();
            }
        }
        entry.last_delta_key = Some(delta_key);
        entry.last_delta_content = Some(delta.to_string());

        // Only append to streaming_text for "text" field deltas.
        // Other field types (e.g., "reasoning") are ignored for the
        // streaming buffer.
        if field != "text" {
            return;
        }

        match &mut entry.streaming_text {
            Some(text) => {
                text.push_str(delta);
            }
            None => {
                entry.streaming_text = Some(delta.to_string());
            }
        }

        // Enforce cap
        if let Some(ref mut text) = entry.streaming_text {
            Self::enforce_streaming_cap(text);
        }

        entry.render_version += 1;
    }

    /// Handle a `SessionStatus` SSE event — map the status string to
    /// [`AgentStatus`] and update the corresponding task.
    /// Also marks subagent sessions as inactive when they complete.
    pub fn process_session_status(&mut self, session_id: &str, status: &str) {
        // Check if this is a subagent session completing
        if matches!(status, "complete" | "completed" | "error") {
            self.mark_subagent_inactive(session_id);
        }

        // Route to parent task if this is a subagent session
        if let Some(parent_task_id) = self.get_parent_task_for_subagent(session_id).map(|s| s.to_string()) {
            self.process_subagent_status(session_id, &parent_task_id, status);
            return;
        }

        if let Some(task_id) = self
            .get_task_id_by_session(session_id)
            .map(|s| s.to_string())
        {
            let agent_status = match status {
                "running" | "busy" => AgentStatus::Running,
                "complete" | "completed" => AgentStatus::Complete,
                "idle" => {
                    // Don't update status — SessionIdle event handles this
                    // with proper Ready/Complete logic. A SessionStatus "idle"
                    // arriving after SessionIdle would overwrite Ready→Complete.
                    return;
                }
                _ => {
                    tracing::warn!("Unknown session status '{}' for task session, ignoring", status);
                    return;
                }
            };
            self.update_task_agent_status(&task_id, agent_status);
        }
    }

    /// Handle a `SessionIdle` SSE event — mark the task as complete
    /// and show a success notification.
    /// Also marks any subagent session as inactive.
    /// Returns the task ID if a task was found and marked complete, `None` otherwise.
    pub fn process_session_idle(&mut self, session_id: &str) -> Option<String> {
        // Mark subagent as inactive if this is a child session
        self.mark_subagent_inactive(session_id);

        // Route to parent task if this is a subagent session
        if let Some(parent) = self.get_parent_task_for_subagent(session_id) {
            let parent_task_id = parent.to_string();
            self.process_subagent_idle(session_id, &parent_task_id);
            return None; // Don't trigger auto-progression for subagent sessions
        }

        self.get_task_id_by_session(session_id)
            .map(|s| s.to_string())
            .map(|task_id| {
                self.update_task_agent_status(&task_id, AgentStatus::Complete);

                let agent_label = self.tasks.get(&task_id)
                    .and_then(|t| t.agent_type.clone())
                    .unwrap_or_else(|| "agent".to_string());
                self.set_notification(
                    format!("{} agent completed", agent_label),
                    NotificationVariant::Success,
                    5000,
                );
                task_id
            })
    }

    /// Handle a `SessionError` SSE event — record the error on the task.
    pub fn process_session_error(&mut self, session_id: &str, error: &str) {
        // Check if this is a subagent session first
        if let Some(parent_task_id) = self.get_parent_task_for_subagent(session_id).map(|s| s.to_string()) {
            self.mark_subagent_error(session_id, error);
            // Also surface on the parent task as a notification
            let agent_name = self.session_tracker.subagent_sessions
                .get(&parent_task_id)
                .and_then(|sessions| sessions.iter().find(|s| s.session_id == session_id))
                .map(|s| s.agent_name.clone())
                .unwrap_or_else(|| "subagent".to_string());
            self.set_notification(
                format!("{} agent error: {}", agent_name, error.chars().take(80).collect::<String>()),
                crate::state::types::NotificationVariant::Error,
                5000,
            );
        } else if let Some(task_id) = self
            .get_task_id_by_session(session_id)
            .map(|s| s.to_string())
        {
            self.set_task_error(&task_id, error.to_string());
        }
    }

    /// Handle a `PermissionAsked` SSE event — create a pending permission
    /// request for the task.
    pub fn process_permission_asked(
        &mut self,
        session_id: &str,
        perm_id: &str,
        tool: &str,
        desc: &str,
    ) {
        // Route to parent task if this is a subagent session
        let (task_id, effective_session_id) = if let Some(parent_task_id) =
            self.get_parent_task_for_subagent(session_id).map(|s| s.to_string())
        {
            (parent_task_id, session_id.to_string())
        } else if let Some(task_id) = self
            .get_task_id_by_session(session_id)
            .map(|s| s.to_string())
        {
            (task_id, session_id.to_string())
        } else {
            return;
        };

        let request = PermissionRequest {
            id: perm_id.to_string(),
            session_id: effective_session_id,
            tool_name: tool.to_string(),
            description: desc.to_string(),
            status: "pending".to_string(),
            details: None,
        };
        self.add_permission_request(&task_id, request);
    }

    /// Extract plan output from the session's streaming text or message history
    /// and store it in `task.plan_output`. Called when an agent completes,
    /// before auto-progression starts the next agent.
    pub fn extract_plan_output(&mut self, task_id: &str) {
        let plan = if let Some(session) = self.session_tracker.task_sessions.get(task_id) {
            // Prefer finalized messages (assistant text parts)
            let from_messages: String = session.messages.iter()
                .rev()
                .filter_map(|msg| {
                    if matches!(msg.role, MessageRole::Assistant) {
                        msg.parts.iter().filter_map(|p| {
                            if let TaskMessagePart::Text { text } = p {
                                Some(text.as_str())
                            } else {
                                None
                            }
                        }).collect::<Vec<_>>().join("\n").into()
                    } else {
                        None
                    }
                })
                .collect::<Vec<_>>()
                .join("\n")
                .trim()
                .to_string();

            if !from_messages.is_empty() {
                from_messages
            } else if let Some(ref text) = session.streaming_text {
                text.trim().to_string()
            } else {
                return;
            }
        } else {
            return;
        };

        // Enforce a 50KB cap — keep the tail (most recent content)
        const PLAN_OUTPUT_CAP_BYTES: usize = 50 * 1024;
        let plan = if plan.len() > PLAN_OUTPUT_CAP_BYTES {
            let start = plan.len() - PLAN_OUTPUT_CAP_BYTES;
            let mut i = start;
            while i < plan.len() && !plan.is_char_boundary(i) { i += 1; }
            plan[i..].to_string()
        } else {
            plan
        };

        if let Some(task) = self.tasks.get_mut(task_id) {
            task.plan_output = Some(plan);
            self.mark_task_dirty(task_id);
        }
    }

    /// Finalize a completed session's streaming output into persistent
    /// message history.  Called after the agent finishes and the full
    /// message list has been fetched from the OpenCode server.
    ///
    /// Sets `session.messages` (registering any subagent sessions found
    /// in the parts) and clears `streaming_text` so that the completed
    /// messages are rendered without duplication.
    ///
    /// Returns `true` if the session actually had streaming text to
    /// finalize (i.e., this is not a no-op), `false` otherwise.
    pub fn finalize_session_streaming(
        &mut self,
        task_id: &str,
        messages: Vec<TaskMessage>,
    ) -> bool {
        let has_streaming = self
            .session_tracker.task_sessions
            .get(task_id)
            .is_some_and(|s| s.streaming_text.is_some());

        self.update_session_messages(task_id, messages);

        // Extract plan output while session.messages is populated with the full
        // fetched message list. This must happen BEFORE update_streaming_text
        // clears streaming_text, so the fallback path in extract_plan_output
        // can still access it if the messages contain no assistant text parts.
        //
        // Ordering dependency: auto-progression (on_agent_completed → start_agent)
        // is also async, and the "do" agent's build_prompt_for_agent reads
        // task.plan_output. Since both finalization and auto-progression are
        // spawned on the same tokio runtime, the finalization lock-hold completes
        // before start_agent acquires the lock to build the prompt.
        self.extract_plan_output(task_id);

        self.update_streaming_text(task_id, None);

        // Clear dedup tracking — no longer needed after finalization.
        // For main sessions this is the only place these are cleared
        // (subagents also clear on complete/idle/error in
        // process_session_status / process_session_idle).
        if let Some(session) = self.session_tracker.task_sessions.get_mut(task_id) {
            session.seen_delta_keys.clear();
            session.last_delta_key = None;
            session.last_delta_content = None;
        }

        has_streaming
    }

    /// Truncate streaming text from the beginning to enforce the cap.
    /// Keeps the most recent content (tail of the buffer).
    /// Handles UTF-8 boundary safety.
    ///
    /// ## Performance
    ///
    /// Uses batched truncation: the buffer must exceed the cap by at least
    /// `TRUNCATION_THRESHOLD_FRACTION`% (default 10%) before truncation
    /// triggers. This amortizes the cost of `String::drain()`, which
    /// involves:
    /// 1. A linear scan for the UTF-8 boundary (~1-4 byte comparisons)
    /// 2. A memmove of the entire remaining string content
    ///
    /// For a 1 MiB cap with 10% threshold, truncation fires roughly every
    /// ~100 KB of new streaming text instead of on every single delta.
    /// This is significant during rapid streaming (e.g., large file reads)
    /// where deltas arrive every few milliseconds.
    ///
    /// Future optimization: a `VecDeque<u8>` or ring buffer could avoid the
    /// memmove entirely by prepending at the back and dropping from the
    /// front. However, `String` is simpler and the current cap (1 MiB) means
    /// the memmove cost is bounded and infrequent.
    pub(crate) fn enforce_streaming_cap(text: &mut String) {
        let cap = STREAMING_TEXT_CAP_BYTES;
        if text.len() <= cap {
            return;
        }
        // Batched truncation: only truncate when significantly over the cap.
        // This avoids the expensive drain+memmove on every small delta.
        let threshold = cap + cap / TRUNCATION_THRESHOLD_FRACTION;
        if text.len() <= threshold {
            return;
        }
        let excess = text.len() - cap;
        let mut split_at = excess;
        while split_at < text.len() && !text.is_char_boundary(split_at) {
            split_at += 1;
        }
        if split_at < text.len() {
            let _ = text.drain(..split_at);
        }
    }
}
