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

    #[error("Variable not found: {reference}")]
    VariableNotFound { reference: String },

    #[error("Invalid coordinates: {0}")]
    InvalidCoordinates(String),

    #[error(
        "No CDP connection — ensure a FocusWindow or LaunchApp targeting a CDP-capable app runs before {node_type}"
    )]
    NoCdpConnection { node_type: String },
}

/// Alias used throughout the executor.
pub type ExecutorResult<T> = Result<T, ExecutorError>;
