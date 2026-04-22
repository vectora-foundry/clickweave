use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::{Context, Result};
use clickweave_core::cdp::{CdpFindElementMatch, CdpFindElementsResponse};
use clickweave_core::tool_mapping::tool_invocation_to_node_type;
use clickweave_core::{Edge, Node, Position, Workflow};
use clickweave_llm::{ChatBackend, Message};
use serde_json::Value;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use super::completion_check::{
    VlmVerdict, build_completion_prompt, parse_yes_no, pick_completion_screenshot_scope,
};
use super::context;
use super::permissions::{PermissionAction, PermissionPolicy, ToolAnnotations, evaluate};
use super::prompt::{self, truncate_summary};
use super::recovery::{self, RecoveryAction};
use super::transition;
use super::types::*;
use crate::executor::Mcp;
use crate::executor::screenshot::capture_screenshot_for_vlm;

/// Internal error type for the agent loop.
/// Distinguishes approval-system failure from other runtime errors.
#[derive(Debug)]
enum LoopError {
    ApprovalUnavailable,
    Other(anyhow::Error),
}

impl From<anyhow::Error> for LoopError {
    fn from(e: anyhow::Error) -> Self {
        LoopError::Other(e)
    }
}

/// Extract a text representation from an MCP tool result for the agent
/// transcript.
///
/// All `Text` blocks are joined with `\n` so the agent sees the full tool
/// response — stripping later blocks silently hides data from the LLM and
/// from cache replay. Image and unknown blocks are rendered as compact
/// placeholders so the agent-facing reply is never empty. JSON-parsing
/// call sites use `cdp_lifecycle::extract_text` instead, which has
/// first-block-only semantics for structured payloads.
fn extract_result_text(result: &clickweave_mcp::ToolCallResult) -> String {
    let mut parts: Vec<String> = Vec::with_capacity(result.content.len());
    for content in &result.content {
        match content {
            clickweave_mcp::ToolContent::Text { text } => parts.push(text.clone()),
            clickweave_mcp::ToolContent::Image { mime_type, .. } => {
                parts.push(format!("[image: {}]", mime_type));
            }
            clickweave_mcp::ToolContent::Unknown(_) => {
                parts.push("[unknown content]".to_string());
            }
        }
    }
    parts.join("\n")
}

/// Result of requesting user approval for a tool action.
enum ApprovalResult {
    Approved,
    Rejected,
    Unavailable,
}

/// State of the consecutive-destructive-tool cap after a tool call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CapStatus {
    /// Streak is still below the cap — run continues normally.
    Armed,
    /// Cap reached — the caller must emit the cap-hit event and halt.
    CapReached,
}

/// Control signal returned from [`AgentRunner::try_replay_cache`].
///
/// `Continue` means the replay handled this iteration (either succeeded or
/// recorded a policy-reject / approval-reject step) and the loop should
/// `continue`. `Break` means a terminal condition was reached (approval
/// unavailable, max-errors, destructive cap). `FellThrough` means the loop
/// should keep going to the LLM.
enum ReplayResult {
    Continue,
    Break,
    FellThrough,
}

/// Control signal returned from [`AgentRunner::handle_step_outcome`].
///
/// `Break` signals the outer step loop should exit (loop-detection,
/// max-errors, completion disagreement, destructive cap, or successful
/// completion). `Continue` keeps the loop running.
enum StepFlow {
    Break,
    Continue,
}

/// Build an index from tool name → MCP annotations from the raw tool
/// JSON list produced by `mcp.tools_as_openai()`. Tools without an
/// `annotations` block produce the default (all-`None`) struct.
///
/// Both the top-level `annotations` shape and the `function.annotations`
/// shape are supported, matching what `ToolAnnotations::from_tool_json`
/// accepts. Tools without a readable name are skipped.
fn build_annotations_index(mcp_tools: &[Value]) -> HashMap<String, ToolAnnotations> {
    let mut index = HashMap::with_capacity(mcp_tools.len());
    for tool in mcp_tools {
        let name = tool
            .get("function")
            .and_then(|f| f.get("name"))
            .or_else(|| tool.get("name"))
            .and_then(|v| v.as_str());
        let Some(name) = name else {
            warn!(
                tool = %tool,
                "MCP tool entry missing 'name' — skipping annotations",
            );
            continue;
        };
        index.insert(name.to_string(), ToolAnnotations::from_tool_json(tool));
    }
    index
}

/// Callback pair for requesting approval from the UI before executing a tool.
///
/// Each approval request uses a fresh `tokio::sync::oneshot` channel to avoid
/// deadlocks — the runner sends an `ApprovalRequest` bundled with a oneshot
/// `Sender<bool>`, and the UI responds exactly once.
pub struct ApprovalGate {
    pub request_tx: mpsc::Sender<(ApprovalRequest, tokio::sync::oneshot::Sender<bool>)>,
}

/// The main agent runner that implements the observe-act loop.
pub struct AgentRunner<'a, B: ChatBackend> {
    llm: &'a B,
    config: AgentConfig,
    state: AgentState,
    messages: Vec<Message>,
    cache: AgentCache,
    event_tx: Option<mpsc::Sender<AgentEvent>>,
    approval_gate: Option<ApprovalGate>,
    /// Optional VLM backend used to verify `agent_done` against a fresh
    /// screenshot. When `None`, the agent's self-reported `agent_done` stands.
    vision: Option<&'a B>,
    /// Permission policy consulted before every non-observation tool call.
    /// The default policy has no rules, allow_all = false, and
    /// require_confirm_destructive = false, which reproduces the previous
    /// behaviour (every approval-gated tool goes through the prompt).
    permissions: PermissionPolicy,
    /// Generation ID for this run. Stamped on every `Node` produced by
    /// `add_workflow_node` so the UI can scope selective-delete and
    /// Clear-conversation to agent-built nodes. Defaults to a fresh UUID
    /// when `with_run_id` is not called.
    run_id: uuid::Uuid,
    /// CDP lifecycle bookkeeping shared with the deterministic executor.
    /// Tracks the currently connected `(app_name, pid)` and — critically
    /// for the agent observe-act loop — the last-observed page URL per
    /// app instance so the runner can restore the selected tab across a
    /// CDP disconnect/reconnect cycle.
    cdp_state: crate::cdp_lifecycle::CdpState,
    /// Per-app-name kind hints (`"Native"`, `"ElectronApp"`,
    /// `"ChromeBrowser"`) learned from structured `focus_window` /
    /// `launch_app` responses and from `probe_app` output. Populated by
    /// [`Self::record_app_kind`] whenever [`Self::resolve_cdp_target`]
    /// surfaces a kind, and consulted by [`Self::should_skip_focus_window`]
    /// so that a subsequent `focus_window` against a known-Native app on
    /// macOS can be suppressed when AX dispatch tools are available —
    /// AX dispatch is focus-preserving, so pre-focusing just steals
    /// foreground from the user for no behavioral benefit.
    known_app_kinds: HashMap<String, String>,
    /// Directory where completion-verification artifacts (screenshot + metadata
    /// JSON) are written after every `verify_completion` call. When `None`,
    /// no artifacts are persisted (e.g. in tests or when storage is disabled).
    verification_artifacts_dir: Option<PathBuf>,
    /// Monotonically incrementing ordinal used to name successive
    /// completion-verification artifact files within one execution so that
    /// repeated `verify_completion` calls do not overwrite each other.
    verification_count: u32,
}

/// Reason the runner suppressed a `focus_window` MCP call. Each variant
/// carries the LLM-visible result text (so the model sees the constraint
/// in-context and adapts its next move) and a terse UI summary routed
/// through the `SubAction` event stream. Keeping both in one type removes
/// the string round-trip the dispatch site, the post-step bookkeeping
/// predicate, and the tests previously did against free-standing `&'static str`
/// sentinels.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(super) enum FocusSkipReason {
    /// macOS Native target, full AX dispatch toolset available —
    /// AX dispatch is focus-preserving so the real call is redundant.
    AxAvailable,
    /// Electron / Chrome target with a live CDP session and the minimum
    /// CDP dispatch toolset — CDP operates on backgrounded windows.
    CdpLive,
    /// Operator flipped [`AgentConfig::allow_focus_window`] to `false`;
    /// every focus_window is dropped regardless of kind or toolset.
    PolicyDisabled,
}

impl FocusSkipReason {
    const ALL: [Self; 3] = [Self::AxAvailable, Self::CdpLive, Self::PolicyDisabled];

    /// Result text returned to the LLM in the synthetic `StepOutcome::Success`.
    /// Must not drift from the strings the tests pin — they encode the
    /// agent→LLM skip-contract.
    pub(super) const fn llm_message(self) -> &'static str {
        match self {
            Self::AxAvailable => {
                "skipped focus_window: AX tools available; window focus not required"
            }
            Self::CdpLive => "skipped focus_window: CDP already live; focus not required",
            Self::PolicyDisabled => {
                "focus_window skipped: agent policy disallows focus changes. Use AX dispatch \
                 (ax_click/ax_set_value/ax_select) or CDP (cdp_click/cdp_fill) instead — \
                 these operate on background windows."
            }
        }
    }

    /// Terse summary for the `SubAction` event surface.
    pub(super) const fn sub_action_summary(self) -> &'static str {
        match self {
            Self::AxAvailable => "skipped: AX dispatch available",
            Self::CdpLive => "skipped: CDP already live; focus not required",
            Self::PolicyDisabled => "skipped: focus_window disabled by agent policy",
        }
    }

    /// Recover the variant from an LLM-visible result text. Used by the
    /// post-step bookkeeping predicate to keep synthetic skips invisible
    /// to CDP auto-connect and workflow-node creation.
    pub(super) fn from_llm_message(text: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|r| r.llm_message() == text)
    }
}

impl<'a, B: ChatBackend> AgentRunner<'a, B> {
    pub fn new(llm: &'a B, config: AgentConfig) -> Self {
        Self {
            llm,
            config,
            state: AgentState::new(Workflow::new("Agent Workflow")),
            messages: Vec::new(),
            cache: AgentCache::default(),
            event_tx: None,
            approval_gate: None,
            vision: None,
            permissions: PermissionPolicy::default(),
            run_id: uuid::Uuid::new_v4(),
            cdp_state: crate::cdp_lifecycle::CdpState::new(),
            known_app_kinds: HashMap::new(),
            verification_artifacts_dir: None,
            verification_count: 0,
        }
    }

    /// Create a runner pre-loaded with a cross-run decision cache.
    pub fn with_cache(llm: &'a B, config: AgentConfig, cache: AgentCache) -> Self {
        Self {
            llm,
            config,
            state: AgentState::new(Workflow::new("Agent Workflow")),
            messages: Vec::new(),
            cache,
            event_tx: None,
            approval_gate: None,
            vision: None,
            permissions: PermissionPolicy::default(),
            run_id: uuid::Uuid::new_v4(),
            cdp_state: crate::cdp_lifecycle::CdpState::new(),
            known_app_kinds: HashMap::new(),
            verification_artifacts_dir: None,
            verification_count: 0,
        }
    }

    /// Stamp this run with a caller-provided generation ID. When
    /// omitted, a fresh UUID is generated at construction.
    pub fn with_run_id(mut self, run_id: uuid::Uuid) -> Self {
        self.run_id = run_id;
        self
    }

    /// Attach a permission policy. When set, the policy is evaluated for
    /// every non-observation tool call before the approval prompt fires.
    /// `Allow` skips the prompt, `Deny` fails the step with a policy-reject
    /// error, `Ask` falls through to the existing approval flow.
    pub fn with_permissions(mut self, policy: PermissionPolicy) -> Self {
        self.permissions = policy;
        self
    }

    /// Attach a VLM backend used to verify agent-reported completion.
    ///
    /// When attached, the loop will take a screenshot after `agent_done`
    /// and ask the VLM whether the screenshot confirms the goal. A NO
    /// verdict halts the run with `TerminalReason::CompletionDisagreement`
    /// instead of falling through to `Completed`.
    pub fn with_vision(mut self, vision: &'a B) -> Self {
        self.vision = Some(vision);
        self
    }

    /// Set the directory where completion-verification artifacts are written.
    ///
    /// When set, every call to `verify_completion` persists a PNG screenshot
    /// and a JSON metadata file to this directory (named
    /// `completion_verification_<ordinal>.{png,json}`) regardless of whether
    /// the VLM agreed or disagreed. Write failures are logged at `warn` level
    /// and do not affect the run outcome.
    pub fn with_verification_artifacts_dir(mut self, dir: PathBuf) -> Self {
        self.verification_artifacts_dir = Some(dir);
        self
    }

    /// Attach an event channel for live event emission.
    pub fn with_events(mut self, tx: mpsc::Sender<AgentEvent>) -> Self {
        self.event_tx = Some(tx);
        self
    }

    /// Attach an approval gate for user-approved execution.
    pub fn with_approval(
        mut self,
        request_tx: mpsc::Sender<(ApprovalRequest, tokio::sync::oneshot::Sender<bool>)>,
    ) -> Self {
        self.approval_gate = Some(ApprovalGate { request_tx });
        self
    }

    /// Consume the runner and return the accumulated cache.
    pub fn into_cache(self) -> AgentCache {
        self.cache
    }

    /// Test-only accessor to the shared CDP lifecycle state.
    ///
    /// Kept behind `#[cfg(test)]` so production code reaches for
    /// `self.cdp_state` directly — callers outside the runner have no
    /// reason to inspect it.
    #[cfg(test)]
    pub(crate) fn cdp_state(&self) -> &crate::cdp_lifecycle::CdpState {
        &self.cdp_state
    }

    /// Test-only seed for the `(app_kind, cdp_connected)` state the
    /// runner would otherwise reach only after `launch_app` →
    /// `auto_connect_cdp` → `on_cdp_connected`. Used by integration
    /// tests that want to exercise the post-CDP-connect focus_window
    /// skip path without the full quit/relaunch/connect choreography.
    #[cfg(test)]
    pub(crate) fn seed_cdp_live_for_test(&mut self, app_name: &str, kind: &str) {
        self.record_app_kind(app_name, kind);
        self.cdp_state.set_connected(app_name, 0);
    }

    /// Test-only entry point into the selected-page snapshot helper so
    /// the agent-vs-executor parity suite can exercise exactly the code
    /// the live run would hit, rather than poking fields.
    #[cfg(test)]
    pub(crate) async fn snapshot_selected_page_url_for_test(
        &mut self,
        app_name: &str,
        pid: i32,
        mcp: &(impl Mcp + ?Sized),
    ) {
        self.snapshot_selected_page_url(app_name, pid, mcp).await;
    }

    /// Send an event through the channel (backpressured).
    ///
    /// Uses `send().await` instead of `try_send()` so the agent loop
    /// slows down when the consumer falls behind, rather than dropping
    /// events that the UI depends on for workflow state.
    async fn emit_event(&self, event: AgentEvent) {
        let Some(tx) = &self.event_tx else { return };
        if tx.is_closed() {
            return;
        }
        if let Err(e) = tx.send(event).await {
            warn!("Failed to emit agent event (channel closed): {e}");
        }
    }

    /// Evaluate the permission policy for a proposed tool call. Looks
    /// the tool up in the annotations index (missing → empty
    /// annotations, which means "no hints") and consults the pure
    /// `permissions::evaluate` function. Returns the resolved action
    /// (`Allow` / `Ask` / `Deny`).
    fn policy_for(
        &self,
        tool_name: &str,
        arguments: &Value,
        annotations_by_tool: &HashMap<String, ToolAnnotations>,
    ) -> PermissionAction {
        let ann = annotations_by_tool
            .get(tool_name)
            .copied()
            .unwrap_or_default();
        evaluate(&self.permissions, tool_name, arguments, &ann)
    }

    /// Update the consecutive-destructive-call tracker after a successful
    /// tool call, and report whether the cap has now been hit.
    ///
    /// `destructive_hint == Some(true)` increments the streak; anything
    /// else resets it. A cap value of `0` disables the feature entirely,
    /// so the method always returns `CapStatus::Armed` in that case.
    fn maybe_halt_on_destructive_cap(
        &mut self,
        tool_name: &str,
        annotations_by_tool: &HashMap<String, ToolAnnotations>,
    ) -> CapStatus {
        if self.config.consecutive_destructive_cap == 0 {
            return CapStatus::Armed;
        }
        let destructive = annotations_by_tool
            .get(tool_name)
            .and_then(|a| a.destructive_hint)
            .unwrap_or(false);
        if destructive {
            self.state
                .recent_destructive_tools
                .push(tool_name.to_string());
        } else {
            self.state.recent_destructive_tools.clear();
        }
        if self.state.recent_destructive_tools.len() >= self.config.consecutive_destructive_cap {
            CapStatus::CapReached
        } else {
            CapStatus::Armed
        }
    }

    /// Halt the run because the consecutive-destructive cap was reached.
    /// Emits the cap-hit event and sets the terminal reason. Called once
    /// when `maybe_halt_on_destructive_cap` reports `CapStatus::CapReached`.
    /// Clears `recent_destructive_tools` afterwards so state serialization
    /// reflects the drained streak.
    async fn emit_destructive_cap_hit(&mut self) {
        let recent = std::mem::take(&mut self.state.recent_destructive_tools);
        let cap = self.config.consecutive_destructive_cap;
        warn!(
            cap,
            tools = ?recent,
            "Consecutive destructive cap reached — halting run"
        );
        self.emit_event(AgentEvent::ConsecutiveDestructiveCapHit {
            recent_tool_names: recent.clone(),
            cap,
        })
        .await;
        self.state.terminal_reason = Some(TerminalReason::ConsecutiveDestructiveCap {
            recent_tool_names: recent,
            cap,
        });
    }

    /// Run the agent loop to completion or max steps.
    ///
    /// # Arguments
    /// * `goal` - The natural language goal for the agent.
    /// * `workflow` - The workflow to build nodes into.
    /// * `mcp` - MCP client for tool execution.
    /// * `variant_context` - Optional context about the current variant.
    /// * `mcp_tools` - Pre-fetched MCP tool definitions in OpenAI format.
    /// * `anchor_node_id` - When `Some`, seeds `last_node_id` so the
    ///   runner's first emitted edge connects into an existing workflow
    ///   chain (conversational Extend mode).
    /// * `prior_turns` - Chat history (goal/summary pairs) from prior
    ///   agent runs. Inlined above the current goal so the LLM has the
    ///   conversational context without adding a message slot that
    ///   would break `compact_step_summaries`.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn run(
        &mut self,
        goal: String,
        workflow: Workflow,
        mcp: &(impl Mcp + ?Sized),
        variant_context: Option<&str>,
        mcp_tools: Vec<Value>,
        anchor_node_id: Option<uuid::Uuid>,
        prior_turns: &[super::prior_turns::PriorTurn],
    ) -> Result<AgentState> {
        self.state = AgentState::new(workflow);
        self.state.last_node_id = anchor_node_id;

        // Build conversation: system instructions + goal as a user message.
        // The goal is kept out of the system prompt so user-controlled text
        // does not occupy the highest-priority instruction layer.
        let mut system_text = prompt::system_prompt();
        if let Some(ctx) = variant_context {
            system_text.push_str(&format!("\n\nVariant context: {}", ctx));
        }
        // Inline the prior-turn log inside the goal string so that
        // `compact_step_summaries` (which treats messages[1] as the goal)
        // preserves the log across context compaction.
        let composed_goal =
            super::prior_turns::build_goal_with_prior_turns(&goal, prior_turns, 1000);
        self.messages = vec![
            Message::system(system_text),
            Message::user(prompt::goal_message(&composed_goal)),
        ];

        // Build the tool list: MCP tools + agent_done + agent_replan.
        // Seeded once at run start and never mutated thereafter — mid-run
        // changes to the tool surface invalidate every prior prompt-cache
        // prefix. See the "Tool Exposure" policy in
        // `docs/reference/engine/execution.md`.
        let tools: Vec<Value> = mcp_tools
            .iter()
            .cloned()
            .chain([prompt::agent_done_tool(), prompt::agent_replan_tool()])
            .collect();

        // Index each tool's MCP annotations by name, so the policy
        // evaluator and the consecutive-destructive-cap tracker can
        // answer "is this tool destructive / read-only?" in O(1) per
        // call. Missing annotations are represented as `None` fields.
        let annotations_by_tool = build_annotations_index(&mcp_tools);

        info!(goal = %goal, max_steps = self.config.max_steps, "Agent starting");

        let mut previous_result: Option<String> = None;
        let mut last_cache_key: Option<String> = None;
        // Tracks the most recent failing (tool_name, args, error) so we can
        // detect the LLM looping on the identical failing call. The cache
        // replay path can also populate this; anything that clears the
        // failure streak (success, replan, completion) resets it to None.
        let mut last_failure: Option<(String, Value, String)> = None;

        for step_index in 0..self.config.max_steps {
            if self.state.completed {
                break;
            }

            // 1. Observe: fetch current page elements
            let elements = self.fetch_elements(mcp).await;

            // Check for page transition
            if step_index > 0 {
                let prev_elements = self
                    .state
                    .steps
                    .last()
                    .map(|s| s.elements.as_slice())
                    .unwrap_or(&[]);
                if transition::detect_transition(prev_elements, &elements) {
                    info!(step = step_index, "Page transition detected");
                }
            }

            // 2. Check cache for a previously seen decision — replay if hit.
            //    Guards:
            //    - Skip when elements are empty (degenerate cache key on native/no-CDP paths)
            //    - Skip if the same cache key was just replayed (prevents infinite loops)
            //    - Approval-gated tools still require user approval on replay
            //    - Post-tool hooks (auto_connect_cdp) run after replay
            match self
                .try_replay_cache(
                    &goal,
                    &elements,
                    step_index,
                    &mcp_tools,
                    &annotations_by_tool,
                    mcp,
                    &mut previous_result,
                    &mut last_cache_key,
                    &mut last_failure,
                )
                .await
            {
                ReplayResult::Continue => continue,
                ReplayResult::Break => break,
                ReplayResult::FellThrough => {}
            }

            // 3. Build the step observation message
            let step_msg = prompt::step_message(
                step_index,
                &elements,
                &self.state.current_url,
                previous_result.as_deref(),
            );
            self.messages.push(Message::user(step_msg));

            // 4a. Supersede older snapshot payloads. CDP/native snapshot
            //     tools each embed a full page view; retaining more than one
            //     in history makes prompt tokens grow linearly with tool-call
            //     count and quickly exhausts the LLM's context window. This
            //     pass is cheap and runs every step so older snapshots never
            //     accumulate, regardless of the coarser compaction threshold.
            if let Some(collapsed) = context::collapse_superseded_snapshots(&self.messages) {
                debug!(
                    before_tokens = context::estimate_messages_tokens(&self.messages),
                    after_tokens = context::estimate_messages_tokens(&collapsed),
                    "Superseded snapshot tool-results"
                );
                self.messages = collapsed;
            }

            // 4b. Coarse context compaction (drop old step messages into a summary)
            //     if the remaining history is still over budget.
            let token_budget = 8000; // Conservative default
            if let Some(compacted) =
                context::compact_step_summaries(&self.messages, &self.state.steps, token_budget, 3)
            {
                debug!(
                    before = self.messages.len(),
                    after = compacted.len(),
                    "Context compacted"
                );
                self.messages = compacted;
            }

            // 5. Call the LLM
            let response = self
                .llm
                .chat(&self.messages, Some(&tools))
                .await
                .context("Agent LLM call failed")?;

            let choice = response
                .choices
                .into_iter()
                .next()
                .context("No choices in LLM response")?;

            // 6. Parse and execute the response
            let (command, outcome) = match self
                .execute_response(
                    &choice.message,
                    mcp,
                    &goal,
                    &elements,
                    &mcp_tools,
                    step_index,
                    &annotations_by_tool,
                )
                .await
            {
                Ok(pair) => pair,
                Err(LoopError::ApprovalUnavailable) => {
                    warn!("Approval system unavailable, terminating agent");
                    self.state.terminal_reason = Some(TerminalReason::ApprovalUnavailable);
                    break;
                }
                Err(LoopError::Other(e)) => return Err(e),
            };

            // Auto-connect CDP for Electron/Chrome apps after the app becomes
            // the foreground target. Covers both fresh launches and cases where
            // the app was already running (focus_window). The tool's response
            // text (structured JSON on modern MCP servers) is threaded in so
            // CDP probing can short-circuit on the server-supplied `kind`.
            //
            // Skipped `focus_window` calls bypass this entirely — no MCP call
            // fired, so there's nothing to reclassify and no new connection
            // identity to record.
            if let AgentCommand::ToolCall {
                tool_name,
                arguments,
                ..
            } = &command
                && let StepOutcome::Success(result_text) = &outcome
                && !Self::is_synthetic_focus_skip(tool_name, result_text)
            {
                self.maybe_cdp_connect(tool_name, arguments, result_text, mcp)
                    .await;
            }

            // Update state
            let step = AgentStep {
                index: step_index,
                elements: elements.clone(),
                command: command.clone(),
                outcome: outcome.clone(),
                page_url: self.state.current_url.clone(),
            };
            self.state.steps.push(step);

            // Handle outcome
            let flow = self
                .handle_step_outcome(
                    &outcome,
                    &command,
                    step_index,
                    &goal,
                    &elements,
                    &mcp_tools,
                    &annotations_by_tool,
                    mcp,
                    &mut previous_result,
                    &mut last_failure,
                )
                .await;
            match flow {
                StepFlow::Break => break,
                StepFlow::Continue => {}
            }

            self.append_assistant_message(&choice.message, previous_result.as_deref());
        }

        if !self.state.completed && self.state.terminal_reason.is_none() {
            self.state.terminal_reason = Some(TerminalReason::MaxStepsReached {
                steps_executed: self.state.steps.len(),
            });
            warn!(
                steps = self.state.steps.len(),
                "Agent reached max steps without completing"
            );
        }

        // Return state by swapping in a fresh one
        let final_state = std::mem::replace(
            &mut self.state,
            AgentState::new(Workflow::new("Agent Workflow")),
        );
        Ok(final_state)
    }

    /// Attempt to replay a cached decision for the current observation.
    ///
    /// Guards:
    /// - Skip when elements are empty (degenerate cache key on native/no-CDP paths)
    /// - Skip if the same cache key was just replayed (prevents infinite loops)
    /// - Approval-gated tools still require user approval on replay
    /// - Post-tool hooks (auto_connect_cdp) run after replay
    ///
    /// Mutates `previous_result`, `last_cache_key`, `last_failure`, and
    /// `self.state` to mirror whatever the replay (or its rejection) did.
    /// Returns a [`ReplayResult`] that tells the outer loop whether to
    /// `continue`, `break`, or fall through to the live LLM path. The
    /// live-execution path shares the post-call bookkeeping in
    /// `handle_step_outcome`; here we dispatch the same transcript
    /// append + StepCompleted + CDP hook + cache-hit bump that a live
    /// tool call would trigger, the only divergence being that loop
    /// detection stays LIVE-only (by design).
    #[allow(clippy::too_many_arguments)]
    async fn try_replay_cache(
        &mut self,
        goal: &str,
        elements: &[CdpFindElementMatch],
        step_index: usize,
        mcp_tools: &[Value],
        annotations_by_tool: &HashMap<String, ToolAnnotations>,
        mcp: &(impl Mcp + ?Sized),
        previous_result: &mut Option<String>,
        last_cache_key: &mut Option<String>,
        last_failure: &mut Option<(String, Value, String)>,
    ) -> ReplayResult {
        if !self.config.use_cache || elements.is_empty() {
            return ReplayResult::FellThrough;
        }
        let current_key = super::cache::cache_key(goal, elements);
        let is_repeat = last_cache_key.as_ref() == Some(&current_key);

        if is_repeat {
            // Reset cache key tracking when falling through to LLM
            *last_cache_key = None;
            return ReplayResult::FellThrough;
        }
        let Some(cached) = self.cache.lookup(goal, elements) else {
            *last_cache_key = None;
            return ReplayResult::FellThrough;
        };
        // Skip observation tools that may exist in old cache files
        // from before the write-side filter was added.
        if Self::is_observation_tool(&cached.tool_name, annotations_by_tool) {
            debug!(
                tool = %cached.tool_name,
                "Skipping cached observation tool (stale entry)"
            );
            *last_cache_key = None;
            return ReplayResult::FellThrough;
        }
        // AX dispatch tools cache a snapshot-generation-scoped uid that the
        // server will reject as `snapshot_expired` on replay. Fall through
        // to the LLM so the agent takes a fresh snapshot + descriptor
        // resolution path.
        if Self::is_ax_dispatch_tool(&cached.tool_name) {
            debug!(
                tool = %cached.tool_name,
                "Skipping cached AX dispatch entry (uid is generation-scoped)"
            );
            *last_cache_key = None;
            return ReplayResult::FellThrough;
        }
        // State-transition tools (launch_app, focus_window, ...) must never
        // replay: the cache key encodes the pre-transition page, and the
        // next iteration's `fetch_elements` can briefly still return that
        // same pre-transition CDP snapshot before auto-connect re-targets.
        // Without this guard the cached decision fires a second time right
        // after the LLM issues it, producing duplicate `step_completed`
        // events and duplicate workflow nodes.
        if Self::is_state_transition_tool(&cached.tool_name) {
            debug!(
                tool = %cached.tool_name,
                "Skipping cached state-transition entry (not safe to replay)"
            );
            *last_cache_key = None;
            return ReplayResult::FellThrough;
        }
        let cached_tool = cached.tool_name.clone();
        let cached_args = cached.arguments.clone();
        debug!(
            tool = %cached_tool,
            hits = cached.hit_count,
            "Cache hit — replaying cached decision"
        );

        // Approval-gated tools must be re-approved on replay.
        // The permission policy decides whether to skip the
        // prompt (Allow), hard-reject (Deny), or prompt (Ask).
        let needs_approval = !Self::is_observation_tool(&cached_tool, annotations_by_tool);
        if needs_approval {
            let policy_action = self.policy_for(&cached_tool, &cached_args, annotations_by_tool);
            if matches!(policy_action, PermissionAction::Deny) {
                // Hard policy reject: fail the step, drop
                // the cached entry so the next iteration
                // does not re-hit it, and continue the loop
                // — same as any other step error.
                self.cache.remove(goal, elements);
                *last_cache_key = None;
                let err_msg = format!("Tool `{}` denied by permission policy", cached_tool);
                warn!(
                    tool = %cached_tool,
                    "Cached tool denied by permission policy"
                );
                let command = AgentCommand::ToolCall {
                    tool_name: cached_tool.clone(),
                    arguments: cached_args.clone(),
                    tool_call_id: format!("cache-{}", step_index),
                };
                self.emit_event(AgentEvent::StepFailed {
                    step_index,
                    tool_name: cached_tool.clone(),
                    error: err_msg.clone(),
                })
                .await;
                let step = AgentStep {
                    index: step_index,
                    elements: elements.to_vec(),
                    command,
                    outcome: StepOutcome::Error(err_msg.clone()),
                    page_url: self.state.current_url.clone(),
                };
                self.state.steps.push(step);
                self.state.consecutive_errors += 1;
                *previous_result = Some(format!("Error: {}", err_msg));
                let action = recovery::recovery_strategy(
                    self.state.consecutive_errors,
                    self.config.max_consecutive_errors,
                );
                if matches!(action, RecoveryAction::Abort) {
                    self.state.terminal_reason = Some(TerminalReason::MaxErrorsReached {
                        consecutive_errors: self.state.consecutive_errors,
                    });
                    return ReplayResult::Break;
                }
                return ReplayResult::Continue;
            }
            if matches!(policy_action, PermissionAction::Ask) {
                match self
                    .request_approval(&cached_tool, &cached_args, step_index, " (cached)")
                    .await
                {
                    Some(ApprovalResult::Rejected) => {
                        // Evict the rejected entry so the next
                        // iteration falls through to the LLM
                        // instead of re-prompting the same
                        // cached action in an approval loop.
                        self.cache.remove(goal, elements);
                        *last_cache_key = None;
                        let command = AgentCommand::ToolCall {
                            tool_name: cached_tool.clone(),
                            arguments: cached_args.clone(),
                            tool_call_id: format!("cache-{}", step_index),
                        };
                        let step = AgentStep {
                            index: step_index,
                            elements: elements.to_vec(),
                            command,
                            outcome: StepOutcome::Replan("User rejected cached action".to_string()),
                            page_url: self.state.current_url.clone(),
                        };
                        self.state.steps.push(step);
                        *previous_result = Some("Replan: user rejected cached action".to_string());
                        return ReplayResult::Continue;
                    }
                    Some(ApprovalResult::Unavailable) => {
                        self.state.terminal_reason = Some(TerminalReason::ApprovalUnavailable);
                        return ReplayResult::Break;
                    }
                    // Approved or no gate configured
                    _ => {}
                }
            }
            // PermissionAction::Allow: skip approval entirely.
        }

        match mcp.call_tool(&cached_tool, Some(cached_args.clone())).await {
            Ok(result) if !result.is_error.unwrap_or(false) => {
                let result_text = extract_result_text(&result);
                let tool_call_id = format!("cache-{}", step_index);
                let command = AgentCommand::ToolCall {
                    tool_name: cached_tool.clone(),
                    arguments: cached_args.clone(),
                    tool_call_id: tool_call_id.clone(),
                };

                // Rebuild workflow node for this run — the cache
                // stores decisions across runs, so the current
                // workflow needs the replayed action as a node.
                let produced_node_id_on_replay = if self.config.build_workflow {
                    self.add_workflow_node(
                        &cached_tool,
                        &cached_args,
                        mcp_tools,
                        annotations_by_tool,
                    )
                    .await
                } else {
                    None
                };
                // Append the replayed node to the cached entry's
                // lineage so selective delete can still evict the
                // right entry later.
                if let Some(node_id) = produced_node_id_on_replay
                    && let Some(entry) = self.cache.entries.get_mut(&current_key)
                {
                    entry.produced_node_ids.push(node_id);
                    entry.hit_count += 1;
                }

                // Reconstruct transcript so the LLM sees the full
                // action history, not just the raw result text.
                self.messages.push(Message::assistant_tool_calls(vec![
                    clickweave_llm::ToolCall {
                        id: tool_call_id.clone(),
                        call_type: clickweave_llm::CallType::Function,
                        function: clickweave_llm::FunctionCall {
                            name: cached_tool.clone(),
                            arguments: cached_args.clone(),
                        },
                    },
                ]));
                self.messages
                    .push(Message::tool_result(&tool_call_id, &result_text));

                // Emit live step event for cached replay
                self.emit_event(AgentEvent::StepCompleted {
                    step_index,
                    tool_name: cached_tool.clone(),
                    summary: truncate_summary(&result_text, 120),
                })
                .await;

                self.maybe_cdp_connect(&cached_tool, &cached_args, &result_text, mcp)
                    .await;

                let step = AgentStep {
                    index: step_index,
                    elements: elements.to_vec(),
                    command,
                    outcome: StepOutcome::Success(result_text.clone()),
                    page_url: self.state.current_url.clone(),
                };
                self.state.steps.push(step);
                self.state.consecutive_errors = 0;
                *last_failure = None;
                *previous_result = Some(result_text);
                *last_cache_key = Some(current_key);

                // Destructive-cap accounting: the cached
                // replay counts toward the streak just like
                // a live tool call.
                if matches!(
                    self.maybe_halt_on_destructive_cap(&cached_tool, annotations_by_tool),
                    CapStatus::CapReached
                ) {
                    self.emit_destructive_cap_hit().await;
                    return ReplayResult::Break;
                }
                ReplayResult::Continue
            }
            Ok(result) => {
                let err_text = extract_result_text(&result);
                debug!(error = %err_text, "Cached decision returned error, falling through to LLM");
                *last_cache_key = None;
                ReplayResult::FellThrough
            }
            Err(e) => {
                debug!(error = %e, "Cached decision execution failed, falling through to LLM");
                *last_cache_key = None;
                ReplayResult::FellThrough
            }
        }
    }

    /// React to a [`StepOutcome`]: emit events, update consecutive-error
    /// counters, maintain loop-detection state (LIVE-only — by design, the
    /// cache-replay path deliberately does not participate), manage the
    /// cross-run decision cache, and decide whether the outer step loop
    /// should break.
    #[allow(clippy::too_many_arguments)]
    async fn handle_step_outcome(
        &mut self,
        outcome: &StepOutcome,
        command: &AgentCommand,
        step_index: usize,
        goal: &str,
        elements: &[CdpFindElementMatch],
        mcp_tools: &[Value],
        annotations_by_tool: &HashMap<String, ToolAnnotations>,
        mcp: &(impl Mcp + ?Sized),
        previous_result: &mut Option<String>,
        last_failure: &mut Option<(String, Value, String)>,
    ) -> StepFlow {
        match outcome {
            StepOutcome::Success(result_text) => {
                self.state.consecutive_errors = 0;
                *last_failure = None;
                *previous_result = Some(result_text.clone());

                self.emit_event(AgentEvent::StepCompleted {
                    step_index,
                    tool_name: command.tool_name_or_unknown().to_string(),
                    summary: truncate_summary(result_text, 120),
                })
                .await;

                let mut cap_status = CapStatus::Armed;
                if let AgentCommand::ToolCall {
                    tool_name,
                    arguments,
                    ..
                } = command
                {
                    // Build the workflow node first so we know the node id
                    // to stamp onto the cache entry (for eviction-on-delete).
                    // A runner-skipped `focus_window` never hit MCP, so it
                    // must not appear as a node in the recorded workflow —
                    // the graph would otherwise show a FocusWindow step
                    // that didn't actually run. The cache-write arm below
                    // already excludes focus_window via STATE_TRANSITION_TOOLS.
                    let produced_node_id = if self.config.build_workflow
                        && !Self::is_synthetic_focus_skip(tool_name, result_text)
                    {
                        self.add_workflow_node(tool_name, arguments, mcp_tools, annotations_by_tool)
                            .await
                    } else {
                        None
                    };
                    // Only cache action tools, never observation tools or
                    // AX dispatch tools. Observation tools provide fresh
                    // environmental evidence that must not be replayed from
                    // stale cache entries. AX dispatch tools carry a
                    // snapshot-generation-scoped uid that the server rejects
                    // on replay — re-running the deterministic resolve path
                    // is the only correct behavior.
                    if self.config.use_cache
                        && !Self::is_observation_tool(tool_name, annotations_by_tool)
                        && !Self::is_ax_dispatch_tool(tool_name)
                        && !Self::is_state_transition_tool(tool_name)
                        && !elements.is_empty()
                    {
                        match produced_node_id {
                            Some(node_id) => {
                                self.cache.store_with_node(
                                    goal,
                                    elements,
                                    tool_name.clone(),
                                    arguments.clone(),
                                    node_id,
                                );
                            }
                            None => {
                                // No workflow node built (mapping failed or
                                // build_workflow off). Keep the decision in
                                // cache but without a node stamp — it will
                                // survive until Clear wipes the whole file.
                                self.cache.store(
                                    goal,
                                    elements,
                                    tool_name.clone(),
                                    arguments.clone(),
                                );
                            }
                        }
                    }
                    cap_status = self.maybe_halt_on_destructive_cap(tool_name, annotations_by_tool);
                }
                if matches!(cap_status, CapStatus::CapReached) {
                    self.emit_destructive_cap_hit().await;
                    return StepFlow::Break;
                }
                StepFlow::Continue
            }
            StepOutcome::Error(err) => {
                self.state.consecutive_errors += 1;
                *previous_result = Some(format!("Error: {}", err));

                self.emit_event(AgentEvent::StepFailed {
                    step_index,
                    tool_name: command.tool_name_or_unknown().to_string(),
                    error: err.clone(),
                })
                .await;

                // Loop detection: if the LLM just issued the identical
                // (tool, args) call and got the identical error back,
                // halt with LoopDetected instead of letting it chew
                // through the max-consecutive-errors budget on the
                // same broken call. The MCP tool error already tells
                // the LLM what's missing — another round won't help.
                if let AgentCommand::ToolCall {
                    tool_name,
                    arguments,
                    ..
                } = command
                {
                    let looped = matches!(
                        last_failure.as_ref(),
                        Some((prev_tool, prev_args, prev_err))
                            if prev_tool == tool_name
                                && prev_args == arguments
                                && prev_err == err
                    );
                    if looped {
                        warn!(
                            tool = %tool_name,
                            error = %err,
                            "Identical failing tool call repeated — aborting"
                        );
                        self.state.terminal_reason = Some(TerminalReason::LoopDetected {
                            tool_name: tool_name.clone(),
                            error: err.clone(),
                        });
                        return StepFlow::Break;
                    }
                    *last_failure = Some((tool_name.clone(), arguments.clone(), err.clone()));
                } else {
                    // Non-tool-call error (e.g. text-only response);
                    // don't count that toward identical-loop detection.
                    *last_failure = None;
                }

                let action = recovery::recovery_strategy(
                    self.state.consecutive_errors,
                    self.config.max_consecutive_errors,
                );
                match action {
                    RecoveryAction::Abort => {
                        warn!(
                            errors = self.state.consecutive_errors,
                            "Too many consecutive errors, aborting"
                        );
                        self.state.terminal_reason = Some(TerminalReason::MaxErrorsReached {
                            consecutive_errors: self.state.consecutive_errors,
                        });
                        StepFlow::Break
                    }
                    RecoveryAction::Continue => {
                        debug!(
                            errors = self.state.consecutive_errors,
                            "Recovery: re-observing and continuing"
                        );
                        StepFlow::Continue
                    }
                }
            }
            StepOutcome::Done(summary) => {
                // Post-agent_done VLM check. A NO verdict halts the run
                // and surfaces a disagreement event for the user to
                // adjudicate; a YES verdict (or any verification error)
                // falls through to normal completion.
                let disagreement = self.verify_completion(goal, summary, mcp).await;
                if let Some((screenshot_b64, vlm_reasoning)) = disagreement {
                    warn!("VLM disagreed with agent_done — halting run for user review");
                    self.emit_event(AgentEvent::CompletionDisagreement {
                        screenshot_b64,
                        vlm_reasoning: vlm_reasoning.clone(),
                        agent_summary: summary.clone(),
                    })
                    .await;
                    self.state.terminal_reason = Some(TerminalReason::CompletionDisagreement {
                        agent_summary: summary.clone(),
                        vlm_reasoning,
                    });
                    // Do not mark `completed` — the run halts pending
                    // user decision instead of re-planning automatically.
                    return StepFlow::Break;
                }

                self.state.completed = true;
                self.state.summary = Some(summary.clone());
                self.state.terminal_reason = Some(TerminalReason::Completed {
                    summary: summary.clone(),
                });
                *previous_result = None;
                info!(summary = %summary, "Agent completed goal");
                self.emit_event(AgentEvent::GoalComplete {
                    summary: summary.clone(),
                })
                .await;
                StepFlow::Continue
            }
            StepOutcome::Replan(reason) => {
                *previous_result = Some(format!("Replan requested: {}", reason));
                warn!(reason = %reason, "Agent requested replan");
                StepFlow::Continue
            }
        }
    }

    /// Append the assistant's response (tool call or plain text) to the
    /// conversation transcript. `reasoning_content` is intentionally omitted
    /// from the transcript: Gemma 4's model card prohibits feeding prior-turn
    /// thought blocks back into subsequent requests, and doing so causes
    /// context accumulation and degraded tool selection over multi-turn runs.
    /// Only the first tool call is included — we execute exactly one tool per
    /// turn, so appending extras would create transcript entries with no
    /// corresponding tool result.
    fn append_assistant_message(
        &mut self,
        message: &clickweave_llm::Message,
        previous_result: Option<&str>,
    ) {
        if let Some(tool_calls) = &message.tool_calls {
            if let Some(tc) = tool_calls.first() {
                let assistant_msg = Message::assistant_tool_calls(vec![tc.clone()]);
                self.messages.push(assistant_msg);
                let result_text = previous_result.unwrap_or("ok");
                self.messages
                    .push(Message::tool_result(&tc.id, result_text));
            }
        } else if let Some(text) = message.content_text() {
            let assistant_msg = Message::assistant(text);
            self.messages.push(assistant_msg);
        }
    }

    /// Fetch interactive elements from the current page via MCP.
    ///
    /// Only calls `cdp_find_elements` if the tool is available (i.e., a CDP
    /// connection has been established). This avoids unnecessary failed MCP
    /// round-trips and log noise on native-app paths.
    async fn fetch_elements(&mut self, mcp: &(impl Mcp + ?Sized)) -> Vec<CdpFindElementMatch> {
        if !mcp.has_tool("cdp_find_elements") {
            return Vec::new();
        }
        match mcp
            .call_tool(
                "cdp_find_elements",
                Some(serde_json::json!({"query": "", "max_results": 300})),
            )
            .await
        {
            Ok(result) if result.is_error != Some(true) => {
                // JSON payload: the first text block holds structured data.
                // Joining later prose blocks with \n breaks serde_json parsing.
                let text = crate::cdp_lifecycle::extract_text(&result);
                match serde_json::from_str::<CdpFindElementsResponse>(&text) {
                    Ok(parsed) => {
                        self.state.current_url = parsed.page_url;
                        return parsed.matches;
                    }
                    Err(parse_err) => {
                        // Falling through to "empty page" is the right
                        // runtime behaviour, but a schema drift in the MCP
                        // server looks identical to a genuinely empty page
                        // from the agent's perspective. Surface the parse
                        // failure so the operator can tell them apart.
                        debug!(error = %parse_err, "Failed to parse cdp_find_elements response");
                        self.emit_event(AgentEvent::Warning {
                            message: format!(
                                "cdp_find_elements response failed to parse: {} — continuing with empty elements",
                                parse_err
                            ),
                        })
                        .await;
                    }
                }
            }
            Ok(result) => {
                let text = extract_result_text(&result);
                debug!(error = %text, "cdp_find_elements returned error");
            }
            Err(e) => {
                debug!(error = %e, "cdp_find_elements call failed");
            }
        }

        Vec::new()
    }

    /// After a successful `launch_app`, probe the app type and auto-connect CDP
    /// for Electron/Chrome apps. This ensures `fetch_elements` returns structured
    /// element data on subsequent steps.
    ///
    /// Returns `Some(port)` if CDP was connected, `None` otherwise.
    ///
    /// This method performs several hidden sub-actions (probe, quit, relaunch,
    /// connect) that are not individually approved. Each sub-action is logged
    /// via `StepCompleted` events so the UI can surface the full chain.
    ///
    /// Delegates the quit/relaunch/connect state machine to
    /// [`cdp_lifecycle`] so agent and executor stay in lock-step on CDP
    /// lifecycle fixes. Selected-tab tracking is shared via the agent's
    /// own [`CdpState`][`crate::cdp_lifecycle::CdpState`] — the same
    /// bookkeeping the executor has long relied on.
    async fn auto_connect_cdp(
        &mut self,
        app_name: &str,
        kind_hint: Option<&str>,
        mcp: &(impl Mcp + ?Sized),
    ) -> Option<u16> {
        use crate::cdp_lifecycle;

        if !mcp.has_tool("cdp_connect") {
            return None;
        }

        // If the caller already classified the app (modern `focus_window`
        // now includes `kind` in its JSON response), trust it and skip
        // the `probe_app` round-trip. Only fall back to probing when the
        // hint is absent or ambiguous.
        let cdp_capable_from_hint = matches!(kind_hint, Some("ElectronApp" | "ChromeBrowser"));
        if !cdp_capable_from_hint {
            if matches!(kind_hint, Some("Native")) {
                debug!(app = app_name, "Kind hint says Native, skipping CDP");
                return None;
            }
            if !mcp.has_tool("probe_app") {
                return None;
            }

            // 1. Probe app type
            let probe_args = serde_json::json!({"app_name": app_name});
            self.emit_event(AgentEvent::SubAction {
                tool_name: "probe_app".to_string(),
                summary: format!("Auto: probing {} for CDP support", app_name),
            })
            .await;
            let probe_text = match mcp.call_tool("probe_app", Some(probe_args)).await {
                Ok(r) => {
                    self.emit_event(AgentEvent::SubAction {
                        tool_name: "probe_app".to_string(),
                        summary: format!("Auto: probed {} (ok)", app_name),
                    })
                    .await;
                    extract_result_text(&r)
                }
                Err(e) => {
                    self.emit_event(AgentEvent::SubAction {
                        tool_name: "probe_app".to_string(),
                        summary: format!("Auto: probe_app failed for {}: {}", app_name, e),
                    })
                    .await;
                    debug!(app = app_name, error = %e, "probe_app failed, skipping CDP");
                    return None;
                }
            };

            if !probe_text.contains("ElectronApp") && !probe_text.contains("ChromeBrowser") {
                debug!(app = app_name, "Not an Electron/Chrome app, skipping CDP");
                return None;
            }
        }

        info!(
            app = app_name,
            "Detected Electron/Chrome app, connecting CDP"
        );

        // 2. Check if already running with --remote-debugging-port
        if let Some(port) = crate::executor::deterministic::cdp::existing_debug_port(app_name).await
        {
            info!(app = app_name, port, "Reusing existing debug port");
            if cdp_lifecycle::connect_with_retries(mcp, port).await.is_ok() {
                self.on_cdp_connected(app_name, port, mcp).await;
                return Some(port);
            }
        }

        // 3. Quit, relaunch with a debug port, then connect CDP
        let port = clickweave_core::cdp::rand_ephemeral_port();

        self.emit_event(AgentEvent::SubAction {
            tool_name: "quit_app".to_string(),
            summary: format!("Auto: quitting {} for CDP relaunch", app_name),
        })
        .await;
        let quit_outcome = cdp_lifecycle::quit_and_wait(mcp, app_name, &mut self.cdp_state).await;
        let quit_summary = match quit_outcome {
            cdp_lifecycle::QuitOutcome::Graceful => {
                format!("Auto: {} quit confirmed", app_name)
            }
            cdp_lifecycle::QuitOutcome::TimedOut => {
                format!("Auto: {} did not quit gracefully, force-killing", app_name)
            }
        };
        self.emit_event(AgentEvent::SubAction {
            tool_name: "quit_app".to_string(),
            summary: quit_summary,
        })
        .await;

        if matches!(quit_outcome, cdp_lifecycle::QuitOutcome::TimedOut) {
            warn!(app = app_name, "App did not quit gracefully, force-killing");
            cdp_lifecycle::force_quit(mcp, app_name).await;
        }

        // Relaunch with debug port
        self.emit_event(AgentEvent::SubAction {
            tool_name: "launch_app".to_string(),
            summary: format!("Auto: relaunching {} with debug port {}", app_name, port),
        })
        .await;
        match cdp_lifecycle::launch_with_debug_port(mcp, app_name, port).await {
            Ok(()) => {
                self.emit_event(AgentEvent::SubAction {
                    tool_name: "launch_app".to_string(),
                    summary: format!("Auto: relaunched {} (ok)", app_name),
                })
                .await;
            }
            Err(err) => {
                warn!(app = app_name, error = %err, "Relaunch with debug port failed");
                self.emit_event(AgentEvent::SubAction {
                    tool_name: "launch_app".to_string(),
                    summary: format!("Auto: relaunch failed for {}: {}", app_name, err),
                })
                .await;
                let fallback = serde_json::json!({"app_name": app_name});
                crate::executor::deterministic::best_effort_tool_call(
                    mcp,
                    "launch_app",
                    Some(fallback),
                    "agent fallback relaunch (debug-port launch failed)",
                )
                .await;
                return None;
            }
        }

        cdp_lifecycle::warmup_after_relaunch().await;

        self.emit_event(AgentEvent::SubAction {
            tool_name: "cdp_connect".to_string(),
            summary: format!("Auto: connecting CDP on port {}", port),
        })
        .await;
        match cdp_lifecycle::connect_with_retries(mcp, port).await {
            Ok(()) => {
                info!(app = app_name, port, "CDP connected");
                self.emit_event(AgentEvent::SubAction {
                    tool_name: "cdp_connect".to_string(),
                    summary: format!("Auto: CDP connected on port {} (ok)", port),
                })
                .await;
                self.on_cdp_connected(app_name, port, mcp).await;
                Some(port)
            }
            Err(last_err) => {
                warn!(
                    app = app_name,
                    port,
                    error = %last_err,
                    "CDP connection failed after retries",
                );
                self.emit_event(AgentEvent::SubAction {
                    tool_name: "cdp_connect".to_string(),
                    summary: format!("Auto: CDP connect failed on port {}", port),
                })
                .await;
                None
            }
        }
    }

    /// Post-connect bookkeeping: mark the app instance as the active
    /// CDP target and record the currently-selected page URL so the
    /// next reconnect can restore the same tab.
    ///
    /// The agent has no reliable PID at observe-time (the MCP server's
    /// response shape varies, and agent-side PID tracking would itself
    /// duplicate executor bookkeeping), so the placeholder `0` is used.
    /// [`CdpState::upgrade_pid`] promotes the entry later when the
    /// runner learns the real PID.
    async fn on_cdp_connected(&mut self, app_name: &str, _port: u16, mcp: &(impl Mcp + ?Sized)) {
        self.cdp_state.set_connected(app_name, 0);
        self.snapshot_selected_page_url(app_name, 0, mcp).await;
    }

    /// Capture whichever page is currently selected for `(app_name, pid)`
    /// and record it so future reconnects can restore it. Delegates to the
    /// shared [`crate::cdp_lifecycle::snapshot_selected_page_url`] helper.
    async fn snapshot_selected_page_url(
        &mut self,
        app_name: &str,
        pid: i32,
        mcp: &(impl Mcp + ?Sized),
    ) {
        crate::cdp_lifecycle::snapshot_selected_page_url(mcp, &mut self.cdp_state, app_name, pid)
            .await;
    }

    /// Request user approval for a tool action. Returns `None` if no
    /// approval gate is configured (auto-approve).
    async fn request_approval(
        &self,
        tool_name: &str,
        arguments: &Value,
        step_index: usize,
        description_suffix: &str,
    ) -> Option<ApprovalResult> {
        let gate = self.approval_gate.as_ref()?;
        let description = format!(
            "{} with {}{}",
            tool_name,
            serde_json::to_string(arguments).unwrap_or_default(),
            description_suffix,
        );
        let request = ApprovalRequest {
            step_index,
            tool_name: tool_name.to_string(),
            arguments: arguments.clone(),
            description,
        };
        let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
        if gate.request_tx.send((request, resp_tx)).await.is_ok() {
            match resp_rx.await {
                Ok(true) => {
                    debug!(tool = %tool_name, "User approved action");
                    Some(ApprovalResult::Approved)
                }
                Ok(false) => {
                    info!(tool = %tool_name, "User rejected action");
                    Some(ApprovalResult::Rejected)
                }
                Err(_) => {
                    warn!(tool = %tool_name, "Approval channel closed");
                    Some(ApprovalResult::Unavailable)
                }
            }
        } else {
            warn!(tool = %tool_name, "Approval channel send failed");
            Some(ApprovalResult::Unavailable)
        }
    }

    /// Verify agent-reported completion against a fresh screenshot via VLM.
    ///
    /// Returns the prepared base64 screenshot and full VLM reply only when
    /// the VLM disagreed (verdict = NO) so the caller can surface a
    /// disagreement event. When the VLM agrees, or when any step of the
    /// check fails (no vision backend, screenshot failed, VLM call failed),
    /// returns `None` and the caller falls through to the normal
    /// `Completed` path. Verification errors must not tank the run.
    ///
    /// On both YES and NO verdicts, the screenshot and a JSON metadata file
    /// are written to `self.verification_artifacts_dir` (when set). Write
    /// failures are logged at `warn` level and do not affect the return value
    /// or abort the run.
    async fn verify_completion(
        &mut self,
        goal: &str,
        summary: &str,
        mcp: &(impl Mcp + ?Sized),
    ) -> Option<(String, String)> {
        let vision = self.vision?;

        let scope = pick_completion_screenshot_scope(self.cdp_state.connected_app.as_ref());
        let Some((prepared_b64, mime)) = capture_screenshot_for_vlm(mcp, scope.clone()).await
        else {
            warn!(
                scope = ?scope,
                "Completion verification: screenshot capture or prep failed, skipping VLM check",
            );
            return None;
        };

        let messages = vec![Message::user_with_images(
            build_completion_prompt(goal, summary),
            vec![(prepared_b64.clone(), mime)],
        )];
        // An empty/missing text body is treated as a verifier failure and
        // falls through, not as an implicit NO. Many non-vision endpoints
        // return empty content instead of erroring; halting the run in that
        // case would punish successful agent_done calls for a broken
        // verifier, which contradicts the fail-through contract.
        let raw_reply = match vision.chat(&messages, None).await {
            Ok(resp) => resp
                .choices
                .first()
                .and_then(|c| c.message.content_text())
                .map(str::to_owned),
            Err(e) => {
                warn!(error = %e, "Completion verification: VLM call failed, skipping check");
                return None;
            }
        };
        let reply = match raw_reply {
            Some(r) if !r.trim().is_empty() => r,
            _ => {
                warn!("Completion verification: VLM returned empty reply, skipping check");
                return None;
            }
        };

        let verdict = parse_yes_no(&reply);

        // Persist artifacts for both verdicts so every verification call
        // leaves forensic evidence. Failures are non-fatal.
        if let Some(dir) = &self.verification_artifacts_dir {
            let ordinal = self.verification_count;
            if let Err(e) = super::completion_check::persist_verification_artifacts(
                dir,
                ordinal,
                verdict,
                &reply,
                goal,
                summary,
                &prepared_b64,
            ) {
                warn!(
                    ordinal,
                    error = %e,
                    "Completion verification: failed to persist artifacts (non-fatal)",
                );
            }
        }
        self.verification_count += 1;

        match verdict {
            VlmVerdict::Yes => {
                info!(reply = %reply, "Completion verification: VLM confirmed goal");
                None
            }
            VlmVerdict::No => {
                info!(reply = %reply, "Completion verification: VLM rejected goal");
                Some((prepared_b64, reply))
            }
        }
    }

    /// Run post-tool hooks for launch/focus actions: auto-connect CDP.
    ///
    /// Refreshes the MCP client's internal tool cache after a successful
    /// connect so subsequent `has_tool(...)` checks (notably the one in
    /// `fetch_elements` that gates `cdp_find_elements`) see the tools the
    /// server exposes post-connect. This does **not** mutate the agent's
    /// LLM-visible tool list — that is seeded once at run start (see
    /// `agent/mod.rs`) and kept stable across steps so prompt-cache
    /// prefixes stay valid. Tools that the LLM picks but that require a
    /// live connection return a clean "not connected" error when called
    /// pre-connection; the agent recovers on the next step.
    async fn maybe_cdp_connect(
        &mut self,
        tool_name: &str,
        arguments: &Value,
        result_text: &str,
        mcp: &(impl Mcp + ?Sized),
    ) {
        if tool_name != "launch_app" && tool_name != "focus_window" {
            // The tool might still be `quit_app` — keep CDP state in
            // lock-step with the underlying process. Executor-side
            // ai_step.rs already performs the same bookkeeping for
            // mid-run quit calls.
            if tool_name == "quit_app"
                && let Some(name) = arguments.get("app_name").and_then(Value::as_str)
            {
                self.cdp_state.mark_app_quit(name);
            }
            return;
        }
        let Some((app_name, kind_hint)) =
            Self::resolve_cdp_target(arguments, result_text, mcp).await
        else {
            return;
        };
        // Stash the kind so subsequent `focus_window` calls against this
        // app can be suppressed when AX dispatch is available — see
        // [`Self::should_skip_focus_window`]. Must happen BEFORE the CDP
        // decision so the record is present even if CDP connect is
        // skipped (Native apps short-circuit `auto_connect_cdp`).
        if let Some(kind) = kind_hint.as_deref() {
            self.record_app_kind(&app_name, kind);
        }
        if let Some(cdp_port) = self
            .auto_connect_cdp(&app_name, kind_hint.as_deref(), mcp)
            .await
        {
            self.emit_event(AgentEvent::CdpConnected {
                app_name: app_name.clone(),
                port: cdp_port,
            })
            .await;
            // Refresh the client-side cache (not the agent's LLM tools
            // vec) so observation gates like `has_tool("cdp_find_elements")`
            // see tools surfaced by the server post-connect.
            if let Err(e) = mcp.refresh_server_tool_list().await {
                warn!(error = %e, "Post-CDP-connect client tool-cache refresh failed");
            }
        }
    }

    /// Resolve the app identity for CDP probing from a successful
    /// `focus_window` / `launch_app` call. Returns `(app_name, kind)`
    /// where `kind` is a pre-classified `AppKind` string
    /// (`"ElectronApp"`, `"ChromeBrowser"`, `"Native"`) when the MCP
    /// server already told us, and `None` when we'll need `probe_app`
    /// to classify.
    ///
    /// Resolution order (fastest first):
    /// 1. **Structured response from MCP.** Modern `focus_window` returns
    ///    `{"app_name", "pid", "kind", ...}` JSON — one parse, no extra
    ///    MCP calls, and the `kind` lets `auto_connect_cdp` skip the
    ///    `probe_app` round-trip entirely.
    /// 2. **`arguments["app_name"]`** — fast path for the
    ///    `{"app_name": "..."}` variant. No `kind` hint.
    /// 3. **`pid` → `list_apps`** — fallback for older MCP versions
    ///    that still return the plain "Window focused successfully"
    ///    text for `focus_window`.
    /// 4. **`window_id` → `list_windows`** — same fallback.
    ///
    /// Returns `None` when no variant matches or lookups fail. All
    /// failures are logged at `debug!` — best-effort; the agent loop
    /// continues without CDP when we can't resolve.
    async fn resolve_cdp_target(
        arguments: &Value,
        result_text: &str,
        mcp: &(impl Mcp + ?Sized),
    ) -> Option<(String, Option<String>)> {
        // 1. Structured MCP response (modern focus_window / launch_app).
        if let Ok(parsed) = serde_json::from_str::<Value>(result_text)
            && let Some(name) = parsed.get("app_name").and_then(Value::as_str)
            && !name.is_empty()
        {
            let kind = parsed
                .get("kind")
                .and_then(Value::as_str)
                .map(str::to_owned);
            return Some((name.to_string(), kind));
        }
        // 2. Direct argument (fast, backwards-compatible).
        if let Some(name) = arguments["app_name"].as_str() {
            return Some((name.to_string(), None));
        }
        if let Some(pid) = arguments["pid"].as_u64()
            && mcp.has_tool("list_apps")
        {
            match mcp
                .call_tool("list_apps", Some(serde_json::json!({})))
                .await
            {
                Ok(r) if r.is_error != Some(true) => {
                    // JSON payload from list_apps — use first-text-block
                    // semantics so trailing prose blocks don't break parsing.
                    let text = crate::cdp_lifecycle::extract_text(&r);
                    if let Ok(entries) = serde_json::from_str::<Vec<Value>>(&text)
                        && let Some(name) = entries.iter().find_map(|entry| {
                            if entry["pid"].as_u64() == Some(pid) {
                                entry["name"].as_str().map(str::to_owned)
                            } else {
                                None
                            }
                        })
                    {
                        return Some((name, None));
                    }
                    debug!(
                        pid,
                        "list_apps returned no entry matching pid for CDP resolution"
                    );
                }
                Ok(r) => {
                    debug!(
                        error = %extract_result_text(&r),
                        "list_apps returned error during CDP app-name resolution",
                    );
                }
                Err(e) => {
                    debug!(error = %e, "list_apps call failed during CDP app-name resolution");
                }
            }
        }
        if let Some(window_id) = arguments["window_id"].as_u64()
            && mcp.has_tool("list_windows")
        {
            match mcp
                .call_tool("list_windows", Some(serde_json::json!({})))
                .await
            {
                Ok(r) if r.is_error != Some(true) => {
                    // JSON payload from list_windows — first-text-block only.
                    let text = crate::cdp_lifecycle::extract_text(&r);
                    if let Ok(entries) = serde_json::from_str::<Vec<Value>>(&text)
                        && let Some(name) = entries.iter().find_map(|entry| {
                            if entry["id"].as_u64() == Some(window_id) {
                                entry["owner_name"]
                                    .as_str()
                                    .or_else(|| entry["name"].as_str())
                                    .map(str::to_owned)
                            } else {
                                None
                            }
                        })
                    {
                        return Some((name, None));
                    }
                    debug!(
                        window_id,
                        "list_windows returned no entry matching window_id for CDP resolution",
                    );
                }
                Ok(r) => {
                    debug!(
                        error = %extract_result_text(&r),
                        "list_windows returned error during CDP app-name resolution",
                    );
                }
                Err(e) => {
                    debug!(error = %e, "list_windows call failed during CDP app-name resolution");
                }
            }
        }
        None
    }

    /// Parse the LLM response and execute the chosen action.
    #[allow(clippy::too_many_arguments)]
    async fn execute_response(
        &self,
        message: &clickweave_llm::Message,
        mcp: &(impl Mcp + ?Sized),
        _goal: &str,
        _elements: &[CdpFindElementMatch],
        _mcp_tools: &[Value],
        step_index: usize,
        annotations_by_tool: &HashMap<String, ToolAnnotations>,
    ) -> Result<(AgentCommand, StepOutcome), LoopError> {
        // Check for tool calls
        if let Some(tool_calls) = &message.tool_calls {
            if tool_calls.len() > 1 {
                warn!(
                    count = tool_calls.len(),
                    "LLM returned multiple tool calls — only the first will be executed"
                );
            }
            if let Some(tc) = tool_calls.first() {
                if let Value::String(raw) = &tc.function.arguments {
                    warn!(
                        tool = %tc.function.name,
                        raw = %raw,
                        "Malformed tool-call arguments from LLM"
                    );
                    let command = AgentCommand::ToolCall {
                        tool_name: tc.function.name.clone(),
                        arguments: Value::Null,
                        tool_call_id: tc.id.clone(),
                    };
                    return Ok((
                        command,
                        StepOutcome::Error(format!("Malformed tool arguments: {}", raw)),
                    ));
                }
                let args: Value = tc.function.arguments.clone();

                // Handle pseudo-tools
                match tc.function.name.as_str() {
                    "agent_done" => {
                        let summary = args["summary"]
                            .as_str()
                            .unwrap_or("Goal completed")
                            .to_string();
                        return Ok((
                            AgentCommand::Done {
                                summary: summary.clone(),
                            },
                            StepOutcome::Done(summary),
                        ));
                    }
                    "agent_replan" => {
                        let reason = args["reason"]
                            .as_str()
                            .unwrap_or("Unknown reason")
                            .to_string();
                        return Ok((
                            AgentCommand::Replan {
                                reason: reason.clone(),
                            },
                            StepOutcome::Replan(reason),
                        ));
                    }
                    _ => {}
                }

                // Runner-side guard for focus-stealing focus_window calls.
                // The system prompt already tells the LLM not to precede
                // AX / CDP dispatch with `focus_window`, but local models
                // ignore that guidance non-deterministically. Three skip
                // paths collapse into one guard:
                //   - User policy (`allow_focus_window == false`) →
                //     unconditional suppression, no probe for app kind.
                //   - Native + full AX dispatch toolset → AX dispatch is
                //     focus-preserving.
                //   - Electron / Chrome-browser + live CDP session + CDP
                //     dispatch toolset → CDP operates on backgrounded
                //     windows without stealing focus.
                // See `should_skip_focus_window` for the exact conditions;
                // with the policy flag `true` (default), first-ever
                // calls, CDP-pre-connect flows, and unknown kinds still
                // execute normally so cross-platform and
                // connect-on-first-focus paths are untouched.
                if tc.function.name == "focus_window"
                    && let Some(reason) = self.should_skip_focus_window(&args, mcp)
                {
                    self.emit_event(AgentEvent::SubAction {
                        tool_name: "focus_window".to_string(),
                        summary: reason.sub_action_summary().to_string(),
                    })
                    .await;
                    let debug_app = args.get("app_name").and_then(|v| v.as_str()).unwrap_or("");
                    debug!(
                        tool = "focus_window",
                        app = debug_app,
                        reason = reason.llm_message(),
                        "Suppressing focus_window",
                    );
                    let command = AgentCommand::ToolCall {
                        tool_name: tc.function.name.clone(),
                        arguments: args.clone(),
                        tool_call_id: tc.id.clone(),
                    };
                    return Ok((
                        command,
                        StepOutcome::Success(reason.llm_message().to_string()),
                    ));
                }

                // Request user approval before executing the tool.
                // Observation-only tools bypass approval entirely; for
                // everything else, the permission policy decides whether
                // to prompt (Ask), skip the prompt (Allow), or hard-reject
                // (Deny).
                let needs_approval =
                    !Self::is_observation_tool(&tc.function.name, annotations_by_tool);
                if needs_approval {
                    let policy_action =
                        self.policy_for(&tc.function.name, &args, annotations_by_tool);
                    match policy_action {
                        PermissionAction::Deny => {
                            warn!(
                                tool = %tc.function.name,
                                "Tool denied by permission policy"
                            );
                            let command = AgentCommand::ToolCall {
                                tool_name: tc.function.name.clone(),
                                arguments: args.clone(),
                                tool_call_id: tc.id.clone(),
                            };
                            let err_msg =
                                format!("Tool `{}` denied by permission policy", tc.function.name);
                            return Ok((command, StepOutcome::Error(err_msg)));
                        }
                        PermissionAction::Allow => {
                            // Skip the approval prompt entirely; the
                            // policy pre-authorized this invocation.
                            debug!(
                                tool = %tc.function.name,
                                "Permission policy allowed tool — skipping approval"
                            );
                        }
                        PermissionAction::Ask => {
                            match self
                                .request_approval(&tc.function.name, &args, step_index, "")
                                .await
                            {
                                Some(ApprovalResult::Rejected) => {
                                    let command = AgentCommand::ToolCall {
                                        tool_name: tc.function.name.clone(),
                                        arguments: args.clone(),
                                        tool_call_id: tc.id.clone(),
                                    };
                                    return Ok((
                                        command,
                                        StepOutcome::Replan("User rejected action".to_string()),
                                    ));
                                }
                                Some(ApprovalResult::Unavailable) => {
                                    return Err(LoopError::ApprovalUnavailable);
                                }
                                // Approved or no gate configured
                                _ => {}
                            }
                        }
                    }
                }

                // Execute the MCP tool
                let command = AgentCommand::ToolCall {
                    tool_name: tc.function.name.clone(),
                    arguments: args.clone(),
                    tool_call_id: tc.id.clone(),
                };

                match mcp.call_tool(&tc.function.name, Some(args)).await {
                    Ok(result) => {
                        let is_error = result.is_error.unwrap_or(false);
                        let text = extract_result_text(&result);
                        if is_error {
                            Ok((command, StepOutcome::Error(text)))
                        } else {
                            Ok((command, StepOutcome::Success(text)))
                        }
                    }
                    Err(e) => Ok((command, StepOutcome::Error(e.to_string()))),
                }
            } else {
                Ok((
                    AgentCommand::TextOnly {
                        text: "Empty tool calls".to_string(),
                    },
                    StepOutcome::Error("LLM returned empty tool calls".to_string()),
                ))
            }
        } else {
            // Text-only response (no tool call)
            let text = message.content_text().unwrap_or("No response").to_string();
            Ok((
                AgentCommand::TextOnly { text: text.clone() },
                StepOutcome::Error(format!("LLM did not call a tool: {}", text)),
            ))
        }
    }

    /// Hardcoded fallback list of observation-only tools.
    ///
    /// A tool classed as observation is exempt from approval prompts and is
    /// neither cached nor turned into a workflow node. The list exists to
    /// cover MCP manifests that predate `readOnlyHint` annotations — once
    /// annotations flow in, the union with `read_only_hint == Some(true)`
    /// supersedes this list. See [`Self::is_observation_tool`] for the
    /// full precedence (hardcoded OR annotation, minus
    /// `CONFIRMABLE_TOOLS`).
    const OBSERVATION_TOOLS: &'static [&'static str] = &[
        "take_screenshot",
        "list_apps",
        "list_windows",
        "find_text",
        "find_image",
        "element_at_point",
        "take_ax_snapshot",
        "probe_app",
        "get_displays",
        "start_recording",
        "start_hover_tracking",
        "load_image",
        "cdp_list_pages",
        "cdp_take_snapshot",
        "cdp_find_elements",
        "android_list_devices",
    ];

    /// AX dispatch tools whose `uid` argument is a
    /// snapshot-generation-scoped handle. The server rejects these uids as
    /// `snapshot_expired` after the next `take_ax_snapshot`, so caching the
    /// raw MCP args and replaying them verbatim is never correct. The
    /// executor's deterministic path re-resolves descriptors on every
    /// invocation — the agent-loop cache must *not* short-circuit that.
    const AX_DISPATCH_TOOLS: &'static [&'static str] = &["ax_click", "ax_set_value", "ax_select"];

    /// Tools that transition app-level foreground / connection state rather
    /// than act on page content. Their cache key (the pre-state page
    /// elements) is inherently mismatched with their effect (a new state),
    /// and the next iteration's `fetch_elements` can still return the
    /// pre-transition CDP snapshot before the auto-connect hook
    /// re-targets — which caused the cached decision to replay immediately
    /// after the LLM issued it, doubling each `step_completed` event and
    /// spawning duplicate workflow nodes.
    const STATE_TRANSITION_TOOLS: &'static [&'static str] = &[
        "launch_app",
        "focus_window",
        "quit_app",
        "cdp_connect",
        "cdp_disconnect",
    ];

    /// True when the tool is an AX dispatch tool whose cached arguments
    /// carry a stale uid and therefore must never replay from the decision
    /// cache. See [`Self::AX_DISPATCH_TOOLS`] for the rationale.
    fn is_ax_dispatch_tool(tool_name: &str) -> bool {
        Self::AX_DISPATCH_TOOLS.contains(&tool_name)
    }

    /// True when the tool transitions app/window/CDP state and therefore
    /// must not be cache-replayed: the cache key reflects the pre-state,
    /// so a replay re-runs the transition against unchanged elements.
    /// See [`Self::STATE_TRANSITION_TOOLS`].
    fn is_state_transition_tool(tool_name: &str) -> bool {
        Self::STATE_TRANSITION_TOOLS.contains(&tool_name)
    }

    /// macOS AX-dispatch toolset — every tool required for the
    /// focus-preserving automation path. When the MCP server advertises
    /// **all** of these plus `take_ax_snapshot`, the agent can drive
    /// native apps without moving the cursor or raising windows, which
    /// makes a preceding `focus_window` call redundant (and focus-stealing).
    /// See [`Self::should_skip_focus_window`] for the guard that consumes
    /// this list.
    const AX_DISPATCH_TOOLSET: &'static [&'static str] =
        &["take_ax_snapshot", "ax_click", "ax_set_value", "ax_select"];

    /// Minimum CDP dispatch toolset required before the runner may
    /// suppress a `focus_window` against an Electron / Chrome-browser
    /// target. Deliberately conservative: `cdp_find_elements` + `cdp_click`
    /// is enough to prove that the agent's next move will operate against
    /// the CDP target (all CDP operations are focus-preserving — they run
    /// against backgrounded windows). Servers missing these tools fall
    /// through to the real `focus_window` call because the agent would
    /// otherwise be left with only coordinate-based tools that *do* need
    /// focus. See [`Self::should_skip_focus_window`].
    const CDP_DISPATCH_TOOLSET: &'static [&'static str] = &["cdp_find_elements", "cdp_click"];

    /// True when `(tool_name, result_text)` pair identifies a
    /// runner-skipped `focus_window` — i.e. one of the synthetic successes
    /// this guard produces (AX-toolset, CDP-live, or user-policy path).
    /// Post-step bookkeeping (CDP auto-connect, workflow-node creation)
    /// consults this so the skipped call stays invisible to both the CDP
    /// lifecycle and the graph.
    fn is_synthetic_focus_skip(tool_name: &str, result_text: &str) -> bool {
        tool_name == "focus_window" && FocusSkipReason::from_llm_message(result_text).is_some()
    }

    /// True when every member of `toolset` is advertised by the MCP
    /// server. Callers use this to gate the focus-window skip — missing
    /// any member of the relevant dispatch family means the agent can't
    /// drive the target through that family, so `focus_window` still
    /// matters.
    fn mcp_has_toolset(mcp: &(impl Mcp + ?Sized), toolset: &[&str]) -> bool {
        toolset.iter().all(|name| mcp.has_tool(name))
    }

    /// Record a per-app kind hint learned from a structured MCP response
    /// (modern `focus_window` / `launch_app`) or from `probe_app`. Kept
    /// as a method so [`Self::maybe_cdp_connect`] can stash whatever
    /// `resolve_cdp_target` surfaced before the CDP decision runs.
    fn record_app_kind(&mut self, app_name: &str, kind: &str) {
        self.known_app_kinds
            .insert(app_name.to_string(), kind.to_string());
    }

    /// Decide whether to suppress a `focus_window` MCP call.
    ///
    /// Returns a [`FocusSkipReason`] in three cases; the first is the
    /// unconditional user-policy short-circuit, the other two require an
    /// `app_name` in `arguments` (window-id / pid-only calls are
    /// ambiguous and always defer to the real tool under the defer path):
    ///
    /// 1. **User policy — `allow_focus_window == false`.** Operator
    ///    opted into a background-run policy; every `focus_window` is
    ///    suppressed with no probe for app kind and no CDP-connected
    ///    check. Takes precedence over every other branch. Returns
    ///    [`FocusSkipReason::PolicyDisabled`]; its LLM-facing text nudges
    ///    the model toward AX / CDP dispatch primitives instead of
    ///    coordinate tools (which genuinely need focus).
    ///
    /// 2. **Native + full AX dispatch toolset.** The MCP server advertises
    ///    every [`Self::AX_DISPATCH_TOOLSET`] tool AND
    ///    [`Self::known_app_kinds`] previously recorded the app as
    ///    `"Native"`. AX dispatch is focus-preserving, so the real
    ///    `focus_window` would only steal foreground from the user.
    ///    Returns [`FocusSkipReason::AxAvailable`].
    ///
    /// 3. **Electron / Chrome-browser + live CDP session + CDP toolset.**
    ///    [`Self::known_app_kinds`] recorded the app as `"ElectronApp"` or
    ///    `"ChromeBrowser"`, the shared [`CdpState`][`crate::cdp_lifecycle::CdpState`]
    ///    reports a live session bound to this app, AND the MCP server
    ///    advertises the minimum CDP dispatch toolset
    ///    ([`Self::CDP_DISPATCH_TOOLSET`]). CDP dispatch operates on
    ///    backgrounded windows, so `focus_window` is redundant. Crucially
    ///    we only skip when a session is *already* live — the first
    ///    `focus_window` against an Electron app often precedes
    ///    `cdp_connect` and may be needed to bring the window front
    ///    before the port is found. Returns [`FocusSkipReason::CdpLive`].
    ///
    /// Returns `None` in every other case — erring on the side of
    /// executing `focus_window` normally so cross-platform workflows,
    /// first-ever-focus calls, and CDP-pre-connect flows keep working.
    fn should_skip_focus_window(
        &self,
        arguments: &Value,
        mcp: &(impl Mcp + ?Sized),
    ) -> Option<FocusSkipReason> {
        // User-policy short-circuit takes precedence over kind/toolset
        // checks — the operator explicitly asked for "no focus changes,
        // ever" and we must honor that regardless of what the MCP server
        // advertises or which app kind we've seen so far.
        if !self.config.allow_focus_window {
            return Some(FocusSkipReason::PolicyDisabled);
        }
        let app_name = arguments.get("app_name").and_then(Value::as_str)?;
        match self.known_app_kinds.get(app_name).map(String::as_str) {
            Some("Native") if Self::mcp_has_toolset(mcp, Self::AX_DISPATCH_TOOLSET) => {
                Some(FocusSkipReason::AxAvailable)
            }
            Some("ElectronApp" | "ChromeBrowser")
                // Agent-side CDP tracking uses PID=0 as a placeholder
                // (see `on_cdp_connected`), so name-scoped lookup via
                // `is_connected_to` is the authoritative is-connected
                // predicate here.
                if self.cdp_state.is_connected_to(app_name, 0)
                    && Self::mcp_has_toolset(mcp, Self::CDP_DISPATCH_TOOLSET) =>
            {
                Some(FocusSkipReason::CdpLive)
            }
            _ => None,
        }
    }

    /// Decide whether a tool should be treated as observation-only for the
    /// purposes of approval gating, cache eligibility, and workflow-node
    /// inclusion.
    ///
    /// Precedence:
    /// 1. Any tool in [`clickweave_core::permissions::CONFIRMABLE_TOOLS`]
    ///    (`launch_app`, `quit_app`, `cdp_connect`) is **never** observation
    ///    — destructive side effects that always warrant user consent.
    /// 2. Otherwise, observation if the tool appears in
    ///    [`Self::OBSERVATION_TOOLS`] (hardcoded allowlist) **or** the MCP
    ///    server advertises `readOnlyHint = true` for it.
    ///
    /// Callers hand in a reference to the per-run `annotations_by_tool`
    /// index so the decision is consistent with the permission policy
    /// evaluated elsewhere — both branches read the same `ToolAnnotations`.
    fn is_observation_tool(
        tool_name: &str,
        annotations_by_tool: &HashMap<String, ToolAnnotations>,
    ) -> bool {
        if clickweave_core::permissions::CONFIRMABLE_TOOLS
            .iter()
            .any(|(n, _)| *n == tool_name)
        {
            return false;
        }
        if Self::OBSERVATION_TOOLS.contains(&tool_name) {
            return true;
        }
        annotations_by_tool
            .get(tool_name)
            .and_then(|a| a.read_only_hint)
            .unwrap_or(false)
    }

    /// Add a workflow node for the executed tool call. Returns the UUID
    /// of the produced node, or `None` if the tool was observation-only
    /// or the tool-to-node-type mapping failed.
    /// Skips observation-only tools that the agent uses for perception.
    async fn add_workflow_node(
        &mut self,
        tool_name: &str,
        arguments: &Value,
        known_tools: &[Value],
        annotations_by_tool: &HashMap<String, ToolAnnotations>,
    ) -> Option<uuid::Uuid> {
        if Self::is_observation_tool(tool_name, annotations_by_tool) {
            return None;
        }
        let mut node_type = match tool_invocation_to_node_type(tool_name, arguments, known_tools) {
            Ok(nt) => nt,
            Err(e) => {
                warn!(error = %e, tool = tool_name, "Could not map tool to workflow node type — workflow graph will be incomplete");
                self.emit_event(AgentEvent::Warning {
                    message: format!("Failed to map tool '{}' to workflow node: {}", tool_name, e),
                })
                .await;
                return None;
            }
        };

        // AX dispatch descriptor enrichment. The tool-mapping inbound path
        // has no access to the AX snapshot that the agent saw, so it writes
        // `AxTarget::ResolvedUid(uid)`. Here — where we *do* have the recent
        // transcript — walk back to the nearest `take_ax_snapshot` result
        // and upgrade to `Descriptor { role, name, parent_name }` so the
        // node replays correctly after a fresh snapshot (which will have a
        // different generation and therefore a different uid).
        self.enrich_ax_descriptor(&mut node_type);

        let position = Position {
            x: 0.0,
            y: (self.state.workflow.nodes.len() as f32) * 120.0,
        };
        let node = Node::new(node_type, position, tool_name, "").with_run_id(self.run_id);
        let node_id = node.id;

        // Emit live node event before pushing (clone for the event)
        self.emit_event(AgentEvent::NodeAdded {
            node: Box::new(node.clone()),
        })
        .await;

        self.state.workflow.nodes.push(node);

        // Connect to previous node
        if let Some(prev_id) = self.state.last_node_id {
            let edge = Edge {
                from: prev_id,
                to: node_id,
            };
            self.emit_event(AgentEvent::EdgeAdded { edge: edge.clone() })
                .await;
            self.state.workflow.edges.push(edge);
        }

        self.state.last_node_id = Some(node_id);
        Some(node_id)
    }

    /// Upgrade an `AxClick` / `AxSetValue` / `AxSelect` node's target from
    /// the raw uid emitted by the LLM (already stored as
    /// `AxTarget::ResolvedUid`) into a replay-stable descriptor, using the
    /// most recent `take_ax_snapshot` result in the agent transcript.
    ///
    /// Thin wrapper over [`upgrade_ax_target_from_snapshot`] that looks up
    /// the snapshot from `self.state.steps`.
    fn enrich_ax_descriptor(&self, node_type: &mut clickweave_core::NodeType) {
        let Some(snapshot_text) = most_recent_ax_snapshot_text(&self.state.steps) else {
            return;
        };
        upgrade_ax_target_from_snapshot(node_type, &snapshot_text);
    }
}

/// Pure helper: scan agent steps from newest to oldest, return the most
/// recent `take_ax_snapshot` success-outcome text. `None` if no such step
/// exists. Pulled out of [`AgentRunner`] so unit tests can assert the
/// lookup without constructing a full runner.
fn most_recent_ax_snapshot_text(steps: &[super::types::AgentStep]) -> Option<String> {
    use super::types::{AgentCommand, StepOutcome};
    for step in steps.iter().rev() {
        let AgentCommand::ToolCall { tool_name, .. } = &step.command else {
            continue;
        };
        if tool_name != "take_ax_snapshot" {
            continue;
        }
        if let StepOutcome::Success(text) = &step.outcome {
            return Some(text.clone());
        }
    }
    None
}

/// Pure helper: if `node_type` is an AX dispatch variant with a
/// `ResolvedUid` target, and `snapshot_text` contains a matching entry,
/// replace the target with a `Descriptor`. No-op otherwise.
///
/// Silent no-ops are preferable to hard failures — the executor's
/// descriptor resolution will surface a clear error later if the uid
/// genuinely cannot be replayed. The upgrade is "best effort at record
/// time."
fn upgrade_ax_target_from_snapshot(node_type: &mut clickweave_core::NodeType, snapshot_text: &str) {
    use clickweave_core::{AxTarget, NodeType};

    let target: &mut AxTarget = match node_type {
        NodeType::AxClick(p) => &mut p.target,
        NodeType::AxSetValue(p) => &mut p.target,
        NodeType::AxSelect(p) => &mut p.target,
        _ => return,
    };

    let uid = match target {
        AxTarget::ResolvedUid(uid) if !uid.is_empty() => uid.clone(),
        _ => return,
    };

    let entries = crate::executor::deterministic::ax::parse_ax_snapshot(snapshot_text);
    let Some(entry) = entries.into_iter().find(|e| e.uid == uid) else {
        return;
    };
    *target = AxTarget::Descriptor {
        role: entry.role,
        name: entry.name.unwrap_or_default(),
        parent_name: entry.parent_name,
    };
}

#[cfg(test)]
mod ax_enrichment_tests {
    use super::super::types::{AgentCommand, AgentStep, StepOutcome};
    use super::{most_recent_ax_snapshot_text, upgrade_ax_target_from_snapshot};
    use clickweave_core::{AxClickParams, AxSelectParams, AxSetValueParams, AxTarget, NodeType};

    fn tool_step(tool_name: &str, outcome: StepOutcome) -> AgentStep {
        AgentStep {
            index: 0,
            elements: Vec::new(),
            command: AgentCommand::ToolCall {
                tool_name: tool_name.to_string(),
                arguments: serde_json::json!({}),
                tool_call_id: "call_0".to_string(),
            },
            outcome,
            page_url: String::new(),
        }
    }

    #[test]
    fn most_recent_snapshot_returns_latest_success() {
        let steps = vec![
            tool_step(
                "take_ax_snapshot",
                StepOutcome::Success("uid=a1g1 AXButton \"Old\"".into()),
            ),
            tool_step("click", StepOutcome::Success("ok".into())),
            tool_step(
                "take_ax_snapshot",
                StepOutcome::Success("uid=a2g2 AXButton \"New\"".into()),
            ),
        ];
        let text = most_recent_ax_snapshot_text(&steps).expect("most recent should be found");
        assert!(text.contains("New"));
    }

    #[test]
    fn most_recent_snapshot_ignores_non_ax_tools_and_failures() {
        let steps = vec![
            tool_step(
                "take_ax_snapshot",
                StepOutcome::Success("uid=a1g1 AXButton \"First\"".into()),
            ),
            tool_step(
                "take_ax_snapshot",
                StepOutcome::Error("snapshot failed".into()),
            ),
            tool_step("click", StepOutcome::Success("unrelated".into())),
        ];
        // Should return the only successful snapshot, skipping the later
        // failure (error outcome) and the unrelated `click` step.
        let text = most_recent_ax_snapshot_text(&steps).expect("fall back to older success");
        assert!(text.contains("First"));
    }

    #[test]
    fn most_recent_snapshot_returns_none_when_no_snapshots() {
        let steps = vec![tool_step("click", StepOutcome::Success("ok".into()))];
        assert!(most_recent_ax_snapshot_text(&steps).is_none());
    }

    #[test]
    fn upgrade_ax_click_resolved_uid_to_descriptor() {
        let mut nt = NodeType::AxClick(AxClickParams {
            target: AxTarget::ResolvedUid("a5g2".into()),
            ..Default::default()
        });
        let snapshot = "uid=a5g2 AXButton \"Continue\"\n";
        upgrade_ax_target_from_snapshot(&mut nt, snapshot);
        match nt {
            NodeType::AxClick(p) => assert_eq!(
                p.target,
                AxTarget::Descriptor {
                    role: "AXButton".into(),
                    name: "Continue".into(),
                    parent_name: None,
                }
            ),
            _ => panic!("expected AxClick"),
        }
    }

    #[test]
    fn upgrade_ax_set_value_preserves_value_field() {
        let mut nt = NodeType::AxSetValue(AxSetValueParams {
            target: AxTarget::ResolvedUid("a10g1".into()),
            value: "preserved".into(),
            ..Default::default()
        });
        let snapshot = "uid=a10g1 AXTextField \"Email\"\n";
        upgrade_ax_target_from_snapshot(&mut nt, snapshot);
        match nt {
            NodeType::AxSetValue(p) => {
                assert_eq!(p.value, "preserved");
                assert_eq!(
                    p.target,
                    AxTarget::Descriptor {
                        role: "AXTextField".into(),
                        name: "Email".into(),
                        parent_name: None,
                    }
                );
            }
            _ => panic!("expected AxSetValue"),
        }
    }

    #[test]
    fn upgrade_preserves_parent_name_for_outline_rows() {
        // NSOutlineView rows often share (role, name) across sections, so
        // the parent anchor is what makes the descriptor unambiguous.
        let mut nt = NodeType::AxSelect(AxSelectParams {
            target: AxTarget::ResolvedUid("a3g1".into()),
            ..Default::default()
        });
        let snapshot = concat!(
            "uid=a1g1 AXOutline \"Sidebar\"\n",
            "  uid=a2g1 AXGroup \"Network\"\n",
            "    uid=a3g1 AXRow \"Wi-Fi\"\n",
        );
        upgrade_ax_target_from_snapshot(&mut nt, snapshot);
        match nt {
            NodeType::AxSelect(p) => assert_eq!(
                p.target,
                AxTarget::Descriptor {
                    role: "AXRow".into(),
                    name: "Wi-Fi".into(),
                    parent_name: Some("Network".into()),
                }
            ),
            _ => panic!("expected AxSelect"),
        }
    }

    #[test]
    fn upgrade_is_noop_when_uid_not_in_snapshot() {
        let mut nt = NodeType::AxClick(AxClickParams {
            target: AxTarget::ResolvedUid("a99g9".into()),
            ..Default::default()
        });
        let snapshot = "uid=a1g1 AXButton \"Other\"\n";
        upgrade_ax_target_from_snapshot(&mut nt, snapshot);
        match nt {
            NodeType::AxClick(p) => {
                assert_eq!(p.target, AxTarget::ResolvedUid("a99g9".into()));
            }
            _ => panic!("expected AxClick"),
        }
    }

    #[test]
    fn upgrade_is_noop_for_non_ax_nodes() {
        let mut nt = NodeType::McpToolCall(clickweave_core::McpToolCallParams {
            tool_name: "click".into(),
            arguments: serde_json::json!({}),
        });
        let snapshot = "uid=a1g1 AXButton \"X\"\n";
        upgrade_ax_target_from_snapshot(&mut nt, snapshot);
        assert!(matches!(nt, NodeType::McpToolCall(_)));
    }

    #[test]
    fn upgrade_is_noop_for_already_descriptor_target() {
        let original = AxTarget::Descriptor {
            role: "AXButton".into(),
            name: "OK".into(),
            parent_name: None,
        };
        let mut nt = NodeType::AxClick(AxClickParams {
            target: original.clone(),
            ..Default::default()
        });
        // Snapshot has a different mapping — shouldn't matter, we don't
        // touch Descriptor targets.
        let snapshot = "uid=a1g1 AXCheckbox \"Rogue\"\n";
        upgrade_ax_target_from_snapshot(&mut nt, snapshot);
        match nt {
            NodeType::AxClick(p) => assert_eq!(p.target, original),
            _ => panic!("expected AxClick"),
        }
    }
}

#[cfg(test)]
mod run_anchor_tests {
    use super::*;
    use uuid::Uuid;

    #[test]
    fn new_state_seeds_last_node_id_from_anchor() {
        // `AgentState::new` starts with `last_node_id = None`. The run()
        // method then overwrites it from the caller-supplied anchor so
        // the first emitted edge links into an existing chain.
        let anchor = Uuid::new_v4();
        let mut state = AgentState::new(Workflow::default());
        assert!(state.last_node_id.is_none());
        state.last_node_id = Some(anchor);
        assert_eq!(state.last_node_id, Some(anchor));
    }

    #[test]
    fn no_anchor_leaves_last_node_id_none() {
        let state = AgentState::new(Workflow::default());
        assert!(state.last_node_id.is_none());
    }
}

#[cfg(test)]
mod resolve_cdp_target_tests {
    use super::*;
    use crate::executor::Mcp;
    use clickweave_llm::{ChatOptions, ChatResponse};
    use clickweave_mcp::ToolCallResult;

    /// Minimal `ChatBackend` used only to satisfy the type parameter on
    /// `AgentRunner<'_, B>` when calling `resolve_cdp_target`, which
    /// itself doesn't touch the backend. All methods panic — the tests
    /// don't instantiate a runner, they call the associated fn
    /// directly, so these are never actually invoked.
    struct UnusedBackend;

    impl ChatBackend for UnusedBackend {
        async fn chat_with_options(
            &self,
            _messages: &[Message],
            _tools: Option<&[Value]>,
            _options: &ChatOptions,
        ) -> anyhow::Result<ChatResponse> {
            unreachable!("resolve_cdp_target does not call the LLM backend")
        }
        fn model_name(&self) -> &str {
            "unused"
        }
    }

    /// MCP stub that panics on any call. Every test in this module
    /// exercises paths (structured response, arguments-only) that must
    /// not reach MCP — the panic proves those paths don't regress to
    /// making extra round-trips.
    struct UnusedMcp;

    impl Mcp for UnusedMcp {
        async fn call_tool(
            &self,
            _name: &str,
            _arguments: Option<Value>,
        ) -> anyhow::Result<ToolCallResult> {
            panic!("resolve_cdp_target reached MCP on a fast-path case");
        }
        fn has_tool(&self, _name: &str) -> bool {
            false
        }
        fn tools_as_openai(&self) -> Vec<Value> {
            Vec::new()
        }
        async fn refresh_server_tool_list(&self) -> anyhow::Result<()> {
            Ok(())
        }
    }

    async fn resolve(arguments: Value, result_text: &str) -> Option<(String, Option<String>)> {
        // `resolve_cdp_target` doesn't depend on `self` or on the
        // backend parameter; pick any concrete backend to satisfy the
        // impl block's type parameter.
        AgentRunner::<UnusedBackend>::resolve_cdp_target(&arguments, result_text, &UnusedMcp).await
    }

    #[tokio::test]
    async fn structured_response_wins_over_pid_argument() {
        let arguments = serde_json::json!({ "pid": 16024 });
        let result_text = serde_json::json!({
            "app_name": "Signal",
            "pid": 16024,
            "bundle_id": "org.whispersystems.signal-desktop",
            "kind": "ElectronApp",
        })
        .to_string();
        let resolved = resolve(arguments, &result_text).await;
        assert_eq!(
            resolved,
            Some(("Signal".to_string(), Some("ElectronApp".to_string())))
        );
    }

    #[tokio::test]
    async fn plain_text_response_falls_back_to_arguments_app_name() {
        let arguments = serde_json::json!({ "app_name": "Signal" });
        let resolved = resolve(arguments, "Window focused successfully").await;
        assert_eq!(resolved, Some(("Signal".to_string(), None)));
    }

    #[tokio::test]
    async fn empty_app_name_in_structured_response_is_ignored() {
        let arguments = serde_json::json!({ "app_name": "Chrome" });
        let result_text = serde_json::json!({ "app_name": "", "pid": 0 }).to_string();
        let resolved = resolve(arguments, &result_text).await;
        assert_eq!(resolved, Some(("Chrome".to_string(), None)));
    }

    /// MCP stub that returns a fixed multi-text-block `list_apps` response.
    /// Pins the contract that the `pid → list_apps` CDP resolution path
    /// parses only the first text block: regression guard for a past bug
    /// where joining blocks with `\n` broke serde_json parsing whenever a
    /// server returned a JSON payload plus trailing prose.
    struct MultiBlockListAppsMcp;

    impl Mcp for MultiBlockListAppsMcp {
        async fn call_tool(
            &self,
            name: &str,
            _arguments: Option<Value>,
        ) -> anyhow::Result<ToolCallResult> {
            assert_eq!(name, "list_apps");
            Ok(ToolCallResult {
                content: vec![
                    clickweave_mcp::ToolContent::Text {
                        text: r#"[{"name":"Signal","pid":16024}]"#.to_string(),
                    },
                    clickweave_mcp::ToolContent::Text {
                        text: "(rendered from cached process table)".to_string(),
                    },
                ],
                is_error: None,
            })
        }
        fn has_tool(&self, name: &str) -> bool {
            name == "list_apps"
        }
        fn tools_as_openai(&self) -> Vec<Value> {
            Vec::new()
        }
        async fn refresh_server_tool_list(&self) -> anyhow::Result<()> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn pid_resolves_to_app_name_even_with_trailing_prose_block() {
        let arguments = serde_json::json!({ "pid": 16024 });
        let resolved = AgentRunner::<UnusedBackend>::resolve_cdp_target(
            &arguments,
            "Window focused successfully",
            &MultiBlockListAppsMcp,
        )
        .await;
        assert_eq!(resolved, Some(("Signal".to_string(), None)));
    }
}

#[cfg(test)]
mod observation_union_tests {
    //! Coverage for [`AgentRunner::is_observation_tool`], the
    //! hardcoded-list ∪ readOnlyHint ∖ CONFIRMABLE_TOOLS predicate that
    //! governs approval bypass, cache eligibility, and workflow-node
    //! inclusion. Only the type parameter `B` of `AgentRunner` matters
    //! for compile-time dispatch; the predicate never touches `&self`,
    //! so the tests call it through a concrete instantiation.
    use super::*;
    use clickweave_llm::{ChatOptions, ChatResponse};

    /// Zero-sized `ChatBackend` used only to instantiate
    /// `AgentRunner::<Backend>::is_observation_tool` at the call site.
    struct Backend;

    impl ChatBackend for Backend {
        async fn chat_with_options(
            &self,
            _messages: &[Message],
            _tools: Option<&[Value]>,
            _options: &ChatOptions,
        ) -> anyhow::Result<ChatResponse> {
            unimplemented!()
        }

        fn model_name(&self) -> &str {
            "observation-test-backend"
        }
    }

    fn is_observation(
        tool_name: &str,
        annotations_by_tool: &HashMap<String, ToolAnnotations>,
    ) -> bool {
        AgentRunner::<Backend>::is_observation_tool(tool_name, annotations_by_tool)
    }

    #[test]
    fn hardcoded_tool_is_observation_without_annotations() {
        let annotations: HashMap<String, ToolAnnotations> = HashMap::new();
        assert!(is_observation("take_screenshot", &annotations));
        assert!(is_observation("cdp_find_elements", &annotations));
    }

    #[test]
    fn readonly_hint_makes_novel_tool_observation() {
        // Tool not in the hardcoded list becomes observation once the MCP
        // manifest advertises `readOnlyHint = true`.
        let mut annotations = HashMap::new();
        annotations.insert(
            "custom_inspect".to_string(),
            ToolAnnotations {
                read_only_hint: Some(true),
                ..Default::default()
            },
        );
        assert!(is_observation("custom_inspect", &annotations));
    }

    #[test]
    fn missing_readonly_hint_is_not_observation() {
        // A tool with no annotations and not in the hardcoded list must
        // fall through to approval — the default-to-Ask path in the
        // permission policy depends on it.
        let annotations: HashMap<String, ToolAnnotations> = HashMap::new();
        assert!(!is_observation("click", &annotations));
        assert!(!is_observation("type_text", &annotations));
    }

    #[test]
    fn readonly_hint_false_is_not_observation() {
        let mut annotations = HashMap::new();
        annotations.insert(
            "custom_click".to_string(),
            ToolAnnotations {
                read_only_hint: Some(false),
                ..Default::default()
            },
        );
        assert!(!is_observation("custom_click", &annotations));
    }

    #[test]
    fn confirmable_tool_always_requires_approval_even_with_readonly_hint() {
        // Guardrail: the MCP server could (mis)advertise `launch_app` as
        // read-only, but it still has user-visible side effects. Our
        // hardcoded destructive list wins regardless.
        let mut annotations = HashMap::new();
        annotations.insert(
            "launch_app".to_string(),
            ToolAnnotations {
                read_only_hint: Some(true),
                ..Default::default()
            },
        );
        assert!(!is_observation("launch_app", &annotations));
        // Same for cdp_connect and quit_app:
        annotations.insert(
            "cdp_connect".to_string(),
            ToolAnnotations {
                read_only_hint: Some(true),
                ..Default::default()
            },
        );
        annotations.insert(
            "quit_app".to_string(),
            ToolAnnotations {
                read_only_hint: Some(true),
                ..Default::default()
            },
        );
        assert!(!is_observation("cdp_connect", &annotations));
        assert!(!is_observation("quit_app", &annotations));
    }

    #[test]
    fn extract_result_text_joins_all_text_blocks_for_transcript() {
        // Agent transcript must see every text block the tool returned.
        // Dropping later blocks silently hides data from the LLM and from
        // cache replay. JSON-parse sites use cdp_lifecycle::extract_text
        // instead.
        let result = clickweave_mcp::ToolCallResult {
            content: vec![
                clickweave_mcp::ToolContent::Text {
                    text: "[{\"x\": 1}]".to_string(),
                },
                clickweave_mcp::ToolContent::Text {
                    text: "trailing commentary".to_string(),
                },
            ],
            is_error: None,
        };
        assert_eq!(
            super::extract_result_text(&result),
            "[{\"x\": 1}]\ntrailing commentary"
        );
    }

    #[test]
    fn extract_result_text_placeholder_for_image_only_result() {
        let result = clickweave_mcp::ToolCallResult {
            content: vec![clickweave_mcp::ToolContent::Image {
                data: "b64data".to_string(),
                mime_type: "image/png".to_string(),
            }],
            is_error: None,
        };
        let text = super::extract_result_text(&result);
        assert!(text.contains("image"), "got {text:?}");
        assert!(text.contains("image/png"), "got {text:?}");
    }

    #[test]
    fn extract_result_text_empty_for_no_content() {
        let result = clickweave_mcp::ToolCallResult {
            content: vec![],
            is_error: None,
        };
        assert_eq!(super::extract_result_text(&result), "");
    }

    #[test]
    fn confirmable_tool_overrides_hardcoded_observation_list() {
        // Belt-and-braces: even if someone adds a CONFIRMABLE tool to
        // OBSERVATION_TOOLS by mistake, the guardrail still fires.
        // (`launch_app` is not in `OBSERVATION_TOOLS` today, but this test
        // pins the precedence rule independent of the specific list.)
        let annotations: HashMap<String, ToolAnnotations> = HashMap::new();
        assert!(!is_observation("launch_app", &annotations));
        assert!(!is_observation("quit_app", &annotations));
        assert!(!is_observation("cdp_connect", &annotations));
    }

    #[test]
    fn take_ax_snapshot_is_observation_but_ax_dispatch_is_not() {
        // Snapshot is read-only, should bypass the approval prompt and be
        // eligible for transcript-level collapse. The three dispatch tools
        // (ax_click / ax_set_value / ax_select) perform real side effects
        // even though the cursor doesn't move — they must stay in the
        // approval path, matching the MCP server's
        // `readOnlyHint: false` / `destructiveHint: false` annotations.
        let mut annotations: HashMap<String, ToolAnnotations> = HashMap::new();
        annotations.insert(
            "take_ax_snapshot".to_string(),
            ToolAnnotations {
                read_only_hint: Some(true),
                ..Default::default()
            },
        );
        annotations.insert(
            "ax_click".to_string(),
            ToolAnnotations {
                read_only_hint: Some(false),
                ..Default::default()
            },
        );
        annotations.insert(
            "ax_set_value".to_string(),
            ToolAnnotations {
                read_only_hint: Some(false),
                ..Default::default()
            },
        );
        annotations.insert(
            "ax_select".to_string(),
            ToolAnnotations {
                read_only_hint: Some(false),
                ..Default::default()
            },
        );
        assert!(is_observation("take_ax_snapshot", &annotations));
        assert!(!is_observation("ax_click", &annotations));
        assert!(!is_observation("ax_set_value", &annotations));
        assert!(!is_observation("ax_select", &annotations));
    }

    #[test]
    fn ax_dispatch_tools_are_not_cacheable() {
        // Cache eligibility on the write side AND replay on the read side
        // must skip AX dispatch tools — their `uid` argument is scoped to
        // one snapshot generation.
        assert!(AgentRunner::<Backend>::is_ax_dispatch_tool("ax_click"));
        assert!(AgentRunner::<Backend>::is_ax_dispatch_tool("ax_set_value"));
        assert!(AgentRunner::<Backend>::is_ax_dispatch_tool("ax_select"));
        // Snapshot and sibling tools are not dispatch tools.
        assert!(!AgentRunner::<Backend>::is_ax_dispatch_tool(
            "take_ax_snapshot"
        ));
        assert!(!AgentRunner::<Backend>::is_ax_dispatch_tool("click"));
        assert!(!AgentRunner::<Backend>::is_ax_dispatch_tool("type_text"));
    }

    #[test]
    fn state_transition_tools_are_not_cacheable() {
        // Cache eligibility on the write side AND replay on the read side
        // must skip state-transition tools. Their cache key encodes the
        // pre-transition page, so replay re-fires the transition against
        // unchanged elements — which caused double `step_completed` events
        // and duplicate workflow nodes after the LLM issued a `launch_app`
        // or `focus_window` that switched away from the current CDP target.
        assert!(AgentRunner::<Backend>::is_state_transition_tool(
            "launch_app"
        ));
        assert!(AgentRunner::<Backend>::is_state_transition_tool(
            "focus_window"
        ));
        assert!(AgentRunner::<Backend>::is_state_transition_tool("quit_app"));
        assert!(AgentRunner::<Backend>::is_state_transition_tool(
            "cdp_connect"
        ));
        assert!(AgentRunner::<Backend>::is_state_transition_tool(
            "cdp_disconnect"
        ));
        // Content-acting and observation tools are not state transitions.
        assert!(!AgentRunner::<Backend>::is_state_transition_tool(
            "cdp_click"
        ));
        assert!(!AgentRunner::<Backend>::is_state_transition_tool(
            "take_ax_snapshot"
        ));
        assert!(!AgentRunner::<Backend>::is_state_transition_tool(
            "ax_click"
        ));
    }

    // -----------------------------------------------------------------
    // focus_window skip guard
    // -----------------------------------------------------------------

    /// Minimal `Mcp` stub used to exercise the focus_window skip guard.
    /// Only `has_tool` is consulted by
    /// [`AgentRunner::should_skip_focus_window`] — `call_tool` /
    /// `tools_as_openai` / `refresh_server_tool_list` are never reached
    /// in these unit tests but must exist to satisfy the trait bound.
    struct ToolsetStub {
        tools: Vec<String>,
    }

    impl ToolsetStub {
        fn with(tools: &[&str]) -> Self {
            Self {
                tools: tools.iter().map(|s| s.to_string()).collect(),
            }
        }
    }

    impl crate::executor::Mcp for ToolsetStub {
        async fn call_tool(
            &self,
            _name: &str,
            _arguments: Option<Value>,
        ) -> anyhow::Result<clickweave_mcp::ToolCallResult> {
            unimplemented!("focus_window skip guard does not dispatch tools")
        }

        fn has_tool(&self, name: &str) -> bool {
            self.tools.iter().any(|t| t == name)
        }

        fn tools_as_openai(&self) -> Vec<Value> {
            Vec::new()
        }

        async fn refresh_server_tool_list(&self) -> anyhow::Result<()> {
            Ok(())
        }
    }

    /// Fresh runner pre-seeded with one app/kind hint for guard tests.
    fn runner_with_kind<'a>(
        backend: &'a Backend,
        app_name: &str,
        kind: &str,
    ) -> AgentRunner<'a, Backend> {
        let mut runner = AgentRunner::new(backend, AgentConfig::default());
        runner.record_app_kind(app_name, kind);
        runner
    }

    const FULL_AX_TOOLSET: &[&str] = &["take_ax_snapshot", "ax_click", "ax_set_value", "ax_select"];

    #[test]
    fn mcp_has_toolset_requires_every_member() {
        // Missing even one member blocks the guard. The guard only fires
        // when the full macOS AX dispatch toolset is present; on Windows
        // and on older MCP servers the set is incomplete and
        // focus_window still matters.
        let mcp_full = ToolsetStub::with(FULL_AX_TOOLSET);
        assert!(AgentRunner::<Backend>::mcp_has_toolset(
            &mcp_full,
            FULL_AX_TOOLSET,
        ));

        for (i, missing) in FULL_AX_TOOLSET.iter().enumerate() {
            let partial: Vec<&str> = FULL_AX_TOOLSET
                .iter()
                .enumerate()
                .filter_map(|(j, t)| (j != i).then_some(*t))
                .collect();
            let mcp = ToolsetStub::with(&partial);
            assert!(
                !AgentRunner::<Backend>::mcp_has_toolset(&mcp, FULL_AX_TOOLSET),
                "toolset without {} must not count as full AX toolset",
                missing,
            );
        }
    }

    #[test]
    fn should_skip_focus_window_fires_for_known_native_with_full_ax_toolset() {
        // Baseline happy path: MCP exposes the full AX toolset AND we've
        // already seen that the target is Native — suppress focus_window
        // to keep the user's foreground undisturbed.
        let backend = Backend;
        let runner = runner_with_kind(&backend, "Calculator", "Native");
        let mcp = ToolsetStub::with(FULL_AX_TOOLSET);
        let args = serde_json::json!({"app_name": "Calculator"});
        assert_eq!(
            runner.should_skip_focus_window(&args, &mcp),
            Some(FocusSkipReason::AxAvailable),
        );
    }

    #[test]
    fn should_skip_focus_window_defers_for_electron_or_chrome_without_live_cdp() {
        // Broader contract (see `should_skip_focus_window`): Electron /
        // Chrome apps DO qualify for the skip, but only after CDP is
        // live for that exact app. When no CDP session is bound yet,
        // the first `focus_window` call often precedes `cdp_connect`
        // and may be needed to bring the window front so the debug
        // port is discoverable. Without CDP live, the guard must defer
        // regardless of which dispatch toolset the MCP server exposes.
        //
        // NOTE: this test previously asserted that Electron / Chrome
        // apps were NEVER skipped. That narrower contract was relaxed
        // when CDP dispatch became the dominant path for these apps.
        // The test now covers the pre-CDP-connect half of the broader
        // contract; the post-CDP-connect half is covered by
        // `should_skip_focus_window_fires_for_electron_with_live_cdp`.
        let backend = Backend;
        // AX + CDP toolsets both present — the only thing missing is
        // the live CDP session, which is the point.
        let mcp = ToolsetStub::with(&[
            "take_ax_snapshot",
            "ax_click",
            "ax_set_value",
            "ax_select",
            "cdp_find_elements",
            "cdp_click",
        ]);
        for kind in ["ElectronApp", "ChromeBrowser"] {
            let runner = runner_with_kind(&backend, "VSCode", kind);
            let args = serde_json::json!({"app_name": "VSCode"});
            assert!(
                runner.should_skip_focus_window(&args, &mcp).is_none(),
                "focus_window must NOT be skipped for kind={} without a live CDP session",
                kind,
            );
        }
    }

    /// Seed a runner with a kind hint AND an active CDP session bound
    /// to the same app — the on-the-wire state the agent reaches after
    /// `launch_app` + successful `cdp_connect`. Delegates to
    /// [`AgentRunner::seed_cdp_live_for_test`] so the "post-`on_cdp_connected`
    /// state shape" has a single source of truth.
    fn runner_with_kind_and_cdp<'a>(
        backend: &'a Backend,
        app_name: &str,
        kind: &str,
    ) -> AgentRunner<'a, Backend> {
        let mut runner = AgentRunner::new(backend, AgentConfig::default());
        runner.seed_cdp_live_for_test(app_name, kind);
        runner
    }

    const FULL_CDP_TOOLSET: &[&str] = &["cdp_find_elements", "cdp_click"];

    #[test]
    fn should_skip_focus_window_fires_for_electron_with_live_cdp() {
        // CDP dispatch operates on backgrounded windows without stealing
        // focus, so once a session is live for the exact app, the real
        // `focus_window` is redundant and the guard must fire.
        let backend = Backend;
        let runner = runner_with_kind_and_cdp(&backend, "Signal", "ElectronApp");
        let mcp = ToolsetStub::with(FULL_CDP_TOOLSET);
        let args = serde_json::json!({"app_name": "Signal"});
        assert_eq!(
            runner.should_skip_focus_window(&args, &mcp),
            Some(FocusSkipReason::CdpLive),
        );
    }

    #[test]
    fn should_skip_focus_window_fires_for_chrome_browser_with_live_cdp() {
        // Same contract as the Electron path — ChromeBrowser targets
        // go through CDP and must be suppressed when a session is live.
        let backend = Backend;
        let runner = runner_with_kind_and_cdp(&backend, "Google Chrome", "ChromeBrowser");
        let mcp = ToolsetStub::with(FULL_CDP_TOOLSET);
        let args = serde_json::json!({"app_name": "Google Chrome"});
        assert_eq!(
            runner.should_skip_focus_window(&args, &mcp),
            Some(FocusSkipReason::CdpLive),
        );
    }

    #[test]
    fn should_skip_focus_window_defers_for_electron_when_cdp_not_connected() {
        // Kind hint + full CDP toolset but NO live session — the first
        // focus_window often precedes cdp_connect and may itself be
        // what brings the window front so the debug port is findable.
        // The guard must defer here.
        let backend = Backend;
        let runner = runner_with_kind(&backend, "Signal", "ElectronApp");
        let mcp = ToolsetStub::with(FULL_CDP_TOOLSET);
        let args = serde_json::json!({"app_name": "Signal"});
        assert!(runner.should_skip_focus_window(&args, &mcp).is_none());
    }

    #[test]
    fn should_skip_focus_window_defers_for_electron_when_cdp_tools_missing() {
        // CDP is live but the MCP server does not advertise the CDP
        // dispatch toolset (older server, stripped build). Without
        // cdp_find_elements / cdp_click the agent cannot drive the
        // target via CDP, so coordinate-based tools — which DO need
        // focus — are the likely fallback. The guard must defer.
        let backend = Backend;
        let runner = runner_with_kind_and_cdp(&backend, "Signal", "ElectronApp");
        // Only cdp_find_elements, missing cdp_click.
        let mcp = ToolsetStub::with(&["cdp_find_elements"]);
        let args = serde_json::json!({"app_name": "Signal"});
        assert!(runner.should_skip_focus_window(&args, &mcp).is_none());
    }

    #[test]
    fn should_skip_focus_window_defers_when_cdp_bound_to_other_app() {
        // A live CDP session bound to a different app must not authorize
        // a skip for this one — the name scope of `is_connected_to` is
        // load-bearing.
        let backend = Backend;
        let mut runner = AgentRunner::new(&backend, AgentConfig::default());
        runner.record_app_kind("Signal", "ElectronApp");
        runner.cdp_state.set_connected("Slack", 0);
        let mcp = ToolsetStub::with(FULL_CDP_TOOLSET);
        let args = serde_json::json!({"app_name": "Signal"});
        assert!(runner.should_skip_focus_window(&args, &mcp).is_none());
    }

    #[test]
    fn should_skip_focus_window_defers_when_kind_unknown() {
        // First-ever focus: no prior probe / structured response, so we
        // can't classify the app. The task is explicit about erring on
        // the side of executing focus_window normally in this case —
        // breaking Electron / Windows workflows is strictly worse than
        // a single preserved focus-steal on the first call.
        let backend = Backend;
        let runner = AgentRunner::new(&backend, AgentConfig::default());
        let mcp = ToolsetStub::with(FULL_AX_TOOLSET);
        let args = serde_json::json!({"app_name": "MysteryApp"});
        assert!(runner.should_skip_focus_window(&args, &mcp).is_none());
    }

    #[test]
    fn should_skip_focus_window_defers_when_ax_toolset_incomplete() {
        // Windows / older MCP servers surface only a partial toolset.
        // Without ax_click / ax_set_value / ax_select, the agent cannot
        // drive the target via AX and `focus_window` is still required.
        let backend = Backend;
        let runner = runner_with_kind(&backend, "Calculator", "Native");
        // Only take_ax_snapshot — no dispatch primitives.
        let mcp = ToolsetStub::with(&["take_ax_snapshot"]);
        let args = serde_json::json!({"app_name": "Calculator"});
        assert!(runner.should_skip_focus_window(&args, &mcp).is_none());
    }

    #[test]
    fn should_skip_focus_window_requires_app_name_in_args() {
        // window_id / pid-only focus_window variants are ambiguous; we
        // can't map them to a recorded kind, so the guard must not
        // fire. resolve_cdp_target's list_apps / list_windows path
        // still runs the real tool, which is the correct behavior.
        let backend = Backend;
        let runner = runner_with_kind(&backend, "Calculator", "Native");
        let mcp = ToolsetStub::with(FULL_AX_TOOLSET);
        let args = serde_json::json!({"window_id": 42});
        assert!(runner.should_skip_focus_window(&args, &mcp).is_none());
    }

    #[test]
    fn is_synthetic_focus_skip_matches_only_the_sentinels() {
        // Post-step bookkeeping gates CDP auto-connect and workflow-node
        // creation on this predicate — it must be tight enough that a
        // real focus_window success never masquerades as a skip, yet
        // match every FocusSkipReason variant so none of the runner's
        // suppressions leak into the workflow graph.
        for reason in FocusSkipReason::ALL {
            assert!(
                AgentRunner::<Backend>::is_synthetic_focus_skip(
                    "focus_window",
                    reason.llm_message()
                ),
                "focus_window + {:?} message must register as synthetic skip",
                reason,
            );
            assert!(
                !AgentRunner::<Backend>::is_synthetic_focus_skip(
                    "launch_app",
                    reason.llm_message()
                ),
                "non-focus_window tool with {:?} message must not register",
                reason,
            );
        }
        // Different result text — a real MCP success must not be
        // treated as skipped.
        assert!(!AgentRunner::<Backend>::is_synthetic_focus_skip(
            "focus_window",
            "Window focused successfully",
        ));
    }

    #[test]
    fn should_skip_focus_window_respects_allow_focus_window_policy() {
        // Policy takes precedence over every kind / toolset branch: when
        // `allow_focus_window == false`, the predicate must return the
        // policy sentinel even for cases that would otherwise defer
        // (unknown kind, missing toolset, missing app_name, CDP-not-live).
        // The returned skip text is the LLM-facing nudge toward AX / CDP
        // dispatch primitives.
        let backend = Backend;
        let mut runner = AgentRunner::new(
            &backend,
            AgentConfig {
                allow_focus_window: false,
                ..Default::default()
            },
        );
        let mcp_empty = ToolsetStub::with(&[]);

        // 1. Unknown app kind, empty toolset — would normally defer.
        let args_named = serde_json::json!({"app_name": "MysteryApp"});
        assert_eq!(
            runner.should_skip_focus_window(&args_named, &mcp_empty),
            Some(FocusSkipReason::PolicyDisabled),
        );

        // 2. Missing app_name (window_id / pid-only form) — the kind /
        // toolset branches always defer here, but policy overrides.
        let args_windowed = serde_json::json!({"window_id": 42});
        assert_eq!(
            runner.should_skip_focus_window(&args_windowed, &mcp_empty),
            Some(FocusSkipReason::PolicyDisabled),
        );

        // 3. Electron kind hint but no live CDP session — normally
        // defers because the first focus_window often precedes
        // cdp_connect. Policy overrides.
        runner.record_app_kind("Signal", "ElectronApp");
        let args_electron = serde_json::json!({"app_name": "Signal"});
        assert_eq!(
            runner.should_skip_focus_window(&args_electron, &mcp_empty),
            Some(FocusSkipReason::PolicyDisabled),
        );

        // 4. Default config still behaves as before — sanity check the
        // feature is truly opt-in and the unknown-kind defer path is
        // preserved.
        let default_runner = AgentRunner::new(&backend, AgentConfig::default());
        assert!(
            default_runner
                .should_skip_focus_window(&args_named, &mcp_empty)
                .is_none(),
            "default policy (allow_focus_window=true) must preserve the \
             existing defer-for-unknown-kind behavior",
        );
    }

    #[test]
    fn record_app_kind_overwrites_previous_value_for_same_app() {
        // Apps can transition between kinds across runs (e.g. a Chrome
        // profile that used to be launched plain and is now launched
        // with --remote-debugging-port). The latest hint must win so
        // the guard reflects the current lifecycle, not history.
        let backend = Backend;
        let mut runner = AgentRunner::new(&backend, AgentConfig::default());
        runner.record_app_kind("Calculator", "Native");
        runner.record_app_kind("Calculator", "ElectronApp");
        let mcp = ToolsetStub::with(FULL_AX_TOOLSET);
        let args = serde_json::json!({"app_name": "Calculator"});
        // Electron now — guard must NOT fire.
        assert!(runner.should_skip_focus_window(&args, &mcp).is_none());
    }
}
