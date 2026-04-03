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
    "cdp_find_elements",
];

/// Planning-only tools that must NEVER appear as workflow node types.
/// Dual-use tools (launch_app, quit_app, select_page, etc.) are valid
/// in both planning and workflow contexts.
pub const PLANNING_ONLY_TOOL_NAMES: &[&str] = &[
    "probe_app",
    "take_ax_snapshot",
    "cdp_connect",
    "cdp_find_elements",
];

/// Extract the tool name from an OpenAI-compatible tool definition.
pub fn tool_name(tool: &serde_json::Value) -> Option<&str> {
    tool.get("function")
        .and_then(|f| f.get("name"))
        .and_then(|n| n.as_str())
}

/// Native action tools — remove from workflow catalog when all apps are Electron/Chrome.
pub const NATIVE_ACTION_TOOLS: &[&str] = &[
    "click",
    "type_text",
    "press_key",
    "scroll",
    "drag",
    "move_mouse",
];

/// CDP action tools — remove from workflow catalog when all apps are Native.
/// Includes both MCP schema names (cdp_-prefixed) and legacy aliases.
pub const CDP_ACTION_TOOLS: &[&str] = &[
    "cdp_click",
    "cdp_type_text",
    "cdp_press_key",
    "cdp_hover",
    "cdp_fill",
    "cdp_navigate",
    "cdp_new_page",
    "cdp_close_page",
    "cdp_select_page",
    "cdp_handle_dialog",
    "cdp_wait_for",
    // Legacy aliases (planner prompt may use unprefixed names):
    "wait_for",
    "fill",
    "navigate_page",
    "new_page",
    "close_page",
    "select_page",
    "handle_dialog",
];

/// Filter a tool list by app type. Returns tools appropriate for the given app kinds.
/// If both flags are false (mixed or unknown), returns all tools.
pub fn filter_tools_by_app_type(
    tools: &[serde_json::Value],
    all_cdp: bool,
    all_native: bool,
) -> Vec<serde_json::Value> {
    if !all_cdp && !all_native {
        return tools.to_vec();
    }

    let deny_list: &[&str] = if all_cdp {
        NATIVE_ACTION_TOOLS
    } else {
        CDP_ACTION_TOOLS
    };

    tools
        .iter()
        .filter(|tool| {
            let name = tool_name(tool).unwrap_or("");
            !deny_list.contains(&name)
        })
        .cloned()
        .collect()
}

/// Tools that are always allowed (read-only observation).
const ALWAYS_ALLOWED: &[&str] = &["probe_app", "take_ax_snapshot"];

/// Tools that require user confirmation (side effects).
const REQUIRES_CONFIRMATION: &[&str] = &["quit_app", "launch_app", "cdp_connect"];

/// Confirmable tool metadata for the permissions UI.
pub const CONFIRMABLE_TOOLS: &[(&str, &str)] = &[
    ("quit_app", "Closes a running application"),
    ("launch_app", "Opens an application"),
    (
        "cdp_connect",
        "Connects to app via Chrome DevTools Protocol",
    ),
];

/// Tools allowed only after CDP is connected (read-only).
const CDP_READ_ONLY: &[&str] = &[
    "cdp_take_snapshot",
    "cdp_list_pages",
    "cdp_select_page",
    "cdp_find_elements",
];

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cdp_find_elements_is_planning_tool() {
        assert!(is_planning_tool("cdp_find_elements"));
    }

    #[test]
    fn cdp_find_elements_is_planning_only() {
        assert!(is_planning_only_tool("cdp_find_elements"));
    }

    #[test]
    fn cdp_find_elements_permission_is_allowed() {
        assert_eq!(
            planning_tool_permission("cdp_find_elements"),
            ToolPermission::Allowed
        );
    }

    /// Every tool in CDP_ACTION_TOOLS must be hardcoded in
    /// tool_invocation_to_node_type so it resolves without the known_tools
    /// fallback. This catches drift between the filter constant and the
    /// mapping — add a tool to CDP_ACTION_TOOLS, forget the match arm,
    /// and this test fails.
    #[test]
    fn cdp_action_tools_all_resolve_without_known_tools() {
        let empty: Vec<serde_json::Value> = vec![];
        for name in CDP_ACTION_TOOLS {
            let result = clickweave_core::tool_mapping::tool_invocation_to_node_type(
                name,
                &serde_json::json!({}),
                &empty,
            );
            assert!(
                result.is_ok(),
                "CDP_ACTION_TOOLS entry '{}' is not hardcoded in \
                 tool_invocation_to_node_type — add a match arm in \
                 clickweave-core/src/tool_mapping.rs",
                name
            );
        }
    }
}
