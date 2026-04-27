//! Application state — domain types and mutation methods.
//!
//! This module contains [`types`] (the core data model: tasks, projects,
//! kanban layout, UI state), [`store`] (core CRUD methods on [`AppState`]),
//! [`sse_processor`] (SSE event processing), [`navigation`] (UI/navigation
//! methods), and [`permissions`] (permission/question handling).

pub mod navigation;
pub mod permissions;
pub mod sse_processor;
pub mod store;
pub mod types;
