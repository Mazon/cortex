//! SSE event loop — subscribe to events, match variants, update state directly.

mod dispatch;
mod event_loop;

#[cfg(test)]
mod tests;

pub use event_loop::sse_event_loop;
pub(crate) use dispatch::determine_completion_status;
pub(crate) use dispatch::process_event;

/// Default maximum consecutive SSE reconnection attempts.
/// Used when the config field is 0 (which would mean "retry forever").
pub(super) const DEFAULT_SSE_MAX_RETRIES: u32 = 50;
