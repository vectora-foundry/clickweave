use super::conversation_loop::{NoExecutor, conversation_loop};
use super::parse::extract_json;
use super::prompt::patcher_system_prompt;
use super::{PatchResult, PatcherOutput};
use crate::{ChatBackend, LlmClient, LlmConfig, Message};
use anyhow::{Context, Result};
use clickweave_core::Workflow;
use serde_json::Value;
use tracing::info;

/// Patch an existing workflow using the planner LLM.
pub async fn patch_workflow(
    workflow: &Workflow,
    user_prompt: &str,
    planner_config: LlmConfig,
    mcp_tools_openai: &[Value],
    allow_ai_transforms: bool,
    allow_agent_steps: bool,
) -> Result<PatchResult> {
    let planner = LlmClient::new(planner_config);
    patch_workflow_with_backend(
        &planner,
        workflow,
        user_prompt,
        mcp_tools_openai,
        allow_ai_transforms,
        allow_agent_steps,
    )
    .await
}

/// Patch a workflow using a given ChatBackend (for testability).
pub async fn patch_workflow_with_backend(
    backend: &impl ChatBackend,
    workflow: &Workflow,
    user_prompt: &str,
    mcp_tools_openai: &[Value],
    allow_ai_transforms: bool,
    allow_agent_steps: bool,
) -> Result<PatchResult> {
    let system = patcher_system_prompt(
        workflow,
        mcp_tools_openai,
        allow_ai_transforms,
        allow_agent_steps,
        false, // has_planning_tools
    );
    let user_msg = format!("Modify the workflow: {}", user_prompt);

    info!("Patching workflow for prompt: {}", user_prompt);

    let messages = vec![Message::system(&system), Message::user(&user_msg)];

    let output = conversation_loop(
        backend,
        messages,
        None::<&NoExecutor>,
        |content| {
            let json_str = extract_json(content);
            serde_json::from_str::<PatcherOutput>(json_str)
                .context("Failed to parse patcher output as JSON")
        },
        None::<fn(&PatcherOutput) -> Result<()>>,
        1, // 1 repair attempt
        None,
        None,
    )
    .await?;

    let patch = super::build_patch_from_output(
        &output.result,
        workflow,
        mcp_tools_openai,
        allow_ai_transforms,
        allow_agent_steps,
    );

    info!(
        "Patch: +{} nodes, -{} nodes, ~{} nodes, +{} edges, -{} edges, {} warnings",
        patch.added_nodes.len(),
        patch.removed_node_ids.len(),
        patch.updated_nodes.len(),
        patch.added_edges.len(),
        patch.removed_edges.len(),
        patch.warnings.len(),
    );

    Ok(patch)
}
