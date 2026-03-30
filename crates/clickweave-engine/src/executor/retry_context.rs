use clickweave_core::NodeVerdict;
use clickweave_llm::Message;
use std::collections::HashSet;
use std::sync::RwLock;
use uuid::Uuid;

use super::PendingLoopExit;

/// Per-run transient state that is meaningful only during a single graph walk.
///
/// Created at the start of `run_with_mcp()` and threaded through methods that
/// need supervision retry state, loop exits, verdicts, and resolution tracking.
/// This keeps `WorkflowExecutor` free of fields whose lifetimes are shorter
/// than the executor itself.
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

    /// Set by eval_control_flow when a loop exits; consumed by the main loop
    /// to run a deferred visual verification after the loop completes.
    pub pending_loop_exit: Option<PendingLoopExit>,

    /// Persistent conversation history for supervision across the entire run.
    pub supervision_history: RwLock<Vec<Message>>,

    /// Verdicts from Verification-role nodes, accumulated during execution.
    pub runtime_verdicts: Vec<NodeVerdict>,

    /// Node IDs the executor has completed in this run (for patch validation).
    /// Each entry is (node_id, sanitized auto_id prefix) so rollback can remove
    /// the corresponding variables.
    pub completed_node_ids: Vec<(Uuid, String)>,

    /// Rejected resolutions keyed by (node_id, target) -- skip callback on retry.
    pub rejected_resolutions: HashSet<(Uuid, String)>,

    /// When true, skip the persistent decision cache during element and app
    /// resolution so the executor re-resolves via LLM instead of replaying a
    /// stale cached decision. Set after an eviction on retry; reset to false
    /// after a node succeeds.
    pub force_resolve: bool,

    /// Set to true when an AI step calls a focus-changing tool (launch_app,
    /// focus_window, quit_app). Used by post-AI-step logic to trigger a state
    /// refresh (Task 11+).
    pub focus_dirty: bool,
}

impl RetryContext {
    pub fn new() -> Self {
        Self {
            supervision_hint: None,
            tried_click_indices: RwLock::new(Vec::new()),
            tried_cdp_uids: RwLock::new(Vec::new()),
            last_typed_url: None,
            pending_loop_exit: None,
            supervision_history: RwLock::new(Vec::new()),
            runtime_verdicts: Vec::new(),
            completed_node_ids: Vec::new(),
            rejected_resolutions: HashSet::new(),
            force_resolve: false,
            focus_dirty: false,
        }
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

    pub fn read_tried_cdp_uids(&self) -> std::sync::RwLockReadGuard<'_, Vec<String>> {
        self.tried_cdp_uids
            .read()
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
