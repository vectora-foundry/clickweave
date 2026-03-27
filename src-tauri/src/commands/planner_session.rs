use super::error::CommandError;
use super::types::*;
use clickweave_llm::planner::tool_use::{
    PlannerToolExecutor, ToolPermission, is_planning_tool, planning_tool_permission,
};
use clickweave_mcp::McpClient;
use serde_json::Value;
use std::sync::Arc;
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::{Mutex, oneshot};
use tracing::info;
use uuid::Uuid;

/// Managed state for the active planner session.
pub struct PlannerHandle {
    /// Channel to send the user's confirmation response.
    /// Set when a confirmation is pending; taken when the response arrives.
    pub confirmation_tx: Option<oneshot::Sender<bool>>,
    /// Session ID of the active planning session.
    pub session_id: Option<String>,
}

impl Default for PlannerHandle {
    fn default() -> Self {
        Self {
            confirmation_tx: None,
            session_id: None,
        }
    }
}

/// Holds the MCP client and Tauri app handle for a planning session.
pub struct PlannerSession {
    mcp: Arc<Mutex<McpClient>>,
    app: AppHandle,
    session_id: String,
    planner_handle: Arc<std::sync::Mutex<PlannerHandle>>,
    planning_tools_openai: Vec<Value>,
}

impl PlannerSession {
    pub async fn new(
        mcp: McpClient,
        app: AppHandle,
        planner_handle: Arc<std::sync::Mutex<PlannerHandle>>,
    ) -> Self {
        let session_id = Uuid::new_v4().to_string();
        let planning_tools_openai = Self::build_planning_tools(&mcp.tools_as_openai());
        {
            let mut handle = planner_handle.lock().unwrap();
            handle.session_id = Some(session_id.clone());
        }
        Self {
            mcp: Arc::new(Mutex::new(mcp)),
            app,
            session_id,
            planner_handle,
            planning_tools_openai,
        }
    }

    /// Build the list of available planning tools by filtering the MCP tool list.
    fn build_planning_tools(mcp_tools: &[Value]) -> Vec<Value> {
        mcp_tools
            .iter()
            .filter(|tool| {
                tool.get("function")
                    .and_then(|f| f.get("name"))
                    .and_then(|n| n.as_str())
                    .is_some_and(is_planning_tool)
            })
            .cloned()
            .collect()
    }

    /// Clean up after planning: disconnect CDP if connected.
    pub async fn cleanup(&self) {
        let mcp = self.mcp.lock().await;
        if mcp.has_tool("cdp_disconnect") {
            let _ = mcp.call_tool("cdp_disconnect", None).await;
            info!("Planning cleanup: disconnected CDP");
        }
        // Clear session from handle
        let mut handle = self.planner_handle.lock().unwrap();
        handle.session_id = None;
        handle.confirmation_tx = None;
    }
}

impl PlannerToolExecutor for PlannerSession {
    async fn call_tool(&self, name: &str, args: Value) -> anyhow::Result<String> {
        let mcp = self.mcp.lock().await;
        let result = mcp
            .call_tool(name, Some(args.clone()))
            .await
            .map_err(|e| anyhow::anyhow!("MCP tool call failed: {}", e))?;

        let text = result
            .content
            .first()
            .and_then(|c| match c {
                clickweave_mcp::ToolContent::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .unwrap_or("")
            .to_string();

        // Emit tool call event for UI logging
        let _ = self.app.emit(
            "planner://tool_call",
            PlannerToolCallPayload {
                session_id: self.session_id.clone(),
                tool_name: name.to_string(),
                args,
                result: Some(text.clone()),
            },
        );

        Ok(text)
    }

    fn permission(&self, name: &str) -> ToolPermission {
        if !is_planning_tool(name) {
            return ToolPermission::Blocked;
        }
        planning_tool_permission(name)
    }

    async fn request_confirmation(&self, message: &str, tool_name: &str) -> anyhow::Result<bool> {
        let (tx, rx) = oneshot::channel();

        // Store the sender in the handle
        {
            let mut handle = self.planner_handle.lock().unwrap();
            handle.confirmation_tx = Some(tx);
        }

        // Emit confirmation request to frontend
        let _ = self.app.emit(
            "planner://confirmation_required",
            PlannerConfirmationPayload {
                session_id: self.session_id.clone(),
                message: message.to_string(),
                tool_name: tool_name.to_string(),
            },
        );

        // Wait for the frontend to respond
        let approved = rx
            .await
            .map_err(|_| anyhow::anyhow!("Confirmation channel closed"))?;

        Ok(approved)
    }

    fn available_planning_tools(&self) -> Vec<Value> {
        self.planning_tools_openai.clone()
    }
}

#[tauri::command]
#[specta::specta]
pub async fn planner_confirmation_respond(
    app: tauri::AppHandle,
    approved: bool,
) -> Result<(), CommandError> {
    let handle = app.state::<std::sync::Mutex<PlannerHandle>>();
    let tx = {
        let mut guard = handle.lock().unwrap();
        guard.confirmation_tx.take()
    };

    if let Some(tx) = tx {
        let _ = tx.send(approved);
        Ok(())
    } else {
        Err(CommandError::validation(
            "No pending planner confirmation".to_string(),
        ))
    }
}
