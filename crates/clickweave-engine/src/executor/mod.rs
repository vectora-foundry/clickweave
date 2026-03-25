mod ai_step;
mod app_resolve;
mod control_flow;
mod deterministic;
mod element_resolve;
pub mod error;
mod graph_nav;
mod run_loop;
mod supervision;
mod trace;
mod variables;
mod verdict;

pub use error::*;

#[cfg(test)]
mod tests;

use clickweave_core::AppKind;
use clickweave_core::decision_cache::DecisionCache;
use clickweave_core::runtime::RuntimeContext;
use clickweave_core::storage::RunStorage;
use clickweave_core::{ExecutionMode, NodeRun, NodeVerdict, Workflow};
use clickweave_llm::{ChatBackend, LlmClient, LlmConfig, Message};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::future::Future;
use std::path::PathBuf;
use std::sync::RwLock;
use tokio::sync::mpsc::Sender;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

/// Minimal trait for MCP tool invocation, used to enable test stubs.
pub(crate) trait Mcp: Send + Sync {
    fn call_tool(
        &self,
        name: &str,
        arguments: Option<serde_json::Value>,
    ) -> impl Future<Output = anyhow::Result<clickweave_mcp::ToolCallResult>> + Send;
}

impl Mcp for clickweave_mcp::McpClient {
    fn call_tool(
        &self,
        name: &str,
        arguments: Option<serde_json::Value>,
    ) -> impl Future<Output = anyhow::Result<clickweave_mcp::ToolCallResult>> + Send {
        clickweave_mcp::McpClient::call_tool(self, name, arguments)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ExecutorState {
    Idle,
    Running,
}

pub enum ExecutorCommand {
    Resume,
    Skip,
    Abort,
}

/// Events sent from the executor back to the UI
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ExecutorEvent {
    Log(String),
    StateChanged(ExecutorState),
    NodeStarted(Uuid),
    NodeCompleted(Uuid),
    NodeFailed(Uuid, String),
    RunCreated(Uuid, NodeRun),
    WorkflowCompleted,
    ChecksCompleted(Vec<NodeVerdict>),
    Error(String),
    SupervisionPassed {
        node_id: Uuid,
        node_name: String,
        summary: String,
    },
    SupervisionPaused {
        node_id: Uuid,
        node_name: String,
        finding: String,
        /// Base64-encoded screenshot captured during verification, if available.
        screenshot: Option<String>,
    },
}

#[derive(Debug, Clone)]
pub(crate) struct ResolvedApp {
    pub name: String,
    pub pid: i32,
}

pub struct WorkflowExecutor<C: ChatBackend = LlmClient> {
    workflow: Workflow,
    agent: C,
    vlm: Option<C>,
    /// Planner-class LLM used for supervision verification in Test mode.
    /// Falls back to VLM, then agent if not configured.
    supervision: Option<C>,
    /// Dedicated VLM for screenshot verification: low max_tokens, thinking disabled.
    verdict_vlm: Option<LlmClient>,
    mcp_binary_path: String,
    execution_mode: ExecutionMode,
    project_path: Option<PathBuf>,
    event_tx: Sender<ExecutorEvent>,
    storage: RunStorage,
    app_cache: RwLock<HashMap<String, ResolvedApp>>,
    focused_app: RwLock<Option<(String, AppKind)>>,
    element_cache: RwLock<HashMap<(String, Option<String>), String>>,
    context: RuntimeContext,
    decision_cache: RwLock<DecisionCache>,
    /// Persistent conversation history for supervision across the entire run.
    supervision_history: RwLock<Vec<Message>>,
    /// Verdicts from Verification-role nodes, accumulated during execution.
    runtime_verdicts: Vec<NodeVerdict>,
    /// Set by eval_control_flow when a loop exits; consumed by the main loop
    /// to run a deferred visual verification after the loop completes.
    pending_loop_exit: Option<PendingLoopExit>,
    /// The app name for which a CDP connection is active (via cdp_connect).
    cdp_connected_app: Option<String>,
    cancel_token: CancellationToken,
    /// Hint from a previous supervision failure, threaded into disambiguation
    /// prompts on retry so the LLM picks a different match.
    supervision_hint: Option<String>,
    /// Native click disambiguation indices already tried during supervision retries.
    tried_click_indices: RwLock<Vec<usize>>,
    /// CDP element UIDs already tried during supervision retries.
    tried_cdp_uids: RwLock<Vec<String>>,
    /// Set when the last click was resolved and executed via CDP.
    /// CDP provides structural verification (element found in DOM by
    /// text/role/parent and click event dispatched), making VLM-based
    /// supervision redundant and error-prone for these clicks.
    last_click_was_cdp: bool,
    /// Set when the last URL-enter navigation on Chrome/CDP was observed via
    /// cdp_list_pages moving away from NTP/blank.
    /// This gives structural verification for the navigation keypress.
    last_url_navigation_was_cdp: bool,
    /// Text from the most recent TypeText node on a Chrome/CDP app when it
    /// looks like a URL (e.g. `gmail.com`, `https://...`).
    /// Arms the following `press_key return` intercept: fires the native
    /// keypress (Chrome handles Omnibox navigation), then polls
    /// `cdp_list_pages` until the URL moves away from NTP/blank so that
    /// supervision fires when Chrome is already loading the destination page.
    last_typed_url: Option<String>,
    /// Persistent Chrome user-data-dir path for `--remote-debugging-port` sessions.
    /// When set, used as `--user-data-dir` instead of a hardcoded path.
    chrome_profile_path: Option<PathBuf>,
}

pub(crate) struct PendingLoopExit {
    pub node_id: Uuid,
    pub loop_name: String,
    pub reason: LoopExitReason,
    pub iterations: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LoopExitReason {
    ConditionMet,
    MaxIterations,
}

impl LoopExitReason {
    pub fn as_str(self) -> &'static str {
        match self {
            LoopExitReason::ConditionMet => "condition_met",
            LoopExitReason::MaxIterations => "max_iterations",
        }
    }
}

impl WorkflowExecutor {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        workflow: Workflow,
        agent_config: LlmConfig,
        vlm_config: Option<LlmConfig>,
        supervision_config: Option<LlmConfig>,
        mcp_binary_path: String,
        execution_mode: ExecutionMode,
        project_path: Option<PathBuf>,
        event_tx: Sender<ExecutorEvent>,
        storage: RunStorage,
        cancel_token: CancellationToken,
        chrome_profile_path: Option<PathBuf>,
    ) -> Self {
        let decision_cache = DecisionCache::load(&storage.cache_path())
            .unwrap_or_else(|| DecisionCache::new(workflow.id));
        let verdict_vlm = vlm_config
            .as_ref()
            .or(supervision_config.as_ref())
            .map(|cfg| LlmClient::new(cfg.clone().with_max_tokens(4096).with_thinking(false)));
        Self {
            workflow,
            agent: LlmClient::new(agent_config),
            vlm: vlm_config.map(|c| LlmClient::new(c.with_thinking(false))),
            supervision: supervision_config.map(|c| LlmClient::new(c.with_thinking(false))),
            verdict_vlm,
            mcp_binary_path,
            execution_mode,
            project_path,
            event_tx,
            storage,
            app_cache: RwLock::new(HashMap::new()),
            focused_app: RwLock::new(None),
            element_cache: RwLock::new(HashMap::new()),
            context: RuntimeContext::new(),
            decision_cache: RwLock::new(decision_cache),
            supervision_history: RwLock::new(Vec::new()),
            runtime_verdicts: Vec::new(),
            pending_loop_exit: None,
            cdp_connected_app: None,
            cancel_token,
            supervision_hint: None,
            tried_click_indices: RwLock::new(Vec::new()),
            tried_cdp_uids: RwLock::new(Vec::new()),
            last_click_was_cdp: false,
            last_url_navigation_was_cdp: false,
            last_typed_url: None,
            chrome_profile_path,
        }
    }
}

impl<C: ChatBackend> WorkflowExecutor<C> {
    // ── RwLock helpers ───────────────────────────────────────────────────
    // Centralize the `.unwrap_or_else(|e| e.into_inner())` poison-recovery
    // pattern so call sites stay concise.

    pub(crate) fn read_app_cache(
        &self,
    ) -> std::sync::RwLockReadGuard<'_, HashMap<String, ResolvedApp>> {
        self.app_cache.read().unwrap_or_else(|e| e.into_inner())
    }

    pub(crate) fn write_app_cache(
        &self,
    ) -> std::sync::RwLockWriteGuard<'_, HashMap<String, ResolvedApp>> {
        self.app_cache.write().unwrap_or_else(|e| e.into_inner())
    }

    pub(crate) fn read_focused_app(
        &self,
    ) -> std::sync::RwLockReadGuard<'_, Option<(String, AppKind)>> {
        self.focused_app.read().unwrap_or_else(|e| e.into_inner())
    }

    pub(crate) fn write_focused_app(
        &self,
    ) -> std::sync::RwLockWriteGuard<'_, Option<(String, AppKind)>> {
        self.focused_app.write().unwrap_or_else(|e| e.into_inner())
    }

    pub(crate) fn read_element_cache(
        &self,
    ) -> std::sync::RwLockReadGuard<'_, HashMap<(String, Option<String>), String>> {
        self.element_cache.read().unwrap_or_else(|e| e.into_inner())
    }

    pub(crate) fn write_element_cache(
        &self,
    ) -> std::sync::RwLockWriteGuard<'_, HashMap<(String, Option<String>), String>> {
        self.element_cache
            .write()
            .unwrap_or_else(|e| e.into_inner())
    }

    pub(crate) fn read_decision_cache(&self) -> std::sync::RwLockReadGuard<'_, DecisionCache> {
        self.decision_cache
            .read()
            .unwrap_or_else(|e| e.into_inner())
    }

    pub(crate) fn write_decision_cache(&self) -> std::sync::RwLockWriteGuard<'_, DecisionCache> {
        self.decision_cache
            .write()
            .unwrap_or_else(|e| e.into_inner())
    }

    #[allow(dead_code)] // Kept for API symmetry with write_supervision_history
    pub(crate) fn read_supervision_history(&self) -> std::sync::RwLockReadGuard<'_, Vec<Message>> {
        self.supervision_history
            .read()
            .unwrap_or_else(|e| e.into_inner())
    }

    pub(crate) fn write_supervision_history(
        &self,
    ) -> std::sync::RwLockWriteGuard<'_, Vec<Message>> {
        self.supervision_history
            .write()
            .unwrap_or_else(|e| e.into_inner())
    }

    pub(crate) fn read_tried_click_indices(&self) -> std::sync::RwLockReadGuard<'_, Vec<usize>> {
        self.tried_click_indices
            .read()
            .unwrap_or_else(|e| e.into_inner())
    }

    pub(crate) fn write_tried_click_indices(&self) -> std::sync::RwLockWriteGuard<'_, Vec<usize>> {
        self.tried_click_indices
            .write()
            .unwrap_or_else(|e| e.into_inner())
    }

    pub(crate) fn read_tried_cdp_uids(&self) -> std::sync::RwLockReadGuard<'_, Vec<String>> {
        self.tried_cdp_uids
            .read()
            .unwrap_or_else(|e| e.into_inner())
    }

    pub(crate) fn write_tried_cdp_uids(&self) -> std::sync::RwLockWriteGuard<'_, Vec<String>> {
        self.tried_cdp_uids
            .write()
            .unwrap_or_else(|e| e.into_inner())
    }

    // ── Convenience accessors ────────────────────────────────────────────

    pub(crate) fn focused_app_name(&self) -> Option<String> {
        self.read_focused_app()
            .as_ref()
            .map(|(name, _)| name.clone())
    }

    /// Check whether a CDP connection is active for the currently focused app.
    pub(crate) fn cdp_connected_to_focused_app(&self) -> bool {
        let Some(app_name) = self.focused_app_name() else {
            return false;
        };
        self.cdp_connected_app.as_deref() == Some(app_name.as_str())
    }

    pub(crate) fn focused_app_kind(&self) -> AppKind {
        self.read_focused_app()
            .as_ref()
            .map(|(_, kind)| *kind)
            .unwrap_or(AppKind::Native)
    }

    /// Format a "previously tried" list as a prompt suffix for disambiguation.
    /// Returns an empty string when nothing has been tried yet.
    pub(crate) fn format_tried_context<T: std::fmt::Debug>(items: &[T], label: &str) -> String {
        if items.is_empty() {
            String::new()
        } else {
            format!("\nPreviously tried {label} that FAILED: {items:?}. Do NOT pick any of these.")
        }
    }

    /// Format the supervision hint (if any) as a prompt suffix for disambiguation.
    pub(crate) fn format_supervision_hint(&self, context: &str) -> String {
        self.supervision_hint
            .as_deref()
            .map(|hint| {
                format!(
                    "\n\nIMPORTANT: {}\
                     The supervision system reported: \"{}\"\n\
                     Pick a DIFFERENT element.",
                    context, hint
                )
            })
            .unwrap_or_default()
    }

    /// Return the best available LLM for text reasoning tasks (app resolution,
    /// element resolution). Prefers supervision (planner-class), falls back to
    /// VLM, then agent. The tiny agent model often has insufficient context for
    /// these prompts.
    pub(crate) fn reasoning_backend(&self) -> &C {
        self.supervision
            .as_ref()
            .or(self.vlm.as_ref())
            .unwrap_or(&self.agent)
    }

    /// Return the best available LLM for vision tasks (image analysis,
    /// screenshot verification). Prefers an explicitly configured VLM, falls
    /// back to supervision (planner-class). Returns `None` only when neither
    /// VLM nor planner is configured.
    pub(crate) fn vision_backend(&self) -> Option<&C> {
        self.vlm.as_ref().or(self.supervision.as_ref())
    }
}
