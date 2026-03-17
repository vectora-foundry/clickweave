use super::error::CommandError;
use super::types::*;
use clickweave_mcp::{McpRouter, default_server_configs};

pub(crate) async fn fetch_mcp_tool_schemas(
    mcp_command: &str,
) -> Result<Vec<serde_json::Value>, CommandError> {
    let configs = default_server_configs(mcp_command);
    let mut router = McpRouter::spawn(&configs)
        .await
        .map_err(|e| CommandError::mcp(format!("Failed to spawn MCP servers: {}", e)))?;
    let tools = router.tools_as_openai();
    router.kill_all();
    Ok(tools)
}

#[tauri::command]
#[specta::specta]
pub async fn plan_workflow(request: PlanRequest) -> Result<PlanResponse, CommandError> {
    let tools = fetch_mcp_tool_schemas(&request.mcp_command).await?;
    let planner_config = request.planner.into_llm_config(None);

    let result = clickweave_llm::planner::plan_workflow(
        &request.intent,
        planner_config,
        &tools,
        request.allow_ai_transforms,
        request.allow_agent_steps,
    )
    .await
    .map_err(|e| CommandError::llm(format!("Planning failed: {}", e)))?;

    Ok(PlanResponse {
        workflow: result.workflow,
        warnings: result.warnings,
    })
}

#[tauri::command]
#[specta::specta]
pub async fn patch_workflow(request: PatchRequest) -> Result<WorkflowPatch, CommandError> {
    let tools = fetch_mcp_tool_schemas(&request.mcp_command).await?;
    let planner_config = request.planner.into_llm_config(None);

    let result = clickweave_llm::planner::patch_workflow(
        &request.workflow,
        &request.user_prompt,
        planner_config,
        &tools,
        request.allow_ai_transforms,
        request.allow_agent_steps,
    )
    .await
    .map_err(|e| CommandError::llm(format!("Patching failed: {}", e)))?;

    Ok(WorkflowPatch {
        added_nodes: result.added_nodes,
        removed_node_ids: result
            .removed_node_ids
            .iter()
            .map(|id| id.to_string())
            .collect(),
        updated_nodes: result.updated_nodes,
        added_edges: result.added_edges,
        removed_edges: result.removed_edges,
        warnings: result.warnings,
    })
}
