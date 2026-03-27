use anyhow::Result;
use serde_json::Value;
use std::future::Future;

/// Permission level for a planning-time tool call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolPermission {
    /// Execute immediately, no user interaction needed.
    Allowed,
    /// Pause and ask the user before executing.
    RequiresConfirmation,
    /// Reject with an error message to the LLM.
    Blocked,
}

/// Callback for executing MCP tools during planning.
/// Implemented by the Tauri layer, which holds the MCP client.
pub trait PlannerToolExecutor: Send + Sync {
    /// Execute an MCP tool call. Returns the tool result as text.
    fn call_tool(&self, name: &str, args: Value) -> impl Future<Output = Result<String>> + Send;

    /// Check if a tool is allowed during planning.
    fn permission(&self, name: &str) -> ToolPermission;

    /// Request user confirmation for a tool call.
    /// Pauses the planner loop until the user responds.
    /// Returns true if the user approved, false if declined.
    fn request_confirmation(
        &self,
        message: &str,
        tool_name: &str,
    ) -> impl Future<Output = Result<bool>> + Send;

    /// List the planning tools available (those present on the MCP server).
    /// Returns OpenAI-compatible tool definitions for the `tools` parameter.
    fn available_planning_tools(&self) -> Vec<Value>;
}
