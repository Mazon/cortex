//! Structured application error types.
//!
//! [`AppError`] replaces bare `anyhow::Error` where callers need to classify
//! failures (e.g., retry logic, user-facing messages). Existing code may still
//! use `anyhow::Result` internally — the intent is **incremental** adoption:
//! new or heavily-modified modules migrate to [`AppResult`], others stay on
//! `anyhow` until they are touched.

use std::path::PathBuf;

// ---------------------------------------------------------------------------
// SSE sub-error
// ---------------------------------------------------------------------------

/// Specific failure modes for the SSE event stream.
#[non_exhaustive]
#[derive(Debug, Clone)]
pub enum SseErrorKind {
    /// The remote closed the connection unexpectedly.
    UnexpectedEof,
    /// A single line exceeded the maximum allowed buffer size.
    BufferOverflow,
    /// The stream contained invalid UTF-8.
    InvalidUtf8,
    /// An HTTP-level error occurred while reading the stream.
    Http { status: u16, message: String },
    /// Any other SSE-level failure.
    Other(String),
}

// ---------------------------------------------------------------------------
// AppError
// ---------------------------------------------------------------------------

/// Top-level application error enum.
///
/// Each variant carries enough context for both logging and user-facing
/// display. The [`Display`](std::fmt::Display) impl produces a concise,
/// human-readable message; the full structured data is available via the
/// public fields.
#[non_exhaustive]
#[derive(Debug)]
pub enum AppError {
    /// A configuration file could not be loaded or parsed.
    Config {
        source: anyhow::Error,
        path: PathBuf,
    },

    /// An error originating from SQLite / rusqlite.
    Database { source: rusqlite::Error },

    /// The OpenCode API returned a non-success HTTP status.
    OpenCodeApi { status: u16, message: String },

    /// Failed to establish or maintain a connection to the OpenCode server.
    OpenCodeConnection { message: String },

    /// An error in the SSE event stream.
    SseStream { kind: SseErrorKind },

    /// A standard I/O error with additional context.
    Io {
        source: std::io::Error,
        context: String,
    },

    /// A failure during state persistence (save / restore).
    Persistence { message: String },

    /// A TUI / rendering error.
    Ui { message: String },
}

// ---------------------------------------------------------------------------
// Result alias
// ---------------------------------------------------------------------------

/// Convenience alias for results using [`AppError`].
pub type AppResult<T> = Result<T, AppError>;

// ---------------------------------------------------------------------------
// Display
// ---------------------------------------------------------------------------

impl std::fmt::Display for AppError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Config { source, path } => {
                write!(f, "config error at {}: {}", path.display(), source)
            }
            Self::Database { source } => {
                write!(f, "database error: {}", source)
            }
            Self::OpenCodeApi { status, message } => {
                write!(f, "OpenCode API error ({}): {}", status, message)
            }
            Self::OpenCodeConnection { message } => {
                write!(f, "OpenCode connection error: {}", message)
            }
            Self::SseStream { kind } => {
                write!(f, "SSE stream error: ")?;
                match kind {
                    SseErrorKind::UnexpectedEof => write!(f, "unexpected end of stream"),
                    SseErrorKind::BufferOverflow => write!(f, "line exceeded maximum buffer size"),
                    SseErrorKind::InvalidUtf8 => write!(f, "invalid UTF-8 in event data"),
                    SseErrorKind::Http { status, message } => {
                        write!(f, "HTTP {} – {}", status, message)
                    }
                    SseErrorKind::Other(msg) => write!(f, "{}", msg),
                }
            }
            Self::Io { source, context } => {
                write!(f, "I/O error ({}): {}", context, source)
            }
            Self::Persistence { message } => {
                write!(f, "persistence error: {}", message)
            }
            Self::Ui { message } => {
                write!(f, "UI error: {}", message)
            }
        }
    }
}

// ---------------------------------------------------------------------------
// std::error::Error
// ---------------------------------------------------------------------------

impl std::error::Error for AppError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Config { source, .. } => Some(source.as_ref()),
            Self::Database { source } => Some(source),
            Self::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// From conversions
// ---------------------------------------------------------------------------

impl From<rusqlite::Error> for AppError {
    fn from(source: rusqlite::Error) -> Self {
        Self::Database { source }
    }
}

impl From<std::io::Error> for AppError {
    fn from(source: std::io::Error) -> Self {
        Self::Io {
            context: String::new(),
            source,
        }
    }
}

// ---------------------------------------------------------------------------
// Retry classification
// ---------------------------------------------------------------------------

impl AppError {
    /// Returns `true` if the error is transient and worth retrying.
    ///
    /// - Server errors (5xx) and rate limits (429) are retryable.
    /// - Connection failures are retryable.
    /// - Client errors (4xx except 429), config, database, persistence, and
    ///   UI errors are **not** retryable.
    pub fn is_retryable(&self) -> bool {
        match self {
            Self::OpenCodeApi { status, .. } => *status == 429 || *status >= 500,
            Self::OpenCodeConnection { .. } => true,
            Self::SseStream { kind } => match kind {
                SseErrorKind::UnexpectedEof => true,
                SseErrorKind::Http { status, .. } => *status == 429 || *status >= 500,
                SseErrorKind::BufferOverflow
                | SseErrorKind::InvalidUtf8
                | SseErrorKind::Other(_) => false,
            },
            _ => false,
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_config() {
        let err = AppError::Config {
            source: anyhow::anyhow!("missing field"),
            path: PathBuf::from("/tmp/cortex.toml"),
        };
        let msg = err.to_string();
        assert!(msg.contains("config error"));
        assert!(msg.contains("/tmp/cortex.toml"));
        assert!(msg.contains("missing field"));
    }

    #[test]
    fn display_database() {
        let err = AppError::Database {
            source: rusqlite::Error::QueryReturnedNoRows,
        };
        let msg = err.to_string();
        assert!(msg.contains("database error"));
    }

    #[test]
    fn display_opencode_api() {
        let err = AppError::OpenCodeApi {
            status: 403,
            message: "forbidden".into(),
        };
        let msg = err.to_string();
        assert!(msg.contains("403"));
        assert!(msg.contains("forbidden"));
    }

    #[test]
    fn display_opencode_connection() {
        let err = AppError::OpenCodeConnection {
            message: "ECONNREFUSED".into(),
        };
        let msg = err.to_string();
        assert!(msg.contains("connection error"));
        assert!(msg.contains("ECONNREFUSED"));
    }

    #[test]
    fn display_sse() {
        let err = AppError::SseStream {
            kind: SseErrorKind::UnexpectedEof,
        };
        assert!(err.to_string().contains("unexpected end of stream"));

        let err = AppError::SseStream {
            kind: SseErrorKind::Http {
                status: 502,
                message: "bad gateway".into(),
            },
        };
        let msg = err.to_string();
        assert!(msg.contains("502"));
    }

    #[test]
    fn display_io() {
        let err = AppError::Io {
            source: std::io::Error::new(std::io::ErrorKind::NotFound, "file missing"),
            context: "loading config".into(),
        };
        let msg = err.to_string();
        assert!(msg.contains("I/O error"));
        assert!(msg.contains("loading config"));
        assert!(msg.contains("file missing"));
    }

    #[test]
    fn from_io_error() {
        let io_err = std::io::Error::new(std::io::ErrorKind::BrokenPipe, "pipe broke");
        let app_err: AppError = io_err.into();
        match app_err {
            AppError::Io { source, .. } => {
                assert_eq!(source.kind(), std::io::ErrorKind::BrokenPipe);
            }
            _ => panic!("expected Io variant"),
        }
    }

    #[test]
    fn from_rusqlite_error() {
        let db_err = rusqlite::Error::QueryReturnedNoRows;
        let app_err: AppError = db_err.into();
        match app_err {
            AppError::Database { .. } => {}
            _ => panic!("expected Database variant"),
        }
    }

    #[test]
    fn is_retryable_server_errors() {
        let err = AppError::OpenCodeApi {
            status: 500,
            message: "internal".into(),
        };
        assert!(err.is_retryable());

        let err = AppError::OpenCodeApi {
            status: 503,
            message: "unavailable".into(),
        };
        assert!(err.is_retryable());
    }

    #[test]
    fn is_retryable_rate_limit() {
        let err = AppError::OpenCodeApi {
            status: 429,
            message: "too many".into(),
        };
        assert!(err.is_retryable());
    }

    #[test]
    fn is_not_retryable_client_errors() {
        for status in [400, 401, 403, 404, 422] {
            let err = AppError::OpenCodeApi {
                status,
                message: "client error".into(),
            };
            assert!(
                !err.is_retryable(),
                "status {} should not be retryable",
                status
            );
        }
    }

    #[test]
    fn is_retryable_connection() {
        let err = AppError::OpenCodeConnection {
            message: "refused".into(),
        };
        assert!(err.is_retryable());
    }

    #[test]
    fn is_retryable_sse_eof() {
        let err = AppError::SseStream {
            kind: SseErrorKind::UnexpectedEof,
        };
        assert!(err.is_retryable());
    }

    #[test]
    fn is_not_retryable_other_variants() {
        let err = AppError::Config {
            source: anyhow::anyhow!("bad"),
            path: PathBuf::from("/tmp/c.toml"),
        };
        assert!(!err.is_retryable());

        let err = AppError::Database {
            source: rusqlite::Error::QueryReturnedNoRows,
        };
        assert!(!err.is_retryable());

        let err = AppError::Persistence {
            message: "save failed".into(),
        };
        assert!(!err.is_retryable());

        let err = AppError::Ui {
            message: "render failed".into(),
        };
        assert!(!err.is_retryable());
    }

    #[test]
    fn error_source_chain() {
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "nope");
        let app_err = AppError::Io {
            source: io_err,
            context: "test".into(),
        };
        assert!(std::error::Error::source(&app_err).is_some());
    }
}
