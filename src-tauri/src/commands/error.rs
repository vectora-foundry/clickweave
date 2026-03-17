use serde::Serialize;
use specta::Type;

/// Structured error type for Tauri IPC commands.
#[derive(Debug, Serialize, Type)]
pub struct CommandError {
    pub kind: ErrorKind,
    pub message: String,
}

#[derive(Debug, Serialize, Type)]
pub enum ErrorKind {
    Validation,
    Io,
    Llm,
    Mcp,
    AlreadyRunning,
    Cancelled,
    Internal,
}

impl std::fmt::Display for CommandError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}: {}", self.kind, self.message)
    }
}

impl CommandError {
    pub fn validation(msg: impl std::fmt::Display) -> Self {
        Self {
            kind: ErrorKind::Validation,
            message: msg.to_string(),
        }
    }

    pub fn io(msg: impl std::fmt::Display) -> Self {
        Self {
            kind: ErrorKind::Io,
            message: msg.to_string(),
        }
    }

    pub fn llm(msg: impl std::fmt::Display) -> Self {
        Self {
            kind: ErrorKind::Llm,
            message: msg.to_string(),
        }
    }

    pub fn mcp(msg: impl std::fmt::Display) -> Self {
        Self {
            kind: ErrorKind::Mcp,
            message: msg.to_string(),
        }
    }

    pub fn already_running() -> Self {
        Self {
            kind: ErrorKind::AlreadyRunning,
            message: "Already running".into(),
        }
    }

    pub fn cancelled() -> Self {
        Self {
            kind: ErrorKind::Cancelled,
            message: "cancelled".into(),
        }
    }

    pub fn internal(msg: impl std::fmt::Display) -> Self {
        Self {
            kind: ErrorKind::Internal,
            message: msg.to_string(),
        }
    }
}
