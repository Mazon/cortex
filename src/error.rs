use anyhow::Result;

/// Top-level application error type for the TUI layer.
#[derive(Debug)]
pub enum AppError {
    Config(String),
    Io(std::io::Error),
    OpenCode(String),
    Database(String),
    State(String),
}

impl std::fmt::Display for AppError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AppError::Config(msg) => write!(f, "Configuration error: {}", msg),
            AppError::Io(err) => write!(f, "IO error: {}", err),
            AppError::OpenCode(msg) => write!(f, "OpenCode error: {}", msg),
            AppError::Database(msg) => write!(f, "Database error: {}", msg),
            AppError::State(msg) => write!(f, "State error: {}", msg),
        }
    }
}

impl std::error::Error for AppError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            AppError::Io(err) => Some(err),
            _ => None,
        }
    }
}

impl From<std::io::Error> for AppError {
    fn from(err: std::io::Error) -> Self {
        AppError::Io(err)
    }
}

impl From<rusqlite::Error> for AppError {
    fn from(err: rusqlite::Error) -> Self {
        AppError::Database(err.to_string())
    }
}

/// Result type alias for the application.
pub type AppResult<T> = Result<T, AppError>;
