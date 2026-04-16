//! OpenCode SDK integration — client, server management, and SSE event loop.
//!
//! * [`client`] — thin wrapper around `opencode-sdk-rs` for session CRUD,
//!   prompt sending, permission resolution, and type conversion helpers.
//! * [`server`] — per-project OpenCode server process lifecycle management
//!   (spawn, health-check, stop) via `opencode serve`.
//! * [`events`] — SSE event loop that subscribes to the OpenCode event
//!   stream and dispatches events into [`AppState`](crate::state::types::AppState).

pub mod client;
pub mod server;
pub mod events;
