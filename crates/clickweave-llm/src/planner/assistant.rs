use super::conversation::{ChatEntry, ChatRole, ConversationSession};
use super::conversation_loop::conversation_loop;
use super::parse::extract_json;
use super::prompt::assistant_system_prompt;
use super::summarize::summarize_overflow;
use super::tool_use::PlannerToolExecutor;
use super::{PatchResult, PatcherOutput, PlannerGraphOutput, PlannerOutput};
use crate::{ChatBackend, LlmClient, LlmConfig, Message};
use anyhow::Result;
use clickweave_core::{Workflow, chrome_profiles::ChromeProfile, validate_workflow};
use serde_json::Value;
use tracing::{info, warn};

/// Result of an assistant chat turn.
pub struct AssistantResult {
    /// Natural language response.
    pub message: String,
    /// Workflow changes, if any.
    pub patch: Option<PatchResult>,
    /// Updated summary if summarization was triggered.
    pub new_summary: Option<String>,
    /// Warnings from step processing.
    pub warnings: Vec<String>,
    /// Tool call/result entries made during this turn.
    pub tool_entries: Vec<ChatEntry>,
    /// Raw prompt token count from the LLM response.
    pub prompt_tokens: Option<u32>,
    /// Refined intent extracted from a new plan, if any.
    pub intent: Option<String>,
}

/// Chat with the assistant, creating an LlmClient from config.
#[allow(clippy::too_many_arguments)]
pub async fn assistant_chat<E: PlannerToolExecutor>(
    workflow: &Workflow,
    user_message: &str,
    session: &ConversationSession,
    run_context_text: Option<&str>,
    config: LlmConfig,
    mcp_tools: &[Value],
    allow_ai_transforms: bool,
    allow_agent_steps: bool,
    max_repair_attempts: usize,
    on_repair_attempt: Option<&(dyn Fn(usize, usize) + Send + Sync)>,
    chrome_profiles: Option<&[ChromeProfile]>,
    executor: Option<&E>,
    pre_gathered_context: Option<&str>,
    cdp_connected: bool,
) -> Result<AssistantResult> {
    let client = LlmClient::new(config);
    assistant_chat_with_backend(
        &client,
        workflow,
        user_message,
        session,
        run_context_text,
        mcp_tools,
        allow_ai_transforms,
        allow_agent_steps,
        max_repair_attempts,
        on_repair_attempt,
        chrome_profiles,
        executor,
        pre_gathered_context,
        cdp_connected,
    )
    .await
}

/// Chat with the assistant using a given ChatBackend (for testability).
#[allow(clippy::too_many_arguments)]
pub async fn assistant_chat_with_backend<E: PlannerToolExecutor>(
    backend: &impl ChatBackend,
    workflow: &Workflow,
    user_message: &str,
    session: &ConversationSession,
    run_context_text: Option<&str>,
    mcp_tools: &[Value],
    allow_ai_transforms: bool,
    allow_agent_steps: bool,
    max_repair_attempts: usize,
    on_repair_attempt: Option<&(dyn Fn(usize, usize) + Send + Sync)>,
    chrome_profiles: Option<&[ChromeProfile]>,
    executor: Option<&E>,
    pre_gathered_context: Option<&str>,
    cdp_connected: bool,
) -> Result<AssistantResult> {
    // 1. Optionally summarize overflow (non-fatal on error)
    let new_summary = if session.needs_summarization(None) {
        match summarize_overflow(backend, session, None).await {
            Ok(summary) if !summary.is_empty() => {
                info!("Summarized conversation overflow");
                Some(summary)
            }
            Ok(_) => None,
            Err(e) => {
                warn!("Summarization failed (non-fatal): {}", e);
                None
            }
        }
    } else {
        None
    };

    // 2. Build system prompt
    let has_planning_tools = executor.is_some_and(|e| e.has_planning_tools());
    let system = assistant_system_prompt(
        workflow,
        mcp_tools,
        allow_ai_transforms,
        allow_agent_steps,
        run_context_text,
        chrome_profiles,
        has_planning_tools,
        pre_gathered_context,
        cdp_connected,
    );

    // 3. Assemble messages: system + optional summary context + recent window + new user message
    let mut messages = vec![Message::system(&system)];

    // Inject summary context if available (prefer new summary, fall back to existing)
    let effective_summary = new_summary.as_deref().or(session.summary.as_deref());
    if let Some(summary) = effective_summary {
        messages.push(Message::user(format!(
            "Conversation context (summary of earlier discussion): {}",
            summary
        )));
        messages.push(Message::assistant(
            "Understood, I have the context from our earlier discussion.".to_string(),
        ));
    }

    // Add recent conversation window
    for entry in session.recent_window(None) {
        let msg = match entry.role {
            ChatRole::User => Message::user(&entry.content),
            ChatRole::Assistant => Message::assistant(&entry.content),
            ChatRole::ToolCall | ChatRole::ToolResult => continue,
        };
        messages.push(msg);
    }

    // Add the new user message
    messages.push(Message::user(user_message));

    // 4. Call the LLM via the unified conversation_loop
    //
    // When max_repair_attempts == 0 we skip validation entirely (1 LLM call, no-op validate).
    // Otherwise max_repair_attempts is the total number of LLM calls allowed.
    let wf = std::sync::Arc::new(workflow.clone());

    let effective_max = if max_repair_attempts == 0 {
        1
    } else {
        max_repair_attempts
    };

    // Build closures for parse and on_repair
    let process = {
        let wf = wf.clone();
        let mcp_tools = mcp_tools.to_vec();
        move |content: &str| -> Result<(String, Option<PatchResult>, Vec<String>, Option<String>)> {
            let (message, patch, warnings, intent) = parse_assistant_response(
                content,
                &wf,
                &mcp_tools,
                allow_ai_transforms,
                allow_agent_steps,
            );
            Ok((message, patch, warnings, intent))
        }
    };

    static REPAIR_HINT: &str = "\
        Reminder: EndLoop must have exactly 1 outgoing edge that points BACK to its paired Loop node \
        (regular edge, no output label). The last body step flows into EndLoop, and EndLoop flows back to Loop. \
        EndLoop never has a forward edge to post-loop nodes — Loop's LoopDone edge handles the exit path.";

    type ValidateFn = Box<
        dyn FnMut(&(String, Option<PatchResult>, Vec<String>, Option<String>)) -> Result<()> + Send,
    >;
    let validate_closure: Option<ValidateFn> = if max_repair_attempts > 0 {
        let wf = wf.clone();
        Some(Box::new(
            move |result: &(String, Option<PatchResult>, Vec<String>, Option<String>)| -> Result<()> {
                if let Some(ref p) = result.1 {
                    let candidate = clickweave_core::merge_patch_into_workflow(
                        &wf,
                        &p.added_nodes,
                        &p.removed_node_ids,
                        &p.updated_nodes,
                        &p.added_edges,
                        &p.removed_edges,
                    );
                    validate_workflow(&candidate)?;
                }
                Ok(())
            },
        ))
    } else {
        None
    };

    let repair_hint = if max_repair_attempts > 0 {
        Some(REPAIR_HINT)
    } else {
        None
    };

    let output = conversation_loop(
        backend,
        messages,
        executor,
        process,
        validate_closure,
        effective_max.saturating_sub(1), // max_repairs (attempts beyond the first)
        on_repair_attempt,
        repair_hint,
    )
    .await?;

    let (message, patch, warnings, intent) = output.result;

    // Build tool-call entries for the conversation history
    let mut tool_entries: Vec<ChatEntry> = Vec::new();
    for tc in &output.tool_calls {
        tool_entries.push(ChatEntry::tool_call(
            &tc.tool_name,
            &tc.tool_call_id,
            &serde_json::to_string(&tc.args).unwrap_or_default(),
        ));
        if let Some(ref result) = tc.result {
            tool_entries.push(ChatEntry::tool_result(
                &tc.tool_call_id,
                &tc.tool_name,
                result,
            ));
        }
    }

    let prompt_tokens = output.usage.as_ref().map(|u| u.prompt_tokens);

    info!(
        has_patch = patch.is_some(),
        warnings = warnings.len(),
        tool_calls = output.tool_calls.len(),
        "Assistant response processed"
    );

    Ok(AssistantResult {
        message,
        patch,
        new_summary,
        warnings,
        tool_entries,
        prompt_tokens,
        intent,
    })
}

/// Try to parse the LLM response as a patch, plan, or conversational text.
///
/// For existing workflows (non-empty), tries PatcherOutput first.
/// For empty workflows, tries PlannerOutput first.
/// Falls back to treating the whole response as conversational.
fn parse_assistant_response(
    content: &str,
    workflow: &Workflow,
    mcp_tools: &[Value],
    allow_ai_transforms: bool,
    allow_agent_steps: bool,
) -> (String, Option<PatchResult>, Vec<String>, Option<String>) {
    let json_str = extract_json(content);

    if !workflow.nodes.is_empty() {
        // Try parsing as PatcherOutput first
        if let Ok(output) = serde_json::from_str::<PatcherOutput>(json_str) {
            // If all arrays are empty, treat as conversational
            if output.add.is_empty()
                && output.add_nodes.is_empty()
                && output.add_edges.is_empty()
                && output.remove_node_ids.is_empty()
                && output.update.is_empty()
            {
                return (content.to_string(), None, Vec::new(), None);
            }
            let prose = extract_prose(content);
            let patch = super::build_patch_from_output(
                &output,
                workflow,
                mcp_tools,
                allow_ai_transforms,
                allow_agent_steps,
            );
            let message = prose.unwrap_or_else(|| describe_patch(&patch));
            let warnings = patch.warnings.clone();
            return (message, Some(patch), warnings, None);
        }
    }

    // Try parsing as graph-format planner output (for control-flow plans)
    if let Ok(graph) = serde_json::from_str::<PlannerGraphOutput>(json_str)
        && !graph.nodes.is_empty()
    {
        let intent = graph.intent.clone();
        let prose = extract_prose(content);
        let patch = super::build_graph_plan_as_patch(
            &graph,
            mcp_tools,
            allow_ai_transforms,
            allow_agent_steps,
        );
        let message = prose.unwrap_or_else(|| describe_patch(&patch));
        let warnings = patch.warnings.clone();
        return (message, Some(patch), warnings, intent);
    }

    // Try parsing as flat PlannerOutput (for simple linear plans)
    if let Ok(output) = serde_json::from_str::<PlannerOutput>(json_str)
        && !output.steps.is_empty()
    {
        let intent = output.intent.clone();
        let prose = extract_prose(content);
        let patch = super::build_plan_as_patch(
            &output.steps,
            mcp_tools,
            allow_ai_transforms,
            allow_agent_steps,
        );
        let message = prose.unwrap_or_else(|| describe_patch(&patch));
        let warnings = patch.warnings.clone();
        return (message, Some(patch), warnings, intent);
    }

    // Conversational response
    (content.to_string(), None, Vec::new(), None)
}

/// Extract prose text before a JSON block, if any.
fn extract_prose(content: &str) -> Option<String> {
    // Check if there's text before a JSON object or code fence
    let trimmed = content.trim();

    // Look for start of JSON block
    let json_start = trimmed.find("```").or_else(|| {
        // Find the first `{` that starts a JSON object
        trimmed.find('{').filter(|&pos| pos > 0)
    });

    if let Some(pos) = json_start {
        let prose = trimmed[..pos].trim();
        if !prose.is_empty() {
            return Some(prose.to_string());
        }
    }

    None
}

/// Chat for resolution queries — accepts a pre-built Message (can include images).
/// Uses the resolution system prompt instead of the assistant system prompt.
/// No planning tools, include_tool_results is always true.
pub async fn resolution_chat_with_backend(
    backend: &impl ChatBackend,
    workflow: &Workflow,
    query_message: Message,
    session: &ConversationSession,
    mcp_tools: &[Value],
) -> Result<AssistantResult> {
    let system = super::resolution::resolution_system_prompt(workflow);

    let mut messages = vec![Message::system(&system)];

    // Summary context
    if let Some(summary) = &session.summary {
        messages.push(Message::user(format!("Conversation context: {}", summary)));
        messages.push(Message::assistant(
            "Understood, I have the context.".to_string(),
        ));
    }

    // Recent window WITH tool results (planning exploration context)
    for entry in session.recent_window(None) {
        let msg = match entry.role {
            ChatRole::User => Message::user(&entry.content),
            ChatRole::Assistant => Message::assistant(&entry.content),
            ChatRole::ToolCall => Message::user(format!(
                "[Tool Call: {}] {}",
                entry.tool_name.as_deref().unwrap_or("?"),
                &entry.content
            )),
            ChatRole::ToolResult => Message::user(format!(
                "[Tool Result: {}] {}",
                entry.tool_name.as_deref().unwrap_or("?"),
                &entry.content
            )),
        };
        messages.push(msg);
    }

    // The resolution query message (may include screenshot image part)
    messages.push(query_message);

    // Single LLM call — no planning tools, no conversation loop
    let response = backend.chat(messages, None).await?;
    let choice = response
        .choices
        .first()
        .ok_or_else(|| anyhow::anyhow!("No response from LLM"))?;
    let content = choice.message.content_text().unwrap_or_default();

    let (message, patch, warnings, _intent) =
        parse_assistant_response(content, workflow, mcp_tools, false, false);

    let prompt_tokens = response.usage.as_ref().map(|u| u.prompt_tokens);

    Ok(AssistantResult {
        message: message.to_string(),
        patch,
        new_summary: None,
        warnings,
        tool_entries: Vec::new(),
        prompt_tokens,
        intent: None,
    })
}

/// Generate a default description of what a patch does.
fn describe_patch(patch: &PatchResult) -> String {
    let mut parts = Vec::new();
    let pl = |n: usize, verb: &str| {
        if n == 1 {
            format!("{} 1 node", verb)
        } else {
            format!("{} {} nodes", verb, n)
        }
    };
    if !patch.added_nodes.is_empty() {
        parts.push(pl(patch.added_nodes.len(), "Added"));
    }
    if !patch.removed_node_ids.is_empty() {
        parts.push(pl(patch.removed_node_ids.len(), "Removed"));
    }
    if !patch.updated_nodes.is_empty() {
        parts.push(pl(patch.updated_nodes.len(), "Updated"));
    }
    if parts.is_empty() {
        "No workflow changes.".to_string()
    } else {
        format!("{}.", parts.join(", "))
    }
}
