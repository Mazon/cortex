//! Task orchestration engine — coordinates multi-agent workflows.
//!
//! The orchestration layer decides which agent to invoke for a given task,
//! manages transitions between planning → doing → reviewing stages, and
//! handles auto-progression of tasks through the kanban pipeline.

pub mod engine;
