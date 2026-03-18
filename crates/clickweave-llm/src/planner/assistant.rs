use super::conversation::{ChatRole, ConversationSession};
use super::parse::extract_json;
use super::prompt::assistant_system_prompt;
use super::repair::chat_with_repair_and_validate;
use super::summarize::summarize_overflow;
use super::{PatchResult, PatcherOutput, PlannerGraphOutput, PlannerOutput};
use crate::{ChatBackend, LlmClient, LlmConfig, Message};
use anyhow::Result;
use clickweave_core::{Edge, Node, Workflow, validate_workflow};
use serde_json::Value;
use std::collections::HashSet;
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
}

/// Chat with the assistant, creating an LlmClient from config.
#[allow(clippy::too_many_arguments)]
pub async fn assistant_chat(
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
    )
    .await
}

/// Chat with the assistant using a given ChatBackend (for testability).
#[allow(clippy::too_many_arguments)]
pub async fn assistant_chat_with_backend(
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
    let system = assistant_system_prompt(
        workflow,
        mcp_tools,
        allow_ai_transforms,
        allow_agent_steps,
        run_context_text,
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
        };
        messages.push(msg);
    }

    // Add the new user message
    messages.push(Message::user(user_message));

    // 4. Call the LLM with validation retry loop
    //
    // When max_repair_attempts == 0 we skip validation entirely (1 LLM call, no-op validate).
    // Otherwise max_repair_attempts is the total number of LLM calls allowed.
    let wf = workflow.clone();
    let validate_patch = {
        let wf = wf.clone();
        move |result: &(String, Option<PatchResult>, Vec<String>)| -> Result<()> {
            if let Some(ref p) = result.1 {
                let candidate = merge_patch_into_workflow(&wf, p);
                validate_workflow(&candidate)?;
            }
            Ok(())
        }
    };

    let effective_max = if max_repair_attempts == 0 {
        1
    } else {
        max_repair_attempts
    };
    let noop_validate = |_: &(String, Option<PatchResult>, Vec<String>)| -> Result<()> { Ok(()) };

    // Build closures for parse and on_repair
    let process = {
        let wf = wf.clone();
        let mcp_tools = mcp_tools.to_vec();
        move |content: &str| -> Result<(String, Option<PatchResult>, Vec<String>)> {
            let (message, patch, warnings) = parse_assistant_response(
                content,
                &wf,
                &mcp_tools,
                allow_ai_transforms,
                allow_agent_steps,
            );
            Ok((message, patch, warnings))
        }
    };

    let on_repair_fn = |attempt: usize, max: usize| {
        if let Some(cb) = &on_repair_attempt {
            cb(attempt, max);
        }
    };

    static REPAIR_HINT: &str = "\
        Reminder: EndLoop must have exactly 1 outgoing edge that points BACK to its paired Loop node \
        (regular edge, no output label). The last body step flows into EndLoop, and EndLoop flows back to Loop. \
        EndLoop never has a forward edge to post-loop nodes — Loop's LoopDone edge handles the exit path.";

    let (message, patch, warnings) = if max_repair_attempts == 0 {
        chat_with_repair_and_validate(
            backend,
            "Assistant",
            messages,
            effective_max,
            process,
            noop_validate,
            on_repair_fn,
            None,
        )
        .await?
    } else {
        chat_with_repair_and_validate(
            backend,
            "Assistant",
            messages,
            effective_max,
            process,
            validate_patch,
            on_repair_fn,
            Some(REPAIR_HINT),
        )
        .await?
    };

    info!(
        has_patch = patch.is_some(),
        warnings = warnings.len(),
        "Assistant response processed"
    );

    Ok(AssistantResult {
        message,
        patch,
        new_summary,
        warnings,
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
) -> (String, Option<PatchResult>, Vec<String>) {
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
                return (content.to_string(), None, Vec::new());
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
            return (message, Some(patch), warnings);
        }
    }

    // Try parsing as graph-format planner output (for control-flow plans)
    if let Ok(graph) = serde_json::from_str::<PlannerGraphOutput>(json_str)
        && !graph.nodes.is_empty()
    {
        let prose = extract_prose(content);
        let patch = super::build_graph_plan_as_patch(
            &graph,
            mcp_tools,
            allow_ai_transforms,
            allow_agent_steps,
        );
        let message = prose.unwrap_or_else(|| describe_patch(&patch));
        let warnings = patch.warnings.clone();
        return (message, Some(patch), warnings);
    }

    // Try parsing as flat PlannerOutput (for simple linear plans)
    if let Ok(output) = serde_json::from_str::<PlannerOutput>(json_str)
        && !output.steps.is_empty()
    {
        let prose = extract_prose(content);
        let patch = super::build_plan_as_patch(
            &output.steps,
            mcp_tools,
            allow_ai_transforms,
            allow_agent_steps,
        );
        let message = prose.unwrap_or_else(|| describe_patch(&patch));
        let warnings = patch.warnings.clone();
        return (message, Some(patch), warnings);
    }

    // Conversational response
    (content.to_string(), None, Vec::new())
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

/// Simulate merging a patch into a workflow to produce a candidate for validation.
///
/// Mirrors the frontend's `applyPendingPatch` logic: remove nodes, apply updates,
/// add new nodes, remove edges, add new edges.
fn merge_patch_into_workflow(workflow: &Workflow, patch: &PatchResult) -> Workflow {
    let removed_ids: HashSet<_> = patch.removed_node_ids.iter().collect();

    let nodes: Vec<Node> = workflow
        .nodes
        .iter()
        .filter(|n| !removed_ids.contains(&n.id))
        .map(|n| {
            patch
                .updated_nodes
                .iter()
                .find(|u| u.id == n.id)
                .cloned()
                .unwrap_or_else(|| n.clone())
        })
        .chain(patch.added_nodes.iter().cloned())
        .collect();

    let edges: Vec<Edge> = workflow
        .edges
        .iter()
        .filter(|e| {
            !patch
                .removed_edges
                .iter()
                .any(|r| e.from == r.from && e.to == r.to && e.output == r.output)
        })
        .cloned()
        .chain(patch.added_edges.iter().cloned())
        .collect();

    Workflow {
        id: workflow.id,
        name: workflow.name.clone(),
        nodes,
        edges,
        groups: workflow.groups.clone(),
    }
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
