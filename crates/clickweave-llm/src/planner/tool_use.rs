use crate::{ChatBackend, ChatResponse, Message};
use anyhow::{Context, Result, anyhow};
use serde_json::Value;
use std::future::Future;
use tracing::{debug, info, warn};

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
pub const MAX_REPAIR_ATTEMPTS: usize = 3;
pub const MAX_BLOCKED_REJECTIONS: usize = 3;

/// Execute a planning tool and return a tool_result message.
pub(crate) async fn execute_tool<E: PlannerToolExecutor>(
    executor: &E,
    name: &str,
    args: Value,
    tc_id: &str,
) -> Message {
    match executor.call_tool(name, args).await {
        Ok(result) => Message::tool_result(tc_id, &result),
        Err(e) => Message::tool_result(tc_id, format!("Tool call failed: {}", e)),
    }
}

/// Run the planner conversation loop with tool-call support.
///
/// The loop alternates between tool-call rounds (context gathering) and
/// text rounds (workflow JSON output). Hard limits prevent infinite loops.
pub async fn plan_with_tool_use<E: PlannerToolExecutor>(
    backend: &impl ChatBackend,
    mut messages: Vec<Message>,
    executor: &E,
    mut process: impl FnMut(&str) -> Result<super::PlanResult>,
) -> Result<super::PlanResult> {
    let planning_tools = executor.available_planning_tools();
    let mut tools_param: Option<Vec<Value>> = if planning_tools.is_empty() {
        None
    } else {
        Some(planning_tools)
    };

    let mut total_tool_calls: usize = 0;
    let mut repair_attempts: usize = 0;
    let mut blocked_rejections: usize = 0;

    loop {
        let response: ChatResponse = backend
            .chat(messages.clone(), tools_param.clone())
            .await
            .context("Planner LLM call failed")?;

        let choice = response
            .choices
            .first()
            .ok_or_else(|| anyhow!("No response from planner"))?;

        if let Some(tool_calls) = &choice.message.tool_calls
            && !tool_calls.is_empty()
        {
            // Append the assistant's tool-call message to preserve transcript
            messages.push(Message::assistant_tool_calls(tool_calls.clone()));

            for tc in tool_calls {
                total_tool_calls += 1;

                if total_tool_calls > MAX_PLANNING_TOOL_CALLS {
                    warn!(
                        "Planning tool call budget exhausted ({} calls), forcing text output",
                        total_tool_calls
                    );
                    messages.push(Message::tool_result(
                            &tc.id,
                            "Tool call budget exhausted. Output the workflow JSON now with whatever context you have.",
                        ));
                    tools_param = None; // Force text response on next turn
                    continue;
                }

                let tool_name = &tc.function.name;
                let args: Value = serde_json::from_str(&tc.function.arguments)
                    .unwrap_or(Value::Object(Default::default()));

                let permission = executor.permission(tool_name);

                match permission {
                    ToolPermission::Blocked => {
                        blocked_rejections += 1;
                        let msg = format!(
                            "Tool '{}' is not available during planning. Use only planning tools.",
                            tool_name
                        );
                        messages.push(Message::tool_result(&tc.id, &msg));

                        if blocked_rejections >= MAX_BLOCKED_REJECTIONS {
                            warn!(
                                "Too many blocked tool rejections ({}), removing planning tools",
                                blocked_rejections
                            );
                            tools_param = None;
                        }
                    }
                    ToolPermission::RequiresConfirmation => {
                        let confirm_msg = format!(
                            "The planner wants to call '{}'. This will affect the running app.",
                            tool_name
                        );
                        match executor.request_confirmation(&confirm_msg, tool_name).await {
                            Ok(true) => {
                                info!("User approved planning tool: {}", tool_name);
                                messages
                                    .push(execute_tool(executor, tool_name, args, &tc.id).await);
                            }
                            Ok(false) => {
                                info!("User declined planning tool: {}", tool_name);
                                messages.push(Message::tool_result(
                                    &tc.id,
                                    "User declined. Proceed without this tool.",
                                ));
                            }
                            Err(e) => {
                                warn!("Confirmation request failed: {}", e);
                                messages.push(Message::tool_result(
                                    &tc.id,
                                    "Confirmation unavailable. Proceed without this tool.",
                                ));
                            }
                        }
                    }
                    ToolPermission::Allowed => {
                        debug!("Executing planning tool: {}", tool_name);
                        messages.push(execute_tool(executor, tool_name, args, &tc.id).await);
                    }
                }
            }
            // Refresh the tool list — tools may have changed after cdp_connect.
            if tools_param.is_some() {
                let refreshed = executor.available_planning_tools();
                tools_param = if refreshed.is_empty() {
                    None
                } else {
                    Some(refreshed)
                };
            }
            continue; // Next LLM turn
        }

        let content = choice
            .message
            .text_content()
            .ok_or_else(|| anyhow!("Planner returned no text content"))?;

        debug!(
            "Planner text output (repair attempt {}): {}",
            repair_attempts, content
        );
        messages.push(Message::assistant(content));

        match process(content) {
            Ok(result) => return Ok(result),
            Err(e) if repair_attempts < MAX_REPAIR_ATTEMPTS => {
                repair_attempts += 1;
                info!("Planner parse error (attempt {}): {}", repair_attempts, e);
                messages.push(Message::user(format!(
                    "Your previous output had an error: {}\n\nPlease fix the JSON and try again. Output ONLY the corrected JSON object.",
                    e
                )));
            }
            Err(e) => return Err(e),
        }
    }
}
