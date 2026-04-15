use std::collections::HashMap;
use std::time::Duration;

use anyhow::{Context, Result};
use clickweave_core::cdp::{CdpFindElementMatch, CdpFindElementsResponse};
use clickweave_core::tool_mapping::tool_invocation_to_node_type;
use clickweave_core::{Edge, Node, Position, Workflow};
use clickweave_llm::{ChatBackend, Message};
use serde_json::Value;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use super::completion_check::{VlmVerdict, build_completion_prompt, parse_yes_no};
use super::context;
use super::permissions::{PermissionAction, PermissionPolicy, ToolAnnotations, evaluate};
use super::prompt;
use super::recovery::{self, RecoveryAction};
use super::transition;
use super::types::*;
use crate::executor::Mcp;

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

/// Extract a text representation from an MCP tool call result.
///
/// Text content is included verbatim. Non-text content (images, unknown types)
/// is represented as a placeholder so the caller knows something was returned.
fn extract_result_text(result: &clickweave_mcp::ToolCallResult) -> String {
    result
        .content
        .iter()
        .map(|c| match c {
            clickweave_mcp::ToolContent::Text { text } => text.clone(),
            clickweave_mcp::ToolContent::Image { mime_type, .. } => {
                format!("[image: {}]", mime_type)
            }
            clickweave_mcp::ToolContent::Unknown => "[unknown content]".to_string(),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Result of requesting user approval for a tool action.
enum ApprovalResult {
    Approved,
    Rejected,
    Unavailable,
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
    /// screenshot. Disabled = legacy behaviour (agent's self-report wins).
    vision: Option<&'a B>,
    /// Permission policy consulted before every non-observation tool call.
    /// The default policy has no rules, allow_all = false, and
    /// require_confirm_destructive = false, which reproduces the previous
    /// behaviour (every approval-gated tool goes through the prompt).
    permissions: PermissionPolicy,
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
        }
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

    /// Send an event through the channel (backpressured).
    ///
    /// Uses `send().await` instead of `try_send()` so the agent loop
    /// slows down when the consumer falls behind, rather than dropping
    /// events that the UI depends on for workflow state.
    async fn emit_event(&self, event: AgentEvent) {
        if let Some(tx) = &self.event_tx
            && let Err(e) = tx.send(event).await
        {
            warn!("Failed to emit agent event: {}", e);
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
    /// tool call. Returns true when the cap is configured and has been
    /// reached, meaning the caller should halt the run.
    ///
    /// `destructive_hint == Some(true)` increments the streak; anything
    /// else resets it. A cap value of `0` disables the feature entirely,
    /// so the method is a no-op in that case.
    fn record_destructive_and_check_cap(
        &mut self,
        tool_name: &str,
        annotations_by_tool: &HashMap<String, ToolAnnotations>,
    ) -> bool {
        if self.config.consecutive_destructive_cap == 0 {
            return false;
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
        self.state.recent_destructive_tools.len() >= self.config.consecutive_destructive_cap
    }

    /// Halt the run because the consecutive-destructive cap was reached.
    /// Emits the cap-hit event and sets the terminal reason. Called once
    /// when `record_destructive_and_check_cap` reports the cap has been
    /// hit. Clears `recent_destructive_tools` afterwards so state serialization
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
    pub(crate) async fn run(
        &mut self,
        goal: String,
        workflow: Workflow,
        mcp: &(impl Mcp + ?Sized),
        variant_context: Option<&str>,
        mcp_tools: Vec<Value>,
    ) -> Result<AgentState> {
        self.state = AgentState::new(workflow);

        // Build conversation: system instructions + goal as a user message.
        // The goal is kept out of the system prompt so user-controlled text
        // does not occupy the highest-priority instruction layer.
        let mut system_text = prompt::system_prompt();
        if let Some(ctx) = variant_context {
            system_text.push_str(&format!("\n\nVariant context: {}", ctx));
        }
        self.messages = vec![
            Message::system(system_text),
            Message::user(prompt::goal_message(&goal)),
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
            if self.config.use_cache && !elements.is_empty() {
                let current_key = super::cache::cache_key(&goal, &elements);
                let is_repeat = last_cache_key.as_ref() == Some(&current_key);

                if !is_repeat && let Some(cached) = self.cache.lookup(&goal, &elements) {
                    // Skip observation tools that may exist in old cache files
                    // from before the write-side filter was added.
                    if Self::OBSERVATION_TOOLS.contains(&cached.tool_name.as_str()) {
                        debug!(
                            tool = %cached.tool_name,
                            "Skipping cached observation tool (stale entry)"
                        );
                    } else {
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
                        let needs_approval =
                            !Self::OBSERVATION_TOOLS.contains(&cached_tool.as_str());
                        if needs_approval {
                            let policy_action =
                                self.policy_for(&cached_tool, &cached_args, &annotations_by_tool);
                            if matches!(policy_action, PermissionAction::Deny) {
                                // Hard policy reject: fail the step, drop
                                // the cached entry so the next iteration
                                // does not re-hit it, and continue the loop
                                // — same as any other step error.
                                self.cache.remove(&goal, &elements);
                                last_cache_key = None;
                                let err_msg =
                                    format!("Tool `{}` denied by permission policy", cached_tool);
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
                                    elements: elements.clone(),
                                    command,
                                    outcome: StepOutcome::Error(err_msg.clone()),
                                    page_url: self.state.current_url.clone(),
                                };
                                self.state.steps.push(step);
                                self.state.consecutive_errors += 1;
                                previous_result = Some(format!("Error: {}", err_msg));
                                let action = recovery::recovery_strategy(
                                    self.state.consecutive_errors,
                                    self.config.max_consecutive_errors,
                                );
                                if matches!(action, RecoveryAction::Abort) {
                                    self.state.terminal_reason =
                                        Some(TerminalReason::MaxErrorsReached {
                                            consecutive_errors: self.state.consecutive_errors,
                                        });
                                    break;
                                }
                                continue;
                            }
                            if matches!(policy_action, PermissionAction::Ask) {
                                match self
                                    .request_approval(
                                        &cached_tool,
                                        &cached_args,
                                        step_index,
                                        " (cached)",
                                    )
                                    .await
                                {
                                    Some(ApprovalResult::Rejected) => {
                                        // Evict the rejected entry so the next
                                        // iteration falls through to the LLM
                                        // instead of re-prompting the same
                                        // cached action in an approval loop.
                                        self.cache.remove(&goal, &elements);
                                        last_cache_key = None;
                                        let command = AgentCommand::ToolCall {
                                            tool_name: cached_tool.clone(),
                                            arguments: cached_args.clone(),
                                            tool_call_id: format!("cache-{}", step_index),
                                        };
                                        let step = AgentStep {
                                            index: step_index,
                                            elements: elements.clone(),
                                            command,
                                            outcome: StepOutcome::Replan(
                                                "User rejected cached action".to_string(),
                                            ),
                                            page_url: self.state.current_url.clone(),
                                        };
                                        self.state.steps.push(step);
                                        previous_result =
                                            Some("Replan: user rejected cached action".to_string());
                                        continue;
                                    }
                                    Some(ApprovalResult::Unavailable) => {
                                        self.state.terminal_reason =
                                            Some(TerminalReason::ApprovalUnavailable);
                                        break;
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
                                if self.config.build_workflow {
                                    self.add_workflow_node(&cached_tool, &cached_args, &mcp_tools)
                                        .await;
                                }

                                // Reconstruct transcript so the LLM sees the full
                                // action history, not just the raw result text.
                                self.messages.push(Message::assistant_tool_calls(vec![
                                    clickweave_llm::ToolCall {
                                        id: tool_call_id.clone(),
                                        call_type: "function".to_string(),
                                        function: clickweave_llm::FunctionCall {
                                            name: cached_tool.clone(),
                                            arguments: serde_json::to_string(&cached_args)
                                                .unwrap_or_default(),
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

                                self.maybe_cdp_connect(&cached_tool, &cached_args, mcp)
                                    .await;

                                let step = AgentStep {
                                    index: step_index,
                                    elements: elements.clone(),
                                    command,
                                    outcome: StepOutcome::Success(result_text.clone()),
                                    page_url: self.state.current_url.clone(),
                                };
                                self.state.steps.push(step);
                                self.state.consecutive_errors = 0;
                                previous_result = Some(result_text);
                                last_cache_key = Some(current_key);

                                // Destructive-cap accounting: the cached
                                // replay counts toward the streak just like
                                // a live tool call.
                                if self.record_destructive_and_check_cap(
                                    &cached_tool,
                                    &annotations_by_tool,
                                ) {
                                    self.emit_destructive_cap_hit().await;
                                    break;
                                }
                                continue;
                            }
                            Ok(result) => {
                                let err_text = extract_result_text(&result);
                                debug!(error = %err_text, "Cached decision returned error, falling through to LLM");
                            }
                            Err(e) => {
                                debug!(error = %e, "Cached decision execution failed, falling through to LLM");
                            }
                        }
                    } // else: not an observation tool
                }
                // Reset cache key tracking when falling through to LLM
                last_cache_key = None;
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
            // the app was already running (focus_window).
            if let AgentCommand::ToolCall {
                tool_name,
                arguments,
                ..
            } = &command
                && matches!(&outcome, StepOutcome::Success(_))
            {
                self.maybe_cdp_connect(tool_name, arguments, mcp).await;
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
            match &outcome {
                StepOutcome::Success(result_text) => {
                    self.state.consecutive_errors = 0;
                    previous_result = Some(result_text.clone());

                    self.emit_event(AgentEvent::StepCompleted {
                        step_index,
                        tool_name: command.tool_name_or_unknown().to_string(),
                        summary: truncate_summary(result_text, 120),
                    })
                    .await;

                    let mut cap_hit_tool: Option<String> = None;
                    if let AgentCommand::ToolCall {
                        tool_name,
                        arguments,
                        ..
                    } = &command
                    {
                        // Only cache action tools, never observation tools.
                        // Observation tools provide fresh environmental evidence
                        // that must not be replayed from stale cache entries.
                        if self.config.use_cache
                            && !Self::OBSERVATION_TOOLS.contains(&tool_name.as_str())
                            && !elements.is_empty()
                        {
                            self.cache.store(
                                &goal,
                                &elements,
                                tool_name.clone(),
                                arguments.clone(),
                            );
                        }
                        if self.config.build_workflow {
                            self.add_workflow_node(tool_name, arguments, &mcp_tools)
                                .await;
                        }
                        if self.record_destructive_and_check_cap(tool_name, &annotations_by_tool) {
                            cap_hit_tool = Some(tool_name.clone());
                        }
                    }
                    if cap_hit_tool.is_some() {
                        self.emit_destructive_cap_hit().await;
                        break;
                    }
                }
                StepOutcome::Error(err) => {
                    self.state.consecutive_errors += 1;
                    previous_result = Some(format!("Error: {}", err));

                    self.emit_event(AgentEvent::StepFailed {
                        step_index,
                        tool_name: command.tool_name_or_unknown().to_string(),
                        error: err.clone(),
                    })
                    .await;

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
                            break;
                        }
                        RecoveryAction::Continue => {
                            debug!(
                                errors = self.state.consecutive_errors,
                                "Recovery: re-observing and continuing"
                            );
                        }
                    }
                }
                StepOutcome::Done(summary) => {
                    // Post-agent_done VLM check. A NO verdict halts the run
                    // and surfaces a disagreement event for the user to
                    // adjudicate; a YES verdict (or any verification error)
                    // falls through to normal completion.
                    let disagreement = self.verify_completion(&goal, summary, mcp).await;
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
                        break;
                    }

                    self.state.completed = true;
                    self.state.summary = Some(summary.clone());
                    self.state.terminal_reason = Some(TerminalReason::Completed {
                        summary: summary.clone(),
                    });
                    previous_result = None;
                    info!(summary = %summary, "Agent completed goal");
                    self.emit_event(AgentEvent::GoalComplete {
                        summary: summary.clone(),
                    })
                    .await;
                }
                StepOutcome::Replan(reason) => {
                    previous_result = Some(format!("Replan requested: {}", reason));
                    warn!(reason = %reason, "Agent requested replan");
                }
            }

            // Add the assistant's response to the conversation. Preserve the
            // model's reasoning_content so subsequent turns can build on prior
            // thinking instead of re-deriving it from scratch each step.
            // Only the first tool call is included — we execute exactly one
            // tool per turn, so appending extras would create transcript
            // entries with no corresponding tool result.
            let reasoning = choice.message.reasoning_content.clone();
            if let Some(tool_calls) = &choice.message.tool_calls {
                if let Some(tc) = tool_calls.first() {
                    let mut assistant_msg = Message::assistant_tool_calls(vec![tc.clone()]);
                    assistant_msg.reasoning_content = reasoning;
                    self.messages.push(assistant_msg);
                    let result_text = previous_result.as_deref().unwrap_or("ok");
                    self.messages
                        .push(Message::tool_result(&tc.id, result_text));
                }
            } else if let Some(text) = choice.message.content_text() {
                let mut assistant_msg = Message::assistant(text);
                assistant_msg.reasoning_content = reasoning;
                self.messages.push(assistant_msg);
            }
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
                let text = extract_result_text(&result);
                if let Ok(parsed) = serde_json::from_str::<CdpFindElementsResponse>(&text) {
                    self.state.current_url = parsed.page_url;
                    return parsed.matches;
                }
                debug!("Failed to parse cdp_find_elements response");
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
    async fn auto_connect_cdp(&self, app_name: &str, mcp: &(impl Mcp + ?Sized)) -> Option<u16> {
        if !mcp.has_tool("probe_app") || !mcp.has_tool("cdp_connect") {
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
            Ok(r) => extract_result_text(&r),
            Err(e) => {
                debug!(app = app_name, error = %e, "probe_app failed, skipping CDP");
                return None;
            }
        };

        if !probe_text.contains("ElectronApp") && !probe_text.contains("ChromeBrowser") {
            debug!(app = app_name, "Not an Electron/Chrome app, skipping CDP");
            return None;
        }

        info!(
            app = app_name,
            "Detected Electron/Chrome app, connecting CDP"
        );

        // 2. Check if already running with --remote-debugging-port
        if let Some(port) = crate::executor::deterministic::cdp::existing_debug_port(app_name).await
        {
            info!(app = app_name, port, "Reusing existing debug port");
            if self.cdp_connect_with_retries(port, mcp).await {
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
        let quit_args = serde_json::json!({"app_name": app_name});
        let _ = mcp.call_tool("quit_app", Some(quit_args)).await;

        // Poll until the app has exited (10s graceful, then force-quit)
        let mut quit_confirmed = false;
        for _ in 0..20 {
            tokio::time::sleep(Duration::from_millis(500)).await;
            let list_args = serde_json::json!({"app_name": app_name, "user_apps_only": true});
            if let Ok(r) = mcp.call_tool("list_apps", Some(list_args)).await {
                let text = extract_result_text(&r);
                if text.trim() == "[]" {
                    quit_confirmed = true;
                    break;
                }
            }
        }
        if !quit_confirmed {
            warn!(app = app_name, "App did not quit within 10s, force-killing");
            let force_args = serde_json::json!({"app_name": app_name, "force": true});
            let _ = mcp.call_tool("quit_app", Some(force_args)).await;
            tokio::time::sleep(Duration::from_secs(2)).await;
        }

        // Relaunch with debug port
        self.emit_event(AgentEvent::SubAction {
            tool_name: "launch_app".to_string(),
            summary: format!("Auto: relaunching {} with debug port {}", app_name, port),
        })
        .await;
        let launch_args = serde_json::json!({
            "app_name": app_name,
            "args": [format!("--remote-debugging-port={}", port)],
        });
        match mcp.call_tool("launch_app", Some(launch_args)).await {
            Ok(r) if r.is_error != Some(true) => {}
            Ok(r) => {
                let err = extract_result_text(&r);
                warn!(app = app_name, error = %err, "Relaunch with debug port failed");
                let fallback = serde_json::json!({"app_name": app_name});
                let _ = mcp.call_tool("launch_app", Some(fallback)).await;
                return None;
            }
            Err(e) => {
                warn!(app = app_name, error = %e, "Relaunch with debug port failed");
                let fallback = serde_json::json!({"app_name": app_name});
                let _ = mcp.call_tool("launch_app", Some(fallback)).await;
                return None;
            }
        }

        // Wait for the app to finish starting
        tokio::time::sleep(Duration::from_secs(3)).await;

        self.emit_event(AgentEvent::SubAction {
            tool_name: "cdp_connect".to_string(),
            summary: format!("Auto: connecting CDP on port {}", port),
        })
        .await;
        if self.cdp_connect_with_retries(port, mcp).await {
            info!(app = app_name, port, "CDP connected");
            Some(port)
        } else {
            warn!(app = app_name, port, "CDP connection failed after retries");
            None
        }
    }

    /// Attempt `cdp_connect` with retries, returning true on success.
    async fn cdp_connect_with_retries(&self, port: u16, mcp: &(impl Mcp + ?Sized)) -> bool {
        let args = serde_json::json!({"port": port});
        for attempt in 0..10 {
            if attempt > 0 {
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
            match mcp.call_tool("cdp_connect", Some(args.clone())).await {
                Ok(r) if r.is_error != Some(true) => return true,
                Ok(r) => {
                    debug!(
                        attempt = attempt + 1,
                        error = %extract_result_text(&r),
                        "cdp_connect attempt failed"
                    );
                }
                Err(e) => {
                    debug!(attempt = attempt + 1, error = %e, "cdp_connect attempt failed");
                }
            }
        }
        false
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
    async fn verify_completion(
        &self,
        goal: &str,
        summary: &str,
        mcp: &(impl Mcp + ?Sized),
    ) -> Option<(String, String)> {
        let vision = self.vision?;

        // The agent may not have a known focused app (e.g. web-only runs on
        // CDP) — rely on the MCP tool's default capture path.
        let result = match mcp
            .call_tool(
                "take_screenshot",
                Some(serde_json::json!({"include_ocr": false})),
            )
            .await
        {
            Ok(r) if r.is_error != Some(true) => r,
            Ok(r) => {
                warn!(
                    error = %extract_result_text(&r),
                    "Completion verification: take_screenshot returned error, skipping VLM check",
                );
                return None;
            }
            Err(e) => {
                warn!(error = %e, "Completion verification: take_screenshot failed, skipping VLM check");
                return None;
            }
        };

        let Some(raw_b64) = result.content.iter().find_map(|c| match c {
            clickweave_mcp::ToolContent::Image { data, .. } => Some(data.clone()),
            _ => None,
        }) else {
            warn!(
                "Completion verification: no image in take_screenshot result, skipping VLM check"
            );
            return None;
        };

        let Some((prepared_b64, mime)) = clickweave_llm::prepare_base64_image_for_vlm(
            &raw_b64,
            clickweave_llm::DEFAULT_MAX_DIMENSION,
        ) else {
            warn!("Completion verification: failed to prepare screenshot for VLM, skipping check");
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

        match parse_yes_no(&reply) {
            VlmVerdict::Yes => {
                info!("Completion verification: VLM confirmed goal");
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
        &self,
        tool_name: &str,
        arguments: &Value,
        mcp: &(impl Mcp + ?Sized),
    ) {
        if tool_name != "launch_app" && tool_name != "focus_window" {
            return;
        }
        let Some(app_name) = arguments["app_name"].as_str() else {
            return;
        };
        if let Some(cdp_port) = self.auto_connect_cdp(app_name, mcp).await {
            self.emit_event(AgentEvent::CdpConnected {
                app_name: app_name.to_string(),
                port: cdp_port,
            })
            .await;
            // Refresh the client-side cache (not the agent's LLM tools
            // vec) so observation gates like `has_tool("cdp_find_elements")`
            // see tools surfaced by the server post-connect.
            if let Err(e) = mcp.refresh_tools().await {
                warn!(error = %e, "Post-CDP-connect client tool-cache refresh failed");
            }
        }
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
                let args: Value = match serde_json::from_str(&tc.function.arguments) {
                    Ok(v) => v,
                    Err(e) => {
                        warn!(
                            tool = %tc.function.name,
                            error = %e,
                            raw = %tc.function.arguments,
                            "Malformed tool-call arguments from LLM"
                        );
                        let command = AgentCommand::ToolCall {
                            tool_name: tc.function.name.clone(),
                            arguments: Value::Null,
                            tool_call_id: tc.id.clone(),
                        };
                        return Ok((
                            command,
                            StepOutcome::Error(format!("Malformed tool arguments: {}", e)),
                        ));
                    }
                };

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

                // Request user approval before executing the tool.
                // Observation-only tools bypass approval entirely; for
                // everything else, the permission policy decides whether
                // to prompt (Ask), skip the prompt (Allow), or hard-reject
                // (Deny).
                let needs_approval = !Self::OBSERVATION_TOOLS.contains(&tc.function.name.as_str());
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

    /// Tools that are observation-only — used by the agent to understand the
    /// screen but should NOT become workflow nodes.
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

    /// Add a workflow node for the executed tool call.
    /// Skips observation-only tools that the agent uses for perception.
    async fn add_workflow_node(
        &mut self,
        tool_name: &str,
        arguments: &Value,
        known_tools: &[Value],
    ) {
        if Self::OBSERVATION_TOOLS.contains(&tool_name) {
            return;
        }
        let node_type = match tool_invocation_to_node_type(tool_name, arguments, known_tools) {
            Ok(nt) => nt,
            Err(e) => {
                warn!(error = %e, tool = tool_name, "Could not map tool to workflow node type — workflow graph will be incomplete");
                self.emit_event(AgentEvent::Warning {
                    message: format!("Failed to map tool '{}' to workflow node: {}", tool_name, e),
                })
                .await;
                return;
            }
        };

        let position = Position {
            x: 0.0,
            y: (self.state.workflow.nodes.len() as f32) * 120.0,
        };
        let node = Node::new(node_type, position, tool_name, "");
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
    }
}
