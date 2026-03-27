use super::error::CommandError;
use super::planner_session::{PlannerHandle, PlannerSession};
use super::types::*;
use clickweave_mcp::McpClient;
use std::sync::Arc;
use std::time::Duration;
use tauri::Manager;

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
async fn spawn_planning_mcp() -> Result<McpClient, CommandError> {
    let mcp_binary =
        crate::mcp_resolve::resolve_mcp_binary().map_err(|e| CommandError::mcp(format!("{e}")))?;
    McpClient::spawn(&mcp_binary, &[])
        .await
        .map_err(|e| CommandError::mcp(format!("Failed to spawn MCP server: {e}")))
}

#[tauri::command]
#[specta::specta]
pub async fn plan_workflow(
    app: tauri::AppHandle,
    request: PlanRequest,
) -> Result<PlanResponse, CommandError> {
    let mcp = spawn_planning_mcp().await?;
    let workflow_tools = mcp.tools_as_openai();

    let planner_handle = app.state::<Arc<std::sync::Mutex<PlannerHandle>>>();
    let session = PlannerSession::new(mcp, app.clone(), Arc::clone(&planner_handle)).await;

    let planner_config = request.planner.into_llm_config(None);
    let planner = clickweave_llm::LlmClient::new(planner_config);

    let chrome_profiles = super::chrome_profiles::get_store(&app).load_profiles();
    let profiles_ref = if chrome_profiles.len() > 1 {
        Some(chrome_profiles.as_slice())
    } else {
        None
    };

    let result = tokio::time::timeout(
        Duration::from_secs(60),
        clickweave_llm::planner::plan_workflow_with_tools(
            &planner,
            &request.intent,
            &workflow_tools,
            request.allow_ai_transforms,
            request.allow_agent_steps,
            profiles_ref,
            &session,
        ),
    )
    .await;

    // Always clean up (disconnect CDP, etc.)
    session.cleanup().await;

    let result = match result {
        Ok(inner) => inner.map_err(|e| CommandError::llm(format!("Planning failed: {}", e))),
        Err(_) => Err(CommandError::llm(
            "Planning timed out after 60 seconds".to_string(),
        )),
    }?;

    Ok(PlanResponse {
        workflow: result.workflow,
        warnings: result.warnings,
    })
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
