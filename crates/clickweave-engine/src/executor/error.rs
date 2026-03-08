use thiserror::Error;

#[derive(Debug, Error)]
pub enum ExecutorError {
    #[error("LLM error: {0}")]
    Llm(String),

    #[error("Tool call failed: {tool}: {message}")]
    ToolCall { tool: String, message: String },

    #[error("App resolution failed: {0}")]
    AppResolution(String),

    #[error("Element resolution failed: {0}")]
    ElementResolution(String),

    #[error("Click target not found: {0}")]
    ClickTarget(String),

    #[error("CDP error: {0}")]
    Cdp(String),

    #[error("Validation error: {0}")]
    Validation(String),

    #[error("Cancelled")]
    Cancelled,

    #[error("IO error: {0}")]
    Io(String),

    #[error("MCP spawn failed: {0}")]
    McpSpawn(String),
}

/// Alias used throughout the executor.
pub type ExecutorResult<T> = Result<T, ExecutorError>;
