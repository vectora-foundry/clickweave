use clickweave_core::NodeVerdict;
use clickweave_llm::Message;
use std::collections::HashMap;
use std::sync::RwLock;
use uuid::Uuid;

/// Per-run transient state that is meaningful only during a single graph walk.
///
/// Created at the start of `run_with_mcp()` and threaded through methods that
/// need supervision retry state and verdicts. This keeps `WorkflowExecutor`
/// free of fields whose lifetimes are shorter than the executor itself.
pub(crate) struct RetryContext {
    /// Hint from a previous supervision failure, threaded into disambiguation
    /// prompts on retry so the LLM picks a different match.
    pub supervision_hint: Option<String>,

    /// Native click disambiguation indices already tried during supervision retries.
    pub tried_click_indices: RwLock<Vec<usize>>,

    /// CDP element UIDs already tried during supervision retries.
    pub tried_cdp_uids: RwLock<Vec<String>>,

    /// Text from the most recent TypeText node on a Chrome/CDP app when it
    /// looks like a URL (e.g. `gmail.com`, `https://...`).
    /// Arms the following `press_key return` intercept.
    pub last_typed_url: Option<String>,

    /// Persistent conversation history for supervision across the entire run.
    pub supervision_history: RwLock<Vec<Message>>,

    /// Verdicts from Verification-role nodes, accumulated during execution.
    pub runtime_verdicts: Vec<NodeVerdict>,

    /// Node IDs the executor has completed in this run (for patch validation).
    /// Each entry is (node_id, sanitized auto_id prefix) so rollback can remove
    /// the corresponding variables.
    pub completed_node_ids: Vec<(Uuid, String)>,

    /// When true, skip the persistent decision cache during element and app
    /// resolution so the executor re-resolves via LLM instead of replaying a
    /// stale cached decision. Set after an eviction on retry; reset to false
    /// after a node succeeds.
    pub force_resolve: bool,

    /// Set to true when an AI step calls a focus-changing tool (launch_app,
    /// focus_window, quit_app). Used by post-AI-step logic to trigger a state
    /// refresh (Task 11+).
    pub focus_dirty: bool,

    /// Raw tool result text from the last deterministic MCP tool call.
    /// Set by deterministic execution, read by supervision to include
    /// actual-vs-intended comparison in the step message.
    pub last_tool_result: Option<String>,

    /// Agent-picked UID overrides for CDP targets whose snapshot was ambiguous.
    /// Keyed by the resolver target string; the CDP resolver consults this map
    /// before taking a snapshot so the follow-up retry clicks the chosen
    /// element instead of re-raising the same ambiguity error.
    pub cdp_ambiguity_overrides: RwLock<HashMap<String, String>>,
}

impl RetryContext {
    pub fn new() -> Self {
        Self {
            supervision_hint: None,
            tried_click_indices: RwLock::new(Vec::new()),
            tried_cdp_uids: RwLock::new(Vec::new()),
            last_typed_url: None,
            supervision_history: RwLock::new(Vec::new()),
            runtime_verdicts: Vec::new(),
            completed_node_ids: Vec::new(),
            force_resolve: false,
            focus_dirty: false,
            last_tool_result: None,
            cdp_ambiguity_overrides: RwLock::new(HashMap::new()),
        }
    }

    pub fn read_cdp_ambiguity_overrides(
        &self,
    ) -> std::sync::RwLockReadGuard<'_, HashMap<String, String>> {
        self.cdp_ambiguity_overrides
            .read()
            .unwrap_or_else(|e| e.into_inner())
    }

    pub fn write_cdp_ambiguity_overrides(
        &self,
    ) -> std::sync::RwLockWriteGuard<'_, HashMap<String, String>> {
        self.cdp_ambiguity_overrides
            .write()
            .unwrap_or_else(|e| e.into_inner())
    }

    // ── RwLock helpers ───────────────────────────────────────────────────
    // Centralize the `.unwrap_or_else(|e| e.into_inner())` poison-recovery
    // pattern so call sites stay concise.

    pub fn read_tried_click_indices(&self) -> std::sync::RwLockReadGuard<'_, Vec<usize>> {
        self.tried_click_indices
            .read()
            .unwrap_or_else(|e| e.into_inner())
    }

    pub fn write_tried_click_indices(&self) -> std::sync::RwLockWriteGuard<'_, Vec<usize>> {
        self.tried_click_indices
            .write()
            .unwrap_or_else(|e| e.into_inner())
    }

    pub fn write_tried_cdp_uids(&self) -> std::sync::RwLockWriteGuard<'_, Vec<String>> {
        self.tried_cdp_uids
            .write()
            .unwrap_or_else(|e| e.into_inner())
    }

    #[allow(dead_code)] // Kept for API symmetry with write_supervision_history
    pub fn read_supervision_history(&self) -> std::sync::RwLockReadGuard<'_, Vec<Message>> {
        self.supervision_history
            .read()
            .unwrap_or_else(|e| e.into_inner())
    }

    pub fn write_supervision_history(&self) -> std::sync::RwLockWriteGuard<'_, Vec<Message>> {
        self.supervision_history
            .write()
            .unwrap_or_else(|e| e.into_inner())
    }
}
