//! App state store — core CRUD methods on AppState.
//!
//! This module contains pure CRUD operations (add/remove/move tasks and
//! projects), session data management, dirty flag handling, persistence
//! restore, and internal helpers.
//!
//! SSE processing methods live in [`crate::state::sse_processor`],
//! navigation/UI methods in [`crate::state::navigation`], and
//! permission/question handling in [`crate::state::permissions`].

mod dirty;
mod persistence;
mod projects;
mod sessions;
mod tasks;

#[cfg(test)]
mod tests;

/// Maximum byte size for `TaskDetailSession::streaming_text`.
/// When a session's streaming buffer exceeds this cap, old text is
/// truncated from the beginning to keep the most recent content.
/// Default: 1 MiB (1,048,576 bytes).
pub const STREAMING_TEXT_CAP_BYTES: usize = 1_048_576;
