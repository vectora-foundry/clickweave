use crate::Message;
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

    /// Whether any planning tools are available.
    fn has_planning_tools(&self) -> bool {
        !self.available_planning_tools().is_empty()
    }

    /// List the planning tools available (those present on the MCP server).
    /// Returns OpenAI-compatible tool definitions for the `tools` parameter.
    fn available_planning_tools(&self) -> Vec<Value>;
}

/// All tools available to the planner LLM during context gathering.
/// Split into planning-only (never valid in workflows) and dual-use
/// (valid in both planning and workflows).
pub const PLANNING_TOOL_NAMES: &[&str] = &[
    // Planning-only
    "probe_app",
    "take_ax_snapshot",
    "cdp_connect",
    // Dual-use (also valid as workflow nodes)
    "quit_app",
    "launch_app",
    // CDP inspection tools (available after cdp_connect)
    "cdp_take_snapshot",
    "cdp_list_pages",
    "cdp_select_page",
];

/// Planning-only tools that must NEVER appear as workflow node types.
/// Dual-use tools (launch_app, quit_app, select_page, etc.) are valid
/// in both planning and workflow contexts.
pub const PLANNING_ONLY_TOOL_NAMES: &[&str] = &["probe_app", "take_ax_snapshot", "cdp_connect"];

/// Tools that are always allowed (read-only observation).
const ALWAYS_ALLOWED: &[&str] = &["probe_app", "take_ax_snapshot"];

/// Tools that require user confirmation (side effects).
const REQUIRES_CONFIRMATION: &[&str] = &["quit_app", "launch_app", "cdp_connect"];

/// Tools allowed only after CDP is connected (read-only).
const CDP_READ_ONLY: &[&str] = &["cdp_take_snapshot", "cdp_list_pages", "cdp_select_page"];

/// Classify a planning tool by permission level.
pub fn planning_tool_permission(name: &str) -> ToolPermission {
    if ALWAYS_ALLOWED.contains(&name) {
        ToolPermission::Allowed
    } else if REQUIRES_CONFIRMATION.contains(&name) {
        ToolPermission::RequiresConfirmation
    } else if CDP_READ_ONLY.contains(&name) {
        ToolPermission::Allowed
    } else {
        ToolPermission::Blocked
    }
}

/// Check whether a tool name is available during planning.
pub fn is_planning_tool(name: &str) -> bool {
    PLANNING_TOOL_NAMES.contains(&name)
}

/// Check whether a tool is planning-only (not valid as a workflow node).
pub fn is_planning_only_tool(name: &str) -> bool {
    PLANNING_ONLY_TOOL_NAMES.contains(&name)
}

/// Hard limits for the planning tool-use loop.
pub const MAX_PLANNING_TOOL_CALLS: usize = 15;
pub const MAX_BLOCKED_REJECTIONS: usize = 3;

/// Maximum characters for a tool result before truncation.
/// Keeps the result within ~3K tokens to avoid blowing context windows.
const MAX_TOOL_RESULT_CHARS: usize = 12_000;

/// Execute a planning tool and return a tool_result message.
pub(crate) async fn execute_tool<E: PlannerToolExecutor>(
    executor: &E,
    name: &str,
    args: Value,
    tc_id: &str,
) -> Message {
    match executor.call_tool(name, args).await {
        Ok(result) => {
            if result.len() > MAX_TOOL_RESULT_CHARS {
                tracing::warn!(
                    tool = %name,
                    original_len = result.len(),
                    truncated_to = MAX_TOOL_RESULT_CHARS,
                    "Planning tool result truncated"
                );
                let truncated = &result[..result
                    .char_indices()
                    .nth(MAX_TOOL_RESULT_CHARS)
                    .map(|(i, _)| i)
                    .unwrap_or(result.len())];
                Message::tool_result(
                    tc_id,
                    format!(
                        "{}\n\n[truncated — result was {} chars]",
                        truncated,
                        result.len()
                    ),
                )
            } else {
                Message::tool_result(tc_id, &result)
            }
        }
        Err(e) => Message::tool_result(tc_id, format!("Tool call failed: {}", e)),
    }
}
