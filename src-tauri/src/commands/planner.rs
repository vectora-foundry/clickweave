use super::error::CommandError;
use super::types::*;
use clickweave_mcp::McpClient;

pub(crate) async fn fetch_mcp_tool_schemas() -> Result<Vec<serde_json::Value>, CommandError> {
    let mcp_binary =
        crate::mcp_resolve::resolve_mcp_binary().map_err(|e| CommandError::mcp(format!("{e}")))?;
    let mut client = McpClient::spawn(&mcp_binary, &[])
        .await
        .map_err(|e| CommandError::mcp(format!("Failed to spawn MCP server: {e}")))?;
    let tools = client.tools_as_openai();
    let _ = client.kill();
    Ok(tools)
}

/// Spawn a long-lived MCP client for the planning session.
pub(crate) async fn spawn_planning_mcp() -> Result<McpClient, CommandError> {
    let mcp_binary =
        crate::mcp_resolve::resolve_mcp_binary().map_err(|e| CommandError::mcp(format!("{e}")))?;
    McpClient::spawn(&mcp_binary, &[])
        .await
        .map_err(|e| CommandError::mcp(format!("Failed to spawn MCP server: {e}")))
}

#[tauri::command]
#[specta::specta]
pub async fn patch_workflow(request: PatchRequest) -> Result<WorkflowPatch, CommandError> {
    let tools = fetch_mcp_tool_schemas().await?;
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
    .map_err(|e| CommandError::llm(e.root_cause().to_string()))?;

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
