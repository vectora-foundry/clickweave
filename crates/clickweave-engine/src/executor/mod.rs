mod action_verification;
mod ai_step;
pub(crate) mod ambiguity;
mod app_resolve;
mod cdp_wait;
pub(crate) mod deterministic;
mod element_resolve;
pub mod error;
mod find_app;
mod graph_nav;
mod prompts;
pub(crate) mod retry_context;
mod run_loop;
pub(crate) mod screenshot;
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
use clickweave_core::{ExecutionMode, NodeRun, NodeVerdict, Workflow};
use clickweave_llm::{ChatBackend, LlmClient, LlmConfig};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::future::Future;
use std::path::PathBuf;
use tokio::sync::mpsc::Sender;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

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

    /// Re-fetch the server's tool list into the client's internal cache.
    /// This refreshes what `has_tool` reports (e.g. so `cdp_find_elements`
    /// becomes visible after `cdp_connect`) but does **not** change the
    /// agent's LLM-visible tool list — the latter is seeded once per run
    /// in `agent/mod.rs` and kept stable for prompt-cache stability. See
    /// the "Tool Exposure" policy in `docs/reference/engine/execution.md`.
    ///
    /// Named for the client-vs-server distinction: this reloads the
    /// **server** tool list into the client cache; reach for it only when
    /// a prior tool call (e.g. `cdp_connect`) changes which server-side
    /// tools are valid.
    fn refresh_server_tool_list(&self) -> impl Future<Output = anyhow::Result<()>> + Send;
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

    fn refresh_server_tool_list(&self) -> impl Future<Output = anyhow::Result<()>> + Send {
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
    /// Agent picked one candidate from an ambiguous CDP resolver match.
    /// Fires after the agent commits to a choice; the run loop continues with
    /// the chosen uid. The UI renders this as a persistent card with a modal
    /// that overlays each candidate's rect on top of the captured screenshot.
    AmbiguityResolved {
        node_id: Uuid,
        target: String,
        candidates: Vec<CandidateView>,
        chosen_uid: String,
        reasoning: String,
        /// Viewport dimensions (CSS pixels) at capture time. Used by the UI
        /// overlay to translate candidate rects — which are viewport-relative
        /// — into image-pixel coordinates when the screenshot includes
        /// chrome (title bar, tab bar) around the viewport.
        viewport_width: f64,
        viewport_height: f64,
        /// Screenshot filename relative to the node's `artifacts/` directory.
        /// The UI reads the live base64 from `screenshot_base64`; this path is
        /// for post-run re-rendering via the trace event.
        screenshot_path: String,
        /// Base64-encoded PNG of the screenshot taken at decision time. Sent
        /// inline so the UI can render the modal immediately without a
        /// separate filesystem read.
        screenshot_base64: String,
    },
    NodeCancelled(Uuid),
}

#[derive(Debug, Clone)]
pub(crate) struct ResolvedApp {
    pub name: String,
    pub pid: i32,
}

/// Number of consecutive trace-write failures tolerated before the executor
/// emits a degraded-persistence error so the UI can warn the operator.
/// Transient disk-full or permission blips recover on their own; sustained
/// failures would otherwise silently produce gap-filled `events.jsonl` files.
pub(crate) const TRACE_WRITE_FAILURE_THRESHOLD: u32 = 3;

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
    app_cache: HashMap<String, ResolvedApp>,
    focused_app: Option<(String, AppKind, i32)>,
    element_cache: HashMap<(String, Option<String>), String>,
    context: RuntimeContext,
    decision_cache: DecisionCache,
    /// Shared CDP lifecycle bookkeeping.
    ///
    /// Holds the currently-connected `(app_name, pid)` (see
    /// [`crate::cdp_lifecycle::CdpState::connected_app`]) and the
    /// per-instance last-observed page URLs used to restore the selected
    /// tab across a disconnect/reconnect cycle. The same struct backs the
    /// agent runner's CDP state so a fix to the lifecycle state machine
    /// applies uniformly to both execution paths.
    cdp_state: crate::cdp_lifecycle::CdpState,
    cancel_token: CancellationToken,
    /// Store for Chrome user-data-dir profiles (resolves profile names to paths).
    chrome_profile_store: ChromeProfileStore,
    /// Cached profile list, loaded once at construction.
    chrome_profiles: Vec<ChromeProfile>,
    /// Delay (ms) before capturing the per-step supervision screenshot.
    supervision_delay_ms: u64,
    /// Count of consecutive trace-write failures. Reset on success. When the
    /// run crosses [`TRACE_WRITE_FAILURE_THRESHOLD`] a degraded-trace error is
    /// emitted exactly once so the UI can warn the operator; later failures
    /// no longer emit until a successful write clears the streak.
    trace_write_failures: u32,
    /// Whether we have already emitted a degraded-trace error for the current
    /// failure streak. Cleared on the next successful trace write.
    trace_failure_reported: bool,
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
            agent: LlmClient::new(agent_config.with_thinking(false)),
            fast: fast_config.map(|c| LlmClient::new(c.with_thinking(false))),
            supervision: supervision_config.map(|c| LlmClient::new(c.with_thinking(false))),
            verdict_fast,
            mcp_binary_path,
            execution_mode,
            project_path,
            event_tx,
            storage,
            app_cache: HashMap::new(),
            focused_app: None,
            element_cache: HashMap::new(),
            context: RuntimeContext::new(),
            decision_cache,
            cdp_state: crate::cdp_lifecycle::CdpState::new(),
            cancel_token,
            chrome_profile_store,
            chrome_profiles,
            supervision_delay_ms,
            trace_write_failures: 0,
            trace_failure_reported: false,
        }
    }
}

impl<C: ChatBackend> WorkflowExecutor<C> {
    // ── Per-run state accessors ─────────────────────────────────────────
    // Several of these wrap fields only to document intent; callers may
    // freely mutate the underlying plain fields via `&mut self`. The
    // executor runs single-threaded inside a dedicated
    // `tauri::async_runtime::spawn` task, so shared-reference mutation is
    // sufficient; no synchronisation primitives are needed.

    pub(crate) fn write_app_cache(&mut self) -> &mut HashMap<String, ResolvedApp> {
        &mut self.app_cache
    }

    pub(crate) fn read_focused_app(&self) -> &Option<(String, AppKind, i32)> {
        &self.focused_app
    }

    pub(crate) fn write_focused_app(&mut self) -> &mut Option<(String, AppKind, i32)> {
        &mut self.focused_app
    }

    pub(crate) fn read_element_cache(&self) -> &HashMap<(String, Option<String>), String> {
        &self.element_cache
    }

    pub(crate) fn read_decision_cache(&self) -> &DecisionCache {
        &self.decision_cache
    }

    pub(crate) fn write_decision_cache(&mut self) -> &mut DecisionCache {
        &mut self.decision_cache
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

    /// Resolve a Chrome profile path for an app by kind and name.
    ///
    /// The profile store only launches Google Chrome (`spawn_chrome` hardcodes
    /// `/Applications/Google Chrome.app/...` on macOS and `chrome`/`google-chrome`
    /// on Windows/Linux). Threading a profile hint into `ensure_cdp_connected`
    /// for anything else — an Electron app, Chromium, or another
    /// `AppKind::ChromeBrowser` variant like Brave, Edge, or Arc — would skip
    /// the "reuse existing debug port" path and force a quit/relaunch with a
    /// Google-Chrome binary the caller never asked for.
    ///
    /// Returns `None` unless `app_kind == ChromeBrowser` **and** `app_name`
    /// looks like Google Chrome (see `is_google_chrome_app_name`).
    pub(crate) fn resolve_chrome_profile_path_for_app(
        &self,
        app_kind: AppKind,
        app_name: &str,
        chrome_profile_name: Option<&str>,
    ) -> ExecutorResult<Option<PathBuf>> {
        if app_kind != AppKind::ChromeBrowser || !is_google_chrome_app_name(app_name) {
            return Ok(None);
        }
        self.resolve_chrome_profile_path(chrome_profile_name)
    }

    // ── Convenience accessors ────────────────────────────────────────────

    pub(crate) fn focused_app_name(&self) -> Option<String> {
        self.focused_app.as_ref().map(|(name, _, _)| name.clone())
    }

    pub(crate) fn focused_app_kind(&self) -> AppKind {
        self.focused_app
            .as_ref()
            .map(|(_, kind, _)| *kind)
            .unwrap_or(AppKind::Native)
    }

    /// Check whether a CDP connection is active for the currently focused app.
    /// Uses PID to distinguish same-name instances within a single execution.
    pub(crate) fn cdp_connected_to_focused_app(&self) -> bool {
        match self.read_focused_app() {
            Some((focus_name, _, focus_pid)) => {
                self.cdp_state.is_connected_to(focus_name, *focus_pid)
            }
            None => false,
        }
    }

    /// Read-only access to the shared CDP lifecycle state. Test-only;
    /// production callers reach into `self.cdp_state` directly, matching
    /// the convention used for other executor fields.
    #[cfg(test)]
    pub(crate) fn cdp_state(&self) -> &crate::cdp_lifecycle::CdpState {
        &self.cdp_state
    }

    /// Mutable access to the shared CDP lifecycle state. Test-only —
    /// same usage profile as [`Self::cdp_state`].
    #[cfg(test)]
    pub(crate) fn cdp_state_mut(&mut self) -> &mut crate::cdp_lifecycle::CdpState {
        &mut self.cdp_state
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

    /// Return the best available LLM for vision tasks (image analysis,
    /// screenshot verification). Prefers an explicitly configured VLM, falls
    /// back to supervision. Returns `None` only when neither
    /// VLM nor supervisor is configured.
    pub(crate) fn vision_backend(&self) -> Option<&C> {
        self.fast.as_ref().or(self.supervision.as_ref())
    }
}

/// Whether an app name denotes Google Chrome — the only app the Chrome-profile
/// launcher (`spawn_chrome`) can actually start.
///
/// Matches "Google Chrome", "Google Chrome Canary", etc., but *not* Chromium
/// (classified as `AppKind::ChromeBrowser` but shipped as its own binary),
/// Brave, Edge, or Arc. Those must pass `None` for the profile path so
/// `ensure_cdp_connected` can (a) reuse an already-running debug port and
/// (b) fall through to the MCP `launch_app` relaunch branch that respects
/// the real binary name.
pub(crate) fn is_google_chrome_app_name(app_name: &str) -> bool {
    let lower = app_name.trim().to_ascii_lowercase();
    matches!(
        lower.as_str(),
        "chrome"
            | "google chrome"
            | "google chrome canary"
            | "google chrome beta"
            | "google chrome dev"
    )
}

#[cfg(test)]
mod app_name_tests {
    use super::is_google_chrome_app_name;

    #[test]
    fn matches_google_chrome_family() {
        assert!(is_google_chrome_app_name("Google Chrome"));
        assert!(is_google_chrome_app_name("Google Chrome Canary"));
        assert!(is_google_chrome_app_name("chrome"));
    }

    #[test]
    fn rejects_chromium() {
        // Chromium is `AppKind::ChromeBrowser` but `spawn_chrome` cannot
        // launch it — it hardcodes the Google Chrome binary.
        assert!(!is_google_chrome_app_name("Chromium"));
        assert!(!is_google_chrome_app_name("chromium"));
    }

    #[test]
    fn rejects_other_chrome_family_browsers() {
        // These are classified as `AppKind::ChromeBrowser` but have no
        // Chrome-profile tooling in this codebase.
        assert!(!is_google_chrome_app_name("Brave Browser"));
        assert!(!is_google_chrome_app_name("Microsoft Edge"));
        assert!(!is_google_chrome_app_name("Arc"));
    }

    #[test]
    fn rejects_electron_and_native_apps() {
        assert!(!is_google_chrome_app_name("Slack"));
        assert!(!is_google_chrome_app_name("Visual Studio Code"));
        assert!(!is_google_chrome_app_name("Calculator"));
    }
}
