use crate::{ChatBackend, ChatResponse, Message};
use anyhow::{Context, Result, anyhow};
use serde_json::Value;
use tracing::{debug, info, warn};

use super::tool_use::{
    MAX_BLOCKED_REJECTIONS, MAX_PLANNING_TOOL_CALLS, PlannerToolExecutor, ToolPermission,
    execute_tool,
};

/// Dummy executor type for callers that don't need tool support.
/// Used as the type parameter when passing `None` as executor.
pub struct NoExecutor;

impl PlannerToolExecutor for NoExecutor {
    async fn call_tool(&self, _name: &str, _args: Value) -> Result<String> {
        Err(anyhow!("No executor available"))
    }
    fn permission(&self, _name: &str) -> ToolPermission {
        ToolPermission::Blocked
    }
    async fn request_confirmation(&self, _message: &str, _tool_name: &str) -> Result<bool> {
        Err(anyhow!("No executor available"))
    }
    fn available_planning_tools(&self) -> Vec<Value> {
        vec![]
    }
}

/// Record of a tool call made during the conversation loop.
#[derive(Debug, Clone)]
pub struct ToolCallRecord {
    pub tool_name: String,
    pub args: Value,
    pub result: Option<String>,
    pub tool_call_id: String,
}

/// Output of the conversation loop.
pub struct ConversationOutput<T> {
    /// The parsed result from the LLM's text response.
    pub result: T,
    /// Structured log of tool calls made during this turn.
    pub tool_calls: Vec<ToolCallRecord>,
    /// Token usage from the last LLM response.
    pub usage: Option<crate::Usage>,
}

/// Unified conversation loop with tool-call, repair, and validation support.
///
/// Replaces `chat_with_repair`, `chat_with_repair_and_validate`, and
/// `plan_with_tool_use` with a single loop that handles all modes:
/// - With executor: handles tool calls (context gathering)
/// - With validate: post-parse validation with retry
/// - With on_repair: callback before each retry for UI feedback
#[allow(clippy::too_many_arguments)]
pub async fn conversation_loop<T, E: PlannerToolExecutor>(
    backend: &(impl ChatBackend + ?Sized),
    mut messages: Vec<Message>,
    executor: Option<&E>,
    mut process: impl FnMut(&str) -> Result<T>,
    mut validate: Option<impl FnMut(&T) -> Result<()>>,
    max_repairs: usize,
    on_repair: Option<&(dyn Fn(usize, usize) + Send + Sync)>,
    repair_hint: Option<&str>,
) -> Result<ConversationOutput<T>> {
    // Build tool parameters from executor
    let mut tools_param: Option<Vec<Value>> = executor
        .map(|e| e.available_planning_tools())
        .filter(|t| !t.is_empty());

    let mut total_tool_calls: usize = 0;
    let mut repair_attempts: usize = 0;
    let mut blocked_rejections: usize = 0;
    let mut tool_call_log: Vec<ToolCallRecord> = Vec::new();
    let mut last_usage: Option<crate::Usage> = None;
    let mut last_parsed: Option<T> = None;

    loop {
        let response: ChatResponse = backend
            .chat(messages.clone(), tools_param.clone())
            .await
            .context("LLM call failed")?;

        last_usage = response.usage;

        let choice = response
            .choices
            .first()
            .ok_or_else(|| anyhow!("No response from LLM"))?;

        // Handle tool calls
        if let Some(tool_calls) = &choice.message.tool_calls
            && !tool_calls.is_empty()
        {
            let executor = executor
                .ok_or_else(|| anyhow!("LLM returned tool calls but no executor was provided"))?;

            messages.push(Message::assistant_tool_calls(tool_calls.clone()));

            for tc in tool_calls {
                total_tool_calls += 1;

                if total_tool_calls > MAX_PLANNING_TOOL_CALLS {
                    warn!(
                        "Tool call budget exhausted ({} calls), forcing text output",
                        total_tool_calls
                    );
                    messages.push(Message::tool_result(
                            &tc.id,
                            "Tool call budget exhausted. Output your response now with whatever context you have.",
                        ));
                    tools_param = None;
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
                            "Tool '{}' is not available. Use only planning tools.",
                            tool_name
                        );
                        messages.push(Message::tool_result(&tc.id, &msg));

                        if blocked_rejections >= MAX_BLOCKED_REJECTIONS {
                            warn!(
                                "Too many blocked tool rejections ({}), disabling tools",
                                blocked_rejections
                            );
                            tools_param = None;
                        }
                        continue;
                    }
                    ToolPermission::RequiresConfirmation => {
                        let confirm_msg = format!(
                            "The assistant wants to call '{}'. This will affect the running app.",
                            tool_name
                        );
                        match executor.request_confirmation(&confirm_msg, tool_name).await {
                            Ok(true) => {
                                info!("User approved planning tool: {}", tool_name);
                            }
                            Ok(false) => {
                                info!("User declined planning tool: {}", tool_name);
                                messages.push(Message::tool_result(
                                    &tc.id,
                                    "User declined. Proceed without this tool.",
                                ));
                                continue;
                            }
                            Err(e) => {
                                warn!("Confirmation request failed: {}", e);
                                messages.push(Message::tool_result(
                                    &tc.id,
                                    "Confirmation unavailable. Proceed without this tool.",
                                ));
                                continue;
                            }
                        }
                    }
                    ToolPermission::Allowed => {
                        debug!("Executing planning tool: {}", tool_name);
                    }
                }

                // Execute tool and record result (shared by Allowed and approved Confirmation)
                let msg = execute_tool(executor, tool_name, args.clone(), &tc.id).await;
                let result_text = msg.text_content().map(|s| s.to_string());
                if let Some(ref text) = result_text {
                    debug!(
                        tool = %tool_name,
                        result = %&text[..text.len().min(500)],
                        "Planning tool result"
                    );
                }
                messages.push(msg);
                tool_call_log.push(ToolCallRecord {
                    tool_name: tool_name.to_string(),
                    args,
                    result: result_text,
                    tool_call_id: tc.id.clone(),
                });
            }
            // Refresh tool list after tool-call round (tools may have changed after cdp_connect)
            if tools_param.is_some() {
                let refreshed = executor.available_planning_tools();
                tools_param = if refreshed.is_empty() {
                    None
                } else {
                    Some(refreshed)
                };
            }
            continue;
        }

        // Text response — try to parse
        let content = choice
            .message
            .text_content()
            .ok_or_else(|| anyhow!("LLM returned no text content"))?;

        debug!(
            "LLM text response (repair attempt {}): {}",
            repair_attempts,
            &content[..content.len().min(200)]
        );
        messages.push(Message::assistant(content));

        match process(content) {
            Ok(result) => {
                // Run validation if provided
                if let Some(ref mut validate_fn) = validate {
                    match validate_fn(&result) {
                        Ok(()) => {
                            return Ok(ConversationOutput {
                                result,
                                tool_calls: tool_call_log,
                                usage: last_usage,
                            });
                        }
                        Err(e) if repair_attempts < max_repairs => {
                            repair_attempts += 1;
                            if let Some(cb) = on_repair {
                                cb(repair_attempts, max_repairs);
                            }
                            info!("Validation error (attempt {}): {}", repair_attempts, e);
                            let mut feedback = format!(
                                "Your previous output had a validation error: {}\n\nPlease fix and try again. Output ONLY the corrected JSON.",
                                e
                            );
                            if let Some(hint) = repair_hint {
                                feedback.push_str("\n\nReminder: ");
                                feedback.push_str(hint);
                            }
                            messages.push(Message::user(feedback));
                            last_parsed = Some(result);
                        }
                        Err(e) => {
                            // Validation failed on last attempt — return result anyway
                            warn!(
                                "Validation failed on final attempt, returning last parsed result: {}",
                                e
                            );
                            return Ok(ConversationOutput {
                                result,
                                tool_calls: tool_call_log,
                                usage: last_usage,
                            });
                        }
                    }
                } else {
                    // No validation — return immediately
                    return Ok(ConversationOutput {
                        result,
                        tool_calls: tool_call_log,
                        usage: last_usage,
                    });
                }
            }
            Err(e) if repair_attempts < max_repairs => {
                repair_attempts += 1;
                if let Some(cb) = on_repair {
                    cb(repair_attempts, max_repairs);
                }
                info!("Parse error (attempt {}): {}", repair_attempts, e);
                let mut feedback = format!(
                    "Your previous output had an error: {}\n\nPlease fix and try again. Output ONLY the corrected JSON.",
                    e
                );
                if let Some(hint) = repair_hint {
                    feedback.push_str("\n\nReminder: ");
                    feedback.push_str(hint);
                }
                messages.push(Message::user(feedback));
            }
            Err(e) => {
                // If we have a last parsed result from a validation failure, return it
                if let Some(result) = last_parsed {
                    warn!("Parse failed on final attempt but have earlier result, returning it");
                    return Ok(ConversationOutput {
                        result,
                        tool_calls: tool_call_log,
                        usage: last_usage,
                    });
                }
                return Err(e);
            }
        }
    }
}
