use super::error::CommandError;
use super::types::*;
use clickweave_llm::planner::tool_use::{
    PlannerToolExecutor, ToolPermission, is_planning_tool, planning_tool_permission,
};
use clickweave_mcp::McpClient;
use serde_json::Value;
use std::collections::HashSet;
use std::sync::Arc;
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_store::StoreExt;
use tokio::sync::{Mutex, oneshot};
use tracing::info;
use uuid::Uuid;

/// Extract the text content from an MCP tool call result.
fn extract_result_text(result: &clickweave_mcp::ToolCallResult) -> String {
    result
        .content
        .first()
        .and_then(|c| c.as_text())
        .unwrap_or("")
        .to_string()
}

/// Synthetic OpenAI function definition for `cdp_find_elements`.
/// This tool is not an MCP tool — it's intercepted by PlannerSession.
fn cdp_find_elements_tool_def() -> Value {
    serde_json::json!({
        "type": "function",
        "function": {
            "name": "cdp_find_elements",
            "description": "Search the CDP-connected page for interactive elements matching a query. Returns a compact list of matches with UID, role, label, and parent context. Only interactive elements (buttons, links, inputs, etc.) are returned. Use this to understand what's on screen and pick the right text target for cdp_click. For fill, use the UID from the results.",
            "parameters": {
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Text to search for in element labels"
                    },
                    "role": {
                        "type": "string",
                        "description": "Optional: filter results to a specific ARIA role (e.g. button, link, textbox)"
                    },
                    "max_results": {
                        "type": "integer",
                        "description": "Maximum number of results to return (default: 10)"
                    }
                },
                "required": ["query"]
            }
        }
    })
}

/// Extract the URL of the currently selected page from `cdp_list_pages` output.
///
/// Native-devtools uses `[N] url` format. A `[selected]` or `(selected)`
/// marker indicates the active page. Returns `None` if no selected marker
/// is found — the caller should not fabricate a page URL because
/// `cdp_select_page` may have changed which page is active.
fn extract_selected_page_url(list_pages_text: &str) -> Option<String> {
    for line in list_pages_text.lines() {
        let t = line.trim_start();
        if !t.starts_with('[') {
            continue;
        }
        let Some(end) = t.find(']') else {
            continue;
        };
        if !t[1..end].chars().all(|c| c.is_ascii_digit()) {
            continue;
        }
        let rest = t[end + 1..].trim();
        if !rest.contains("selected") {
            continue;
        }
        let url = rest
            .trim_end_matches("[selected]")
            .trim_end_matches("(selected)")
            .trim();
        if !url.is_empty() {
            return Some(url.to_string());
        }
    }
    None
}

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
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

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
                clickweave_llm::planner::tool_use::tool_name(tool).is_some_and(is_planning_tool)
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

    /// Get the full MCP tool list for workflow building (not just planning tools).
    pub async fn mcp_tools_openai(&self) -> Vec<Value> {
        let mcp = self.mcp.lock().await;
        mcp.tools_as_openai()
    }

    /// After cdp_connect succeeds, re-fetch the tool list from the MCP server
    /// so newly available CDP tools appear in subsequent LLM turns.
    /// Also injects the synthetic `cdp_find_elements` definition.
    pub(crate) async fn refresh_planning_tools(&self) {
        let mut mcp = self.mcp.lock().await;
        if let Err(e) = mcp.refresh_tools().await {
            tracing::warn!("Failed to refresh MCP tools after cdp_connect: {}", e);
            return;
        }
        let mut new_tools = Self::build_planning_tools(&mcp.tools_as_openai());
        // Inject synthetic cdp_find_elements (not an MCP tool).
        new_tools.push(cdp_find_elements_tool_def());
        info!(
            "Refreshed planning tools after cdp_connect: {} tools available",
            new_tools.len()
        );
        *self
            .planning_tools_openai
            .write()
            .unwrap_or_else(|e| e.into_inner()) = new_tools;
    }

    /// Handle the virtual `cdp_find_elements` tool.
    async fn handle_cdp_find_elements(&self, args: &Value) -> anyhow::Result<String> {
        use std::fmt::Write as _;

        let query = args
            .get("query")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("cdp_find_elements requires a 'query' argument"))?
            .to_string();
        let role_filter = args.get("role").and_then(|v| v.as_str()).map(String::from);
        let max_results = args
            .get("max_results")
            .and_then(|v| v.as_u64())
            .unwrap_or(10) as usize;

        // Single lock scope for both MCP calls to avoid TOCTOU between
        // page URL retrieval and snapshot capture.
        let (page_url, snapshot_text) = {
            let mcp = self.mcp.lock().await;

            let page_url = match mcp
                .call_tool("cdp_list_pages", Some(serde_json::json!({})))
                .await
            {
                Ok(result) => result
                    .content
                    .first()
                    .and_then(|c| c.as_text())
                    .and_then(extract_selected_page_url)
                    .unwrap_or_else(|| "(unknown page)".to_string()),
                Err(_) => "(unknown page)".to_string(),
            };

            let snapshot_result = mcp
                .call_tool("cdp_take_snapshot", Some(serde_json::json!({})))
                .await
                .map_err(|e| anyhow::anyhow!("cdp_take_snapshot failed: {e}"))?;
            if snapshot_result.is_error == Some(true) {
                let err = snapshot_result
                    .content
                    .first()
                    .and_then(|c| c.as_text())
                    .unwrap_or("unknown error");
                return Err(anyhow::anyhow!("cdp_take_snapshot error: {err}"));
            }
            let snapshot_text = snapshot_result
                .content
                .first()
                .and_then(|c| c.as_text())
                .unwrap_or("")
                .to_string();

            (page_url, snapshot_text)
        };

        // Build element inventory and search results from the same snapshot.
        let inventory = clickweave_core::cdp::build_element_inventory(&snapshot_text, 5);
        let result = clickweave_core::cdp::search_interactive_elements(
            &snapshot_text,
            &query,
            role_filter.as_deref(),
            max_results,
        );

        let mut output = format!("Searching on page: {page_url}\n\n");

        // Element inventory header.
        if !inventory.groups.is_empty() {
            let total: usize = inventory.groups.iter().map(|g| g.count).sum();
            // Show all labels when page is small, truncate otherwise.
            let show_all = total <= 20;

            writeln!(output, "Page elements:").unwrap();
            for g in &inventory.groups {
                let samples = if show_all {
                    g.sample_labels.join(", ")
                } else {
                    let shown: Vec<&str> =
                        g.sample_labels.iter().take(5).map(|s| s.as_str()).collect();
                    let label_text = shown.join(", ");
                    if g.count > shown.len() {
                        format!("{label_text}, ...+{} more", g.count - shown.len())
                    } else {
                        label_text
                    }
                };
                if samples.is_empty() {
                    writeln!(output, "  {} ({})", g.role, g.count).unwrap();
                } else {
                    writeln!(output, "  {} ({}): {}", g.role, g.count, samples).unwrap();
                }
            }
            output.push('\n');
        }

        // Search results.
        if result.matches.is_empty() {
            write!(output, "No interactive matches for \"{query}\".").unwrap();
        } else {
            writeln!(output, "Matches for \"{query}\":").unwrap();
            for m in &result.matches {
                write!(output, "  uid={} {} \"{}\"", m.uid, m.role, m.label).unwrap();
                if let Some(parent_role) = &m.parent_role {
                    match &m.parent_name {
                        Some(name) => write!(output, " (parent: {parent_role} \"{name}\")"),
                        None => write!(output, " (parent: {parent_role})"),
                    }
                    .unwrap();
                }
                output.push('\n');
            }
        }
        if result.omitted_count > 0 {
            write!(
                output,
                "\n~{} additional non-interactive match{} omitted.",
                result.omitted_count,
                if result.omitted_count == 1 { "" } else { "es" }
            )
            .unwrap();
        }

        Ok(output)
    }

    /// Call an MCP tool directly (for pre-gather use).
    /// Does NOT emit planner://tool_call events — pre-gather has its own eventing.
    pub async fn call_mcp_tool(
        &self,
        name: &str,
        args: Option<serde_json::Value>,
    ) -> anyhow::Result<String> {
        let mcp = self.mcp.lock().await;
        let result = mcp
            .call_tool(name, args)
            .await
            .map_err(|e| anyhow::anyhow!("MCP tool call failed: {}", e))?;
        let text = extract_result_text(&result);
        if result.is_error == Some(true) {
            return Err(anyhow::anyhow!("{}", text));
        }
        Ok(text)
    }

    /// Build an element inventory from a CDP snapshot for pre-gather injection.
    pub async fn build_pre_gather_inventory(&self) -> anyhow::Result<String> {
        let snapshot_text = self
            .call_mcp_tool("cdp_take_snapshot", Some(serde_json::json!({})))
            .await?;
        let inventory = clickweave_core::cdp::build_element_inventory(&snapshot_text, 50);

        let mut output = String::new();
        use std::fmt::Write as _;
        writeln!(output, "Interactive elements:").unwrap();
        for g in &inventory.groups {
            let labels: Vec<String> = g
                .sample_labels
                .iter()
                .map(|l| format!("\"{}\"", l))
                .collect();
            let label_text = if labels.len() > 50 {
                let shown = labels[..50].join(", ");
                format!("{}, ...+{} more", shown, labels.len() - 50)
            } else {
                labels.join(", ")
            };
            if label_text.is_empty() {
                writeln!(output, "  {} ({})", g.role, g.count).unwrap();
            } else {
                writeln!(output, "  {} ({}): {}", g.role, g.count, label_text).unwrap();
            }
        }
        Ok(output)
    }
}

impl PlannerToolExecutor for PlannerSession {
    async fn call_tool(&self, name: &str, args: Value) -> anyhow::Result<String> {
        // Virtual tools: intercepted here, not forwarded to MCP.
        if name == "cdp_find_elements" {
            let text = self.handle_cdp_find_elements(&args).await?;
            let _ = self.app.emit(
                "planner://tool_call",
                PlannerToolCallPayload {
                    session_id: self.session_id.clone(),
                    tool_name: name.to_string(),
                    args,
                    result: Some(text.clone()),
                },
            );
            return Ok(text);
        }

        let result = {
            let mcp = self.mcp.lock().await;
            mcp.call_tool(name, Some(args.clone()))
                .await
                .map_err(|e| anyhow::anyhow!("MCP tool call failed: {}", e))?
        };

        let text = extract_result_text(&result);

        let _ = self.app.emit(
            "planner://tool_call",
            PlannerToolCallPayload {
                session_id: self.session_id.clone(),
                tool_name: name.to_string(),
                args,
                result: Some(text.clone()),
            },
        );

        if name == "cdp_connect" {
            self.refresh_planning_tools().await;
        }

        Ok(text)
    }

    fn permission(&self, name: &str) -> ToolPermission {
        if !is_planning_tool(name) {
            return ToolPermission::Blocked;
        }
        let base = planning_tool_permission(name);
        if base != ToolPermission::RequiresConfirmation {
            return base;
        }
        // Re-read live from the store so "Always allow" takes effect
        // immediately (even within the same planning session). The Tauri
        // store is cached in memory, so this is a cheap HashMap lookup.
        let (allow_all, allowed) = load_tool_permissions(&self.app);
        if allow_all || allowed.contains(name) {
            return ToolPermission::Allowed;
        }
        ToolPermission::RequiresConfirmation
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

/// Read tool permissions from persisted settings.json via the Tauri store plugin.
/// Returns (allow_all, set_of_individually_allowed_tool_names).
fn load_tool_permissions(app: &AppHandle) -> (bool, HashSet<String>) {
    let store = match app.store("settings.json") {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!("Failed to open settings store for permissions: {}", e);
            return (false, HashSet::new());
        }
    };

    let perms = match store.get("toolPermissions") {
        Some(v) => v,
        None => return (false, HashSet::new()),
    };

    let allow_all = perms
        .get("allowAll")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let mut allowed = HashSet::new();
    if let Some(tools) = perms.get("tools").and_then(|v| v.as_object()) {
        for (name, level) in tools {
            if level.as_str() == Some("allow") {
                allowed.insert(name.clone());
            }
        }
    }

    info!(
        "Loaded tool permissions: allow_all={}, allowed={:?}",
        allow_all, allowed
    );
    (allow_all, allowed)
}

/// Managed state that owns the PlannerSession for the current assistant conversation.
/// One session at a time — created on first assistant turn, reused across turns,
/// cleaned up when conversation is cleared.
///
/// The session is taken out via `take_session()` during LLM calls so the lock
/// isn't held while waiting for tool confirmations. Put it back with `return_session()`.
#[derive(Default)]
pub struct AssistantSessionHandle {
    session: Option<PlannerSession>,
    pub(crate) conversation: clickweave_llm::planner::conversation::ConversationSession,
    pub(crate) assistant_config: Option<clickweave_llm::LlmConfig>,
    pub(crate) abort: Option<tokio::task::AbortHandle>,
    pub(crate) execution_locked: bool,
    pub(crate) session_in_use: bool,
    /// Snapshot of the workflow at execution start, used by the resolution listener
    /// to build the resolution system prompt.
    pub(crate) resolution_workflow: Option<clickweave_core::Workflow>,
}

impl AssistantSessionHandle {
    /// Take the session out for use during an LLM call.
    /// Returns None if no session exists yet (caller must create one first).
    pub fn take_session(&mut self) -> Option<PlannerSession> {
        self.session.take()
    }

    /// Return the session after an LLM call completes.
    pub fn return_session(&mut self, session: PlannerSession) {
        self.session = Some(session);
    }

    /// Check if a session already exists.
    pub fn has_session(&self) -> bool {
        self.session.is_some()
    }

    /// Check if the session is locked by an active execution.
    pub fn is_execution_locked(&self) -> bool {
        self.execution_locked
    }

    /// Get the current session ID (if a session exists).
    pub fn session_id(&self) -> Option<&str> {
        self.session.as_ref().map(|s| s.session_id.as_str())
    }

    pub async fn clear(&mut self) {
        if let Some(session) = self.session.take() {
            session.cleanup().await;
        }
        if let Some(abort) = self.abort.take() {
            abort.abort();
        }
        self.conversation = clickweave_llm::planner::conversation::ConversationSession::default();
        self.assistant_config = None;
        self.execution_locked = false;
        self.session_in_use = false;
        self.resolution_workflow = None;
    }
}

#[tauri::command]
#[specta::specta]
pub async fn clear_assistant_session(app: tauri::AppHandle) -> Result<(), CommandError> {
    let handle = app.state::<tokio::sync::Mutex<AssistantSessionHandle>>();
    let mut guard = handle.lock().await;
    if guard.execution_locked {
        return Err(CommandError::validation(
            "Cannot clear session during execution",
        ));
    }
    guard.clear().await;
    Ok(())
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

#[cfg(test)]
mod tests {
    use super::extract_selected_page_url;

    #[test]
    fn extract_url_native_devtools_selected() {
        let text = "[0] https://example.com\n[1] https://other.com [selected]";
        assert_eq!(
            extract_selected_page_url(text).as_deref(),
            Some("https://other.com")
        );
    }

    #[test]
    fn extract_url_native_devtools_no_selected_marker() {
        let text = "[0] https://first.com\n[1] https://second.com";
        assert!(extract_selected_page_url(text).is_none());
    }

    #[test]
    fn extract_url_empty() {
        assert!(extract_selected_page_url("").is_none());
    }

    #[test]
    fn extract_url_no_pages() {
        assert!(extract_selected_page_url("No pages found").is_none());
    }

    #[test]
    fn extract_url_electron_background_page() {
        let text =
            "[0] chrome-extension://abc/background.html\n[1] file:///app/index.html [selected]";
        assert_eq!(
            extract_selected_page_url(text).as_deref(),
            Some("file:///app/index.html")
        );
    }
}
