//! Custom tracing layer that routes Warn/Error events to the TUI notification system.

use std::sync::{Arc, Mutex, Weak};

use crate::state::types::{AppState, NotificationVariant};

/// A tracing layer that captures Warn and Error level events and pushes
/// them as notifications into the AppState notification queue.
///
/// Uses a `Weak<Mutex<AppState>>` reference so the tracing subscriber
/// doesn't prevent AppState from being dropped during shutdown.
pub struct TuiNotificationLayer {
    state: Arc<Mutex<Option<Weak<Mutex<AppState>>>>>,
    /// Maximum message length for notification (truncated if longer).
    max_message_len: usize,
}

impl TuiNotificationLayer {
    pub fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(None)),
            max_message_len: 80,
        }
    }

    /// Set the AppState reference after it's been created.
    /// Called once from main.rs after AppState initialization.
    pub fn set_state(&self, state: &Arc<Mutex<AppState>>) {
        let mut guard = self.state.lock().unwrap_or_else(|e| e.into_inner());
        *guard = Some(Arc::downgrade(state));
    }

    fn push_notification(&self, message: String, variant: NotificationVariant) {
        let guard = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(weak) = guard.as_ref() {
            if let Some(state_arc) = weak.upgrade() {
                if let Ok(mut state) = state_arc.lock() {
                    let truncated = if message.chars().count() > self.max_message_len {
                        format!(
                            "{}...",
                            message
                                .chars()
                                .take(self.max_message_len - 3)
                                .collect::<String>()
                        )
                    } else {
                        message
                    };
                    state.set_notification(truncated, variant, 5000);
                    state.mark_render_dirty();
                }
            }
        }
    }
}

impl Clone for TuiNotificationLayer {
    fn clone(&self) -> Self {
        Self {
            state: self.state.clone(),
            max_message_len: self.max_message_len,
        }
    }
}

impl<S> tracing_subscriber::Layer<S> for TuiNotificationLayer
where
    S: tracing::Subscriber,
{
    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        _ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        let metadata = event.metadata();
        let level = *metadata.level();

        if level == tracing::Level::ERROR {
            let mut visitor = StringVisitor::new();
            event.record(&mut visitor);
            let message = if visitor.message.is_empty() {
                metadata.name().to_string()
            } else {
                format!("{}: {}", metadata.target(), visitor.message)
            };
            self.push_notification(message, NotificationVariant::Error);
        } else if level == tracing::Level::WARN {
            let mut visitor = StringVisitor::new();
            event.record(&mut visitor);
            let message = if visitor.message.is_empty() {
                metadata.name().to_string()
            } else {
                visitor.message
            };

            // Filter out SSE infrastructure warnings — these are handled by
            // the status bar's connection indicator, not the notification bar.
            if is_sse_infrastructure_warning(&message) {
                return;
            }

            self.push_notification(message, NotificationVariant::Warning);
        }
    }
}

/// Returns true if the message is a warning that should be suppressed from
/// the TUI notification bar because it's already handled elsewhere (e.g.
/// by an explicit notification call or a status bar indicator).
/// Includes SSE connection lifecycle events and hung-task detection.
fn is_sse_infrastructure_warning(message: &str) -> bool {
    message.contains("SSE connection error")
        || message.contains("SSE connection failed")
        || message.contains("SSE max retries reached")
        || message.contains("SSE reconnecting")
        || message.contains("Skipping malformed SSE event")
        || message.contains("Failed to fetch session messages for finalization")
        || message.contains("SSE buffer exceeded")
        || message.contains("Marking task as Hung") // Handled by explicit notification in app.rs
}

/// Helper visitor that extracts the "message" field from a tracing event.
struct StringVisitor {
    message: String,
}

impl StringVisitor {
    fn new() -> Self {
        Self {
            message: String::new(),
        }
    }
}

impl tracing::field::Visit for StringVisitor {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.message = format!("{:?}", value);
            // Strip surrounding quotes if present
            if self.message.starts_with('"')
                && self.message.ends_with('"')
                && self.message.len() >= 2
            {
                self.message = self.message[1..self.message.len() - 1].to_string();
            }
        }
    }

    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        if field.name() == "message" {
            self.message = value.to_string();
        }
    }
}
