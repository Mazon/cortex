//! Application state — domain types and mutation methods.
//!
//! This module contains [`types`] (the core data model: tasks, projects,
//! kanban layout, UI state) and [`store`] (mutation methods on [`AppState`]
//! that keep the in-memory representation consistent).

pub mod store;
pub mod types;
