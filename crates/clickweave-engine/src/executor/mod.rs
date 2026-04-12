mod action_verification;
mod ai_step;
mod app_resolve;
mod cdp_wait;
pub(crate) mod deterministic;
mod element_resolve;
pub mod error;
mod find_app;
mod graph_nav;
pub(crate) mod retry_context;
mod run_loop;
mod supervision;
mod trace;
mod variables;
mod verdict;

pub use error::*;

#[cfg(test)]
mod tests;

use clickweave_core::AppKind;
use clickweave_core::chrome_profiles::{ChromeProfile, ChromeProfileStore};
use clickweave_core::decision_cache::DecisionCache;
use clickweave_core::runtime::RuntimeContext;
use clickweave_core::storage::RunStorage;
use clickweave_core::{ExecutionMode, NodeRun, NodeVerdict, RuntimeResolution, Workflow};
use clickweave_llm::{ChatBackend, LlmClient, LlmConfig};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::path::PathBuf;
use std::sync::RwLock;
use tokio::sync::mpsc::Sender;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

/// Query sent from executor to Tauri layer when resolution fails.
/// Lives in engine crate (not core) because it carries a tokio channel sender.
pub struct RuntimeQuery {
    pub node_id: Uuid,
    pub node_name: String,
    pub action_description: String,
    pub target: String,
    pub screenshot: Option<String>,
    pub element_inventory: String,
    pub current_node_id: Uuid,
    pub completed_node_ids: Vec<Uuid>,
    pub response_tx: tokio::sync::oneshot::Sender<RuntimeResolution>,
}

/// Trait abstracting MCP tool operations, used to enable test stubs.
pub(crate) trait Mcp: Send + Sync {
    fn call_tool(
        &self,
        name: &str,
        arguments: Option<serde_json::Value>,
    ) -> impl Future<Output = anyhow::Result<clickweave_mcp::ToolCallResult>> + Send;

    /// Check whether a tool with the given name is available.
    fn has_tool(&self, name: &str) -> bool;

    /// Convert available tools to the OpenAI-compatible function-call format.
    fn tools_as_openai(&self) -> Vec<serde_json::Value>;

    /// Re-fetch the tool list from the MCP server. Call after state-changing
    /// operations that expose new tools (e.g. `cdp_connect`).
    fn refresh_tools(&self) -> impl Future<Output = anyhow::Result<()>> + Send;
}

impl Mcp for clickweave_mcp::McpClient {
    fn call_tool(
        &self,
        name: &str,
        arguments: Option<serde_json::Value>,
    ) -> impl Future<Output = anyhow::Result<clickweave_mcp::ToolCallResult>> + Send {
        clickweave_mcp::McpClient::call_tool(self, name, arguments)
    }

    fn has_tool(&self, name: &str) -> bool {
        clickweave_mcp::McpClient::has_tool(self, name)
    }

    fn tools_as_openai(&self) -> Vec<serde_json::Value> {
        clickweave_mcp::McpClient::tools_as_openai(self)
    }

    fn refresh_tools(&self) -> impl Future<Output = anyhow::Result<()>> + Send {
        clickweave_mcp::McpClient::refresh_tools(self)
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
    NodeCancelled(Uuid),
}

#[derive(Debug, Clone)]
pub(crate) struct ResolvedApp {
    pub name: String,
    pub pid: i32,
}

pub struct WorkflowExecutor<C: ChatBackend = LlmClient> {
    workflow: Workflow,
    agent: C,
    fast: Option<C>,
    /// Supervisor LLM used for step verdict verification in Test mode.
    /// Falls back to fast model, then agent if not configured.
    supervision: Option<C>,
    /// Dedicated fast model for screenshot verification: low max_tokens, thinking disabled.
    verdict_fast: Option<LlmClient>,
    mcp_binary_path: String,
    execution_mode: ExecutionMode,
    project_path: Option<PathBuf>,
    event_tx: Sender<ExecutorEvent>,
    storage: RunStorage,
    app_cache: RwLock<HashMap<String, ResolvedApp>>,
    focused_app: RwLock<Option<(String, AppKind, i32)>>,
    element_cache: RwLock<HashMap<(String, Option<String>), String>>,
    context: RuntimeContext,
    decision_cache: RwLock<DecisionCache>,
    /// The app name and PID for which a CDP connection is active (via cdp_connect).
    /// PID is used to distinguish same-name app instances within a single execution.
    cdp_connected_app: Option<(String, i32)>,
    cancel_token: CancellationToken,
    /// Store for Chrome user-data-dir profiles (resolves profile names to paths).
    chrome_profile_store: ChromeProfileStore,
    /// Cached profile list, loaded once at construction.
    chrome_profiles: Vec<ChromeProfile>,
    /// Channel to send resolution queries to the Tauri listener (Test mode only).
    resolution_tx: Option<tokio::sync::mpsc::Sender<RuntimeQuery>>,
    /// Delay (ms) before capturing the per-step supervision screenshot.
    supervision_delay_ms: u64,
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
        fast_config: Option<LlmConfig>,
        supervision_config: Option<LlmConfig>,
        mcp_binary_path: String,
        execution_mode: ExecutionMode,
        project_path: Option<PathBuf>,
        event_tx: Sender<ExecutorEvent>,
        storage: RunStorage,
        cancel_token: CancellationToken,
        chrome_profiles_dir: PathBuf,
        resolution_tx: Option<tokio::sync::mpsc::Sender<RuntimeQuery>>,
        supervision_delay_ms: u64,
    ) -> Self {
        let chrome_profile_store = ChromeProfileStore::new(chrome_profiles_dir);
        let chrome_profiles = chrome_profile_store.ensure_profiles().unwrap_or_else(|e| {
            tracing::warn!("Chrome profile setup failed (non-fatal): {e}");
            chrome_profile_store.load_profiles()
        });
        let decision_cache = DecisionCache::load(&storage.cache_path(), workflow.id)
            .unwrap_or_else(|| DecisionCache::new(workflow.id));
        let verdict_fast = fast_config
            .as_ref()
            .or(supervision_config.as_ref())
            .map(|cfg| LlmClient::new(cfg.clone().with_max_tokens(4096).with_thinking(false)));
        Self {
            workflow,
            agent: LlmClient::new(agent_config.with_thinking(true)),
            fast: fast_config.map(|c| LlmClient::new(c.with_thinking(false))),
            supervision: supervision_config.map(|c| LlmClient::new(c.with_thinking(false))),
            verdict_fast,
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
            cdp_connected_app: None,
            cancel_token,
            chrome_profile_store,
            chrome_profiles,
            resolution_tx,
            supervision_delay_ms,
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
    ) -> std::sync::RwLockReadGuard<'_, Option<(String, AppKind, i32)>> {
        self.focused_app.read().unwrap_or_else(|e| e.into_inner())
    }

    pub(crate) fn write_focused_app(
        &self,
    ) -> std::sync::RwLockWriteGuard<'_, Option<(String, AppKind, i32)>> {
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

    // ── Chrome profile resolution ────────────────────────────────────────

    /// Resolve a chrome_profile name from launch_app arguments to a filesystem path.
    /// Falls back to the first available profile if no name is provided.
    /// Returns an error if a name is provided but doesn't match any profile.
    pub(crate) fn resolve_chrome_profile_path(
        &self,
        chrome_profile_name: Option<&str>,
    ) -> ExecutorResult<Option<PathBuf>> {
        match chrome_profile_name {
            Some(name) => self
                .chrome_profile_store
                .resolve_profile_path_by_name(name)
                .map(Some)
                .ok_or_else(|| {
                    let available: Vec<&str> = self
                        .chrome_profiles
                        .iter()
                        .map(|p| p.name.as_str())
                        .collect();
                    ExecutorError::ToolCall {
                        tool: "launch_app".to_string(),
                        message: format!(
                            "Unknown Chrome profile '{}'. Available profiles: {}",
                            name,
                            available.join(", ")
                        ),
                    }
                }),
            None => Ok(self
                .chrome_profiles
                .first()
                .map(|p| self.chrome_profile_store.profile_path(&p.id))),
        }
    }

    // ── Convenience accessors ────────────────────────────────────────────

    pub(crate) fn focused_app_name(&self) -> Option<String> {
        self.read_focused_app()
            .as_ref()
            .map(|(name, _, _)| name.clone())
    }

    pub(crate) fn focused_app_kind(&self) -> AppKind {
        self.read_focused_app()
            .as_ref()
            .map(|(_, kind, _)| *kind)
            .unwrap_or(AppKind::Native)
    }

    #[allow(dead_code)]
    pub(crate) fn focused_app_pid(&self) -> Option<i32> {
        self.read_focused_app().as_ref().map(|(_, _, pid)| *pid)
    }

    /// Check whether a CDP connection is active for the currently focused app.
    /// Uses PID to distinguish same-name instances within a single execution.
    pub(crate) fn cdp_connected_to_focused_app(&self) -> bool {
        match (&self.cdp_connected_app, &*self.read_focused_app()) {
            (Some((cdp_name, cdp_pid)), Some((focus_name, _, focus_pid))) => {
                if cdp_name != focus_name {
                    return false;
                }
                // If both PIDs are known (non-zero), require they match.
                // PID=0 means "unknown" (e.g., from AI step bookkeeping).
                if *cdp_pid != 0 && *focus_pid != 0 {
                    return cdp_pid == focus_pid;
                }
                true // Name matches, at least one PID unknown
            }
            _ => false,
        }
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
    pub(crate) fn format_supervision_hint(
        retry_ctx: &retry_context::RetryContext,
        context: &str,
    ) -> String {
        retry_ctx
            .supervision_hint
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
    /// element resolution). Prefers supervision, falls back to
    /// VLM, then agent. The tiny agent model often has insufficient context for
    /// these prompts.
    pub(crate) fn reasoning_backend(&self) -> &C {
        self.supervision
            .as_ref()
            .or(self.fast.as_ref())
            .unwrap_or(&self.agent)
    }

    /// Send a runtime resolution query and wait for the response.
    /// Returns None if no resolution_tx is available (Run mode) or
    /// if this (node_id, target) was previously rejected.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn request_resolution(
        &self,
        retry_ctx: &retry_context::RetryContext,
        node_id: Uuid,
        node_name: &str,
        action_description: &str,
        target: &str,
        element_inventory: &str,
        screenshot: Option<String>,
    ) -> Option<RuntimeResolution> {
        let tx = self.resolution_tx.as_ref()?;

        // Skip if previously rejected for this (node, target)
        if retry_ctx
            .rejected_resolutions
            .contains(&(node_id, target.to_string()))
        {
            return None;
        }

        let (response_tx, response_rx) = tokio::sync::oneshot::channel();
        let query = RuntimeQuery {
            node_id,
            node_name: node_name.to_string(),
            action_description: action_description.to_string(),
            target: target.to_string(),
            screenshot,
            element_inventory: element_inventory.to_string(),
            current_node_id: node_id,
            completed_node_ids: retry_ctx
                .completed_node_ids
                .iter()
                .map(|(id, _)| *id)
                .collect(),
            response_tx,
        };

        if tx.send(query).await.is_err() {
            return None; // listener shut down
        }

        response_rx.await.ok()
    }

    /// Roll back execution state to just after the given target node.
    ///
    /// Removes all completed nodes after `target` from `ctx.completed_node_ids`,
    /// strips the corresponding variables from `self.context`, clears loop
    /// counters, and removes verdicts for invalidated nodes.
    fn rollback_to(&mut self, target: Uuid, ctx: &mut retry_context::RetryContext) {
        // Use rposition to find the LAST (most recent) occurrence of the target.
        // In loops, the same node appears multiple times; we want to keep all
        // iterations up to and including the most recent completion of the target.
        let rollback_from = ctx
            .completed_node_ids
            .iter()
            .rposition(|(id, _)| *id == target)
            .map(|pos| pos + 1)
            .unwrap_or(ctx.completed_node_ids.len());

        let invalidated: Vec<(Uuid, String)> =
            ctx.completed_node_ids.drain(rollback_from..).collect();

        for (_, prefix) in &invalidated {
            self.context.remove_variables_with_prefix(prefix);
        }

        // NOTE: Loop counters are intentionally NOT cleared here.
        // completed_node_ids doesn't track Loop/EndLoop control-flow nodes,
        // so we cannot determine which loops the rewind crosses. Clearing
        // all counters resets active parent loops; clearing none preserves
        // stale counters if the rewind crosses a loop boundary. Both are
        // wrong in different edge cases. In practice, runtime resolution
        // rewinds stay within the same loop iteration or advance forward,
        // so preserving counters is the safer default. A proper fix requires
        // tracking loop entry/exit in the rewind path (deferred).

        let inv_ids: HashSet<Uuid> = invalidated.iter().map(|(id, _)| *id).collect();

        ctx.runtime_verdicts
            .retain(|v| !inv_ids.contains(&v.node_id));

        // Prune execution_history: truncate right after the last retained
        // NodeCompleted so that stale control-flow entries (BranchTaken,
        // LoopIteration, etc.) recorded after the rewind target are also removed.
        let target_completed_count = ctx.completed_node_ids.len();
        if target_completed_count == 0 {
            ctx.execution_history.clear();
        } else {
            // Find position right after the Nth NodeCompleted (N = retained count)
            let mut seen = 0usize;
            let mut cutoff = 0;
            for (i, e) in ctx.execution_history.iter().enumerate() {
                if matches!(
                    e,
                    retry_context::ExecutionHistoryEntry::NodeCompleted { .. }
                ) {
                    seen += 1;
                    if seen == target_completed_count {
                        cutoff = i + 1;
                        break;
                    }
                }
            }
            ctx.execution_history.truncate(cutoff);
        }
    }

    /// Apply a resolution patch to the in-memory workflow.
    pub(crate) fn apply_resolution_patch(&mut self, patch: &clickweave_core::WorkflowPatchCompact) {
        self.workflow = clickweave_core::merge_patch_into_workflow(
            &self.workflow,
            &patch.added_nodes,
            &patch.removed_node_ids,
            &patch.updated_nodes,
            &patch.added_edges,
            &patch.removed_edges,
        );
    }

    /// Return the best available LLM for vision tasks (image analysis,
    /// screenshot verification). Prefers an explicitly configured VLM, falls
    /// back to supervision. Returns `None` only when neither
    /// VLM nor supervisor is configured.
    pub(crate) fn vision_backend(&self) -> Option<&C> {
        self.fast.as_ref().or(self.supervision.as_ref())
    }
}
