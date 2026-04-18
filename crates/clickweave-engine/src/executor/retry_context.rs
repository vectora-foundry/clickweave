use clickweave_core::NodeVerdict;
use clickweave_llm::Message;
use std::collections::HashMap;
use uuid::Uuid;

/// Per-run transient state that is meaningful only during a single graph walk.
///
/// Created at the start of `run_with_mcp()` and threaded through methods that
/// need supervision retry state and verdicts. This keeps `WorkflowExecutor`
/// free of fields whose lifetimes are shorter than the executor itself.
///
/// Mutation goes through `&mut RetryContext`: the context is owned by the
/// single-threaded run loop, so there is no concurrent access to guard against.
pub(crate) struct RetryContext {
    /// Hint from a previous supervision failure, threaded into disambiguation
    /// prompts on retry so the LLM picks a different match.
    pub supervision_hint: Option<String>,

    /// Native click disambiguation indices already tried during supervision retries.
    pub tried_click_indices: Vec<usize>,

    /// CDP element UIDs already tried during supervision retries.
    pub tried_cdp_uids: Vec<String>,

    /// Text from the most recent TypeText node on a Chrome/CDP app when it
    /// looks like a URL (e.g. `gmail.com`, `https://...`).
    /// Arms the following `press_key return` intercept.
    pub last_typed_url: Option<String>,

    /// Supervision conversation history for the currently executing node.
    /// Cleared at the start of each node (see `reset_for_next_node`) so a
    /// long workflow does not accumulate unrelated prior visual
    /// observations into later supervision verdicts. Each verdict is
    /// self-contained: the supervisor looks at this step's screenshot and
    /// decides whether this step worked.
    pub supervision_history: Vec<Message>,

    /// Verdicts from Verification-role nodes, accumulated during execution.
    pub runtime_verdicts: Vec<NodeVerdict>,

    /// Node IDs the executor has completed in this run (for patch validation).
    /// Each entry is (node_id, sanitized auto_id prefix) so rollback can remove
    /// the corresponding variables.
    pub completed_node_ids: Vec<(Uuid, String)>,

    /// Whether the next resolve call should consult the persistent decision
    /// cache. Set to [`CacheMode::Bypass`] after an eviction on retry; reset
    /// to [`CacheMode::UseCache`] after a node succeeds.
    pub cache_mode: super::app_resolve::CacheMode,

    /// Set to true when an AI step calls a focus-changing tool (launch_app,
    /// focus_window, quit_app). Used by post-AI-step logic to trigger a state
    /// refresh.
    pub focus_dirty: bool,

    /// Raw tool result text from the last deterministic MCP tool call.
    /// Set by deterministic execution, read by supervision to include
    /// actual-vs-intended comparison in the step message.
    pub last_tool_result: Option<String>,

    /// Agent-picked UID overrides for CDP targets whose snapshot was ambiguous.
    /// Keyed by the resolver target string; the CDP resolver consults this map
    /// before taking a snapshot so the follow-up retry clicks the chosen
    /// element instead of re-raising the same ambiguity error.
    pub cdp_ambiguity_overrides: HashMap<String, String>,
}

impl RetryContext {
    pub fn new() -> Self {
        Self {
            supervision_hint: None,
            tried_click_indices: Vec::new(),
            tried_cdp_uids: Vec::new(),
            last_typed_url: None,
            supervision_history: Vec::new(),
            runtime_verdicts: Vec::new(),
            completed_node_ids: Vec::new(),
            cache_mode: super::app_resolve::CacheMode::UseCache,
            focus_dirty: false,
            last_tool_result: None,
            cdp_ambiguity_overrides: HashMap::new(),
        }
    }

    /// Clear supervision history between workflow nodes. The supervisor
    /// prompt stays self-contained per step — prior visual observations
    /// from unrelated earlier steps inflate the prompt and confuse the
    /// verdict. The system prompt is re-seeded lazily inside
    /// `judge_with_history` when the next step runs.
    pub fn reset_supervision_history(&mut self) {
        self.supervision_history.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clickweave_llm::Message;

    #[test]
    fn reset_supervision_history_clears_accumulated_messages() {
        // Pins the per-node history scope: a long workflow must not accumulate
        // prior nodes' supervision exchanges into later verdicts. The run
        // loop calls this at the top of every node.
        let mut ctx = RetryContext::new();
        ctx.supervision_history
            .push(Message::user("observation from node 1"));
        ctx.supervision_history.push(Message::assistant("YES"));
        assert_eq!(ctx.supervision_history.len(), 2);

        ctx.reset_supervision_history();
        assert!(ctx.supervision_history.is_empty());
    }
}
