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
#[derive(Default)]
pub struct PlannerHandle {
    /// Channel to send the user's confirmation response.
    /// Set when a confirmation is pending; taken when the response arrives.
    pub confirmation_tx: Option<oneshot::Sender<bool>>,
    /// Session ID of the active planning session.
    pub session_id: Option<String>,
}

/// Helper to lock the planner handle, recovering from poisoning.
fn lock_handle(
    handle: &std::sync::Mutex<PlannerHandle>,
) -> std::sync::MutexGuard<'_, PlannerHandle> {
    handle.lock().unwrap_or_else(|e| e.into_inner())
}

/// Holds the MCP client and Tauri app handle for a planning session.
pub struct PlannerSession {
    mcp: Arc<Mutex<McpClient>>,
    app: AppHandle,
    session_id: String,
    planner_handle: Arc<std::sync::Mutex<PlannerHandle>>,
    /// Planning tools available to the LLM, updated after cdp_connect.
    planning_tools_openai: std::sync::RwLock<Vec<Value>>,
}

impl PlannerSession {
    /// Create a new planning session. Returns an error if a session is already active.
    pub async fn try_new(
        mcp: McpClient,
        app: AppHandle,
        planner_handle: Arc<std::sync::Mutex<PlannerHandle>>,
        mcp_tools_openai: &[Value],
    ) -> Result<Self, CommandError> {
        let session_id = Uuid::new_v4().to_string();
        let planning_tools_openai = Self::build_planning_tools(mcp_tools_openai);
        {
            let mut handle = lock_handle(&planner_handle);
            if handle.session_id.is_some() {
                return Err(CommandError::validation(
                    "A planning session is already active",
                ));
            }
            handle.session_id = Some(session_id.clone());
        }
        Ok(Self {
            mcp: Arc::new(Mutex::new(mcp)),
            app,
            session_id,
            planner_handle,
            planning_tools_openai: std::sync::RwLock::new(planning_tools_openai),
        })
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

    /// Clean up after planning: disconnect CDP if connected, clear handle state,
    /// and notify the frontend to clear its planner UI.
    pub async fn cleanup(&self) {
        let mcp = self.mcp.lock().await;
        if mcp.has_tool("cdp_disconnect") {
            let _ = mcp.call_tool("cdp_disconnect", None).await;
            info!("Planning cleanup: disconnected CDP");
        }
        // Clear session from handle
        let mut handle = lock_handle(&self.planner_handle);
        handle.session_id = None;
        handle.confirmation_tx = None;

        // Notify frontend to clear planner state (dismisses stale confirmation dialogs)
        let _ = self.app.emit(
            "planner://session_ended",
            serde_json::json!({ "session_id": self.session_id }),
        );
    }

    /// After cdp_connect succeeds, re-fetch the tool list from the MCP server
    /// so newly available CDP tools appear in subsequent LLM turns.
    async fn refresh_planning_tools(&self) {
        let mut mcp = self.mcp.lock().await;
        if let Err(e) = mcp.refresh_tools().await {
            tracing::warn!("Failed to refresh MCP tools after cdp_connect: {}", e);
            return;
        }
        let new_tools = Self::build_planning_tools(&mcp.tools_as_openai());
        info!(
            "Refreshed planning tools after cdp_connect: {} tools available",
            new_tools.len()
        );
        *self
            .planning_tools_openai
            .write()
            .unwrap_or_else(|e| e.into_inner()) = new_tools;
    }
}

impl PlannerToolExecutor for PlannerSession {
    async fn call_tool(&self, name: &str, args: Value) -> anyhow::Result<String> {
        let result = {
            let mcp = self.mcp.lock().await;
            mcp.call_tool(name, Some(args.clone()))
                .await
                .map_err(|e| anyhow::anyhow!("MCP tool call failed: {}", e))?
        };

        let text = result
            .content
            .first()
            .and_then(|c| c.as_text())
            .unwrap_or("")
            .to_string();

        let _ = self.app.emit(
            "planner://tool_call",
            PlannerToolCallPayload {
                session_id: self.session_id.clone(),
                tool_name: name.to_string(),
                args,
                result: Some(text.clone()),
            },
        );

        // After cdp_connect, the MCP server exposes new CDP inspection tools.
        // Re-fetch so subsequent LLM turns can use them.
        if name == "cdp_connect" {
            self.refresh_planning_tools().await;
        }

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

        {
            let mut handle = lock_handle(&self.planner_handle);
            handle.confirmation_tx = Some(tx);
        }

        let _ = self.app.emit(
            "planner://confirmation_required",
            PlannerConfirmationPayload {
                session_id: self.session_id.clone(),
                message: message.to_string(),
                tool_name: tool_name.to_string(),
            },
        );

        let approved = rx
            .await
            .map_err(|_| anyhow::anyhow!("Confirmation channel closed"))?;

        Ok(approved)
    }

    fn has_planning_tools(&self) -> bool {
        !self
            .planning_tools_openai
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .is_empty()
    }

    fn available_planning_tools(&self) -> Vec<Value> {
        self.planning_tools_openai
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
}

#[tauri::command]
#[specta::specta]
pub async fn planner_confirmation_respond(
    app: tauri::AppHandle,
    approved: bool,
) -> Result<(), CommandError> {
    let handle = app.state::<std::sync::Arc<std::sync::Mutex<PlannerHandle>>>();
    let tx = {
        let mut guard = lock_handle(&handle);
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
