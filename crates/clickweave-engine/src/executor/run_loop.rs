use super::Mcp;
use super::error::ExecutorError;
use super::retry_context::RetryContext;
use super::{ExecutorCommand, ExecutorEvent, ExecutorResult, ExecutorState, WorkflowExecutor};
use clickweave_core::{ExecutionMode, NodeRole, NodeRun, NodeType, RunStatus};
use clickweave_llm::ChatBackend;
use serde_json::Value;
use std::time::Duration;
use tokio::sync::mpsc::Receiver;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

enum SupervisionAction {
    Retry,
    Skip,
    Abort,
}

/// Outcome of executing a single node with its supervision retry loop.
enum StepOutcome {
    /// Node completed successfully (possibly after supervision retries).
    Succeeded,
    /// User or system aborted during supervision.
    Aborted,
    /// Node failed (error already emitted/finalized inside the step).
    Failed,
    /// Inline verification-role node produced a failing verdict.
    VerificationFailed,
    /// Cancellation token fired during node execution.
    Cancelled,
}

async fn wait_for_supervision_command(
    command_rx: &mut Receiver<ExecutorCommand>,
    cancel: &CancellationToken,
) -> SupervisionAction {
    tokio::select! {
        biased;
        _ = cancel.cancelled() => SupervisionAction::Abort,
        cmd = command_rx.recv() => match cmd {
            Some(ExecutorCommand::Resume) => SupervisionAction::Retry,
            Some(ExecutorCommand::Skip) => SupervisionAction::Skip,
            Some(ExecutorCommand::Abort) | None => SupervisionAction::Abort,
        },
    }
}

impl<C: ChatBackend> WorkflowExecutor<C> {
    /// For text-input nodes (TypeText, CdpFill, CdpType), re-execute the
    /// preceding click node to re-establish element focus before a supervision
    /// retry.  Without this, retries keep typing into the same wrong field
    /// when the original click targeted the wrong element.
    async fn re_execute_preceding_click(
        &mut self,
        node_id: Uuid,
        node_type: &NodeType,
        mcp: &(impl Mcp + ?Sized),
        retry_ctx: &mut RetryContext,
    ) {
        if !node_type.is_text_input() {
            return;
        }

        let Some(pred_id) = self.find_predecessor(node_id) else {
            return;
        };

        // Clone what we need to avoid holding an immutable borrow on
        // self.workflow across the mutable execute_deterministic call.
        let pred = self
            .workflow
            .find_node(pred_id)
            .filter(|n| n.enabled)
            .map(|n| (n.name.clone(), n.node_type.clone()));

        let Some((pred_name, pred_type)) = pred else {
            return;
        };

        if !pred_type.is_focus_establishing() {
            return;
        }

        self.log(format!(
            "Re-running preceding click '{}' to re-establish focus",
            pred_name
        ));
        self.evict_caches_for_node(&pred_type);

        // Evict click disambiguation entries for the predecessor so the
        // re-executed click doesn't replay the same (wrong) cached choice.
        let prefix = format!("{}\0", pred_id);
        self.write_decision_cache()
            .click_disambiguation
            .retain(|k, _| !k.starts_with(&prefix));
        match self
            .execute_deterministic(pred_id, &pred_type, mcp, None, retry_ctx)
            .await
        {
            Ok(_) => {
                self.log(format!("Preceding click '{}' succeeded", pred_name));
            }
            Err(e) => {
                self.log(format!(
                    "Warning: preceding click '{}' failed: {} (continuing with retry)",
                    pred_name, e
                ));
            }
        }
    }

    /// Execute a regular node with retry logic. Returns the result value on success.
    #[allow(clippy::too_many_arguments)]
    async fn execute_node_with_retries(
        &mut self,
        node_id: Uuid,
        node_name: &str,
        node_type: &NodeType,
        tools: &[Value],
        mcp: &(impl Mcp + ?Sized),
        timeout_ms: Option<u64>,
        retries: u32,
        node_run: &mut Option<NodeRun>,
        retry_ctx: &mut RetryContext,
    ) -> ExecutorResult<Value> {
        let mut attempt = 0;

        loop {
            if self.is_cancelled() {
                return Err(ExecutorError::Cancelled);
            }
            let result = match node_type {
                NodeType::AiStep(params) => {
                    self.execute_ai_step(
                        params,
                        tools,
                        mcp,
                        timeout_ms,
                        node_run.as_mut(),
                        retry_ctx,
                    )
                    .await
                }
                other => {
                    self.execute_deterministic(node_id, other, mcp, node_run.as_mut(), retry_ctx)
                        .await
                }
            };

            match result {
                Ok(value) => {
                    retry_ctx.force_resolve = false;
                    return Ok(value);
                }
                Err(e) if attempt < retries => {
                    attempt += 1;
                    self.log(format!(
                        "Node {} failed (attempt {}/{}): {}. Retrying...",
                        node_name,
                        attempt,
                        retries + 1,
                        e
                    ));
                    // Plain retries should not inherit supervision-driven
                    // candidate exclusions — clear disambiguation state for a
                    // fresh start on each plain retry.
                    retry_ctx.write_tried_click_indices().clear();
                    retry_ctx.write_tried_cdp_uids().clear();
                    retry_ctx.write_cdp_ambiguity_overrides().clear();
                    self.evict_caches_for_node(node_type);
                    retry_ctx.force_resolve = true;
                    self.record_event(
                        node_run.as_ref(),
                        "retry",
                        serde_json::json!({
                            "attempt": attempt,
                            "error": e.to_string(),
                        }),
                    );
                }
                Err(e) => return Err(e),
            }
        }
    }

    /// Handle the manual supervision pause flow: emit SupervisionPaused,
    /// wait for user command (Resume/Skip/Abort), and return the action.
    async fn handle_supervision_pause(
        &self,
        node_id: Uuid,
        node_name: &str,
        finding: String,
        screenshot: Option<String>,
        command_rx: &mut Receiver<ExecutorCommand>,
    ) -> SupervisionAction {
        self.emit(ExecutorEvent::SupervisionPaused {
            node_id,
            node_name: node_name.to_string(),
            finding,
            screenshot,
        });

        wait_for_supervision_command(command_rx, &self.cancel_token).await
    }

    /// Execute a single node with the full supervision retry loop.
    ///
    /// Wraps `execute_node_with_retries` with supervision verification,
    /// auto-retry, runtime resolution, and manual pause/resume handling.
    /// Returns a `StepOutcome` indicating what the main loop should do next.
    #[allow(clippy::too_many_arguments)]
    async fn execute_with_supervision(
        &mut self,
        node_id: Uuid,
        node_name: &str,
        node_auto_id: &str,
        node_type: &NodeType,
        node_role: NodeRole,
        expected_outcome: Option<&str>,
        tools: &[Value],
        mcp: &(impl Mcp + ?Sized),
        timeout_ms: Option<u64>,
        retries: u32,
        supervision_retries: u32,
        node_run: &mut Option<NodeRun>,
        ctx: &mut RetryContext,
        command_rx: &mut Receiver<ExecutorCommand>,
    ) -> StepOutcome {
        let mut supervision_attempts: u32 = 0;

        loop {
            match self
                .execute_node_with_retries(
                    node_id, node_name, node_type, tools, mcp, timeout_ms, retries, node_run, ctx,
                )
                .await
            {
                Ok(node_result) => {
                    if ctx.focus_dirty {
                        self.refresh_focused_pid(mcp).await;
                        ctx.focus_dirty = false;
                    }

                    self.extract_and_store_variables(
                        node_auto_id,
                        &node_result,
                        node_type,
                        node_run.as_ref(),
                    );

                    // Run action verification if configured
                    if let Some((method, assertion)) =
                        super::action_verification::extract_verification_config(node_type)
                        && let Err(e) = self
                            .run_action_verification(
                                node_auto_id,
                                &method,
                                &assertion,
                                mcp,
                                node_run.as_ref(),
                            )
                            .await
                    {
                        self.log(format!(
                            "Action verification failed for {}: {}",
                            node_auto_id, e
                        ));
                    }

                    // Inline verdict for Verification-role nodes
                    if node_role == NodeRole::Verification
                        && let Some(v) = self
                            .evaluate_verification(
                                node_id,
                                node_name,
                                node_type,
                                expected_outcome,
                                &node_result,
                                mcp,
                            )
                            .await
                    {
                        let failed = v
                            .check_results
                            .iter()
                            .any(|r| r.verdict == clickweave_core::CheckVerdict::Fail);
                        self.log(format!(
                            "Verification '{}': {}",
                            node_name,
                            if failed { "FAIL" } else { "PASS" },
                        ));
                        let has_warn = v
                            .check_results
                            .iter()
                            .any(|r| r.verdict == clickweave_core::CheckVerdict::Warn);
                        ctx.runtime_verdicts.push(v);
                        if has_warn {
                            self.log(format!(
                                "WARNING: Verification node '{}' has no expected_outcome — \
                                 verification was not evaluated. \
                                 Set expected_outcome to enable actual verification.",
                                node_name,
                            ));
                        }
                        if failed {
                            self.emit_error(format!("Verification failed: '{}'", node_name));
                            return StepOutcome::VerificationFailed;
                        }
                    }

                    // Supervision (Test mode only)
                    if self.execution_mode == ExecutionMode::Test {
                        let verification = self.verify_step(node_name, node_type, mcp, ctx).await;
                        if verification.passed {
                            ctx.supervision_hint = None;
                            ctx.force_resolve = false;
                            // Consume the URL navigation intent now that
                            // supervision confirmed the step succeeded.
                            ctx.last_typed_url = None;
                            self.emit(ExecutorEvent::SupervisionPassed {
                                node_id,
                                node_name: node_name.to_string(),
                                summary: verification.reasoning,
                            });
                            return StepOutcome::Succeeded;
                        } else if supervision_attempts < supervision_retries {
                            // Auto-retry: evict caches and re-execute with hint
                            supervision_attempts += 1;
                            self.log(format!(
                                "Supervision auto-retry {}/{} for '{}': {}",
                                supervision_attempts,
                                supervision_retries,
                                node_name,
                                verification.reasoning,
                            ));
                            ctx.supervision_hint = Some(verification.reasoning.clone());
                            // Drop the prior agent pick so the supervision
                            // hint can drive a different disambiguation on
                            // the retry. Without this, the resolver would
                            // short-circuit on the stale override.
                            ctx.write_cdp_ambiguity_overrides().clear();
                            self.evict_caches_for_node(node_type);
                            ctx.force_resolve = true;
                            self.record_event(
                                node_run.as_ref(),
                                "supervision_retry",
                                serde_json::json!({
                                    "attempt": supervision_attempts,
                                    "reason": verification.reasoning,
                                }),
                            );
                            self.re_execute_preceding_click(node_id, node_type, mcp, ctx)
                                .await;
                            continue;
                        } else {
                            // Exhausted auto-retries — fall through to the
                            // manual supervision dialog.
                            ctx.supervision_hint = None;

                            match self
                                .handle_supervision_pause(
                                    node_id,
                                    node_name,
                                    verification.reasoning,
                                    verification.screenshot,
                                    command_rx,
                                )
                                .await
                            {
                                SupervisionAction::Retry => {
                                    self.log("Supervision: user chose Retry");
                                    // Reset supervision state so the manual retry gets a
                                    // fresh start with the full auto-retry budget.
                                    supervision_attempts = 0;
                                    ctx.write_tried_click_indices().clear();
                                    ctx.write_tried_cdp_uids().clear();
                                    ctx.write_cdp_ambiguity_overrides().clear();
                                    ctx.supervision_hint = None;
                                    self.re_execute_preceding_click(node_id, node_type, mcp, ctx)
                                        .await;
                                    continue;
                                }
                                SupervisionAction::Skip => {
                                    self.log("Supervision: user chose Skip");
                                    return StepOutcome::Succeeded;
                                }
                                SupervisionAction::Abort => {
                                    self.log("Supervision: user chose Abort");
                                    return StepOutcome::Aborted;
                                }
                            }
                        }
                    } else {
                        return StepOutcome::Succeeded;
                    }
                }
                Err(ExecutorError::Cancelled) => {
                    self.log(format!("Node '{}' cancelled", node_name));
                    if let Some(run) = node_run {
                        self.finalize_run(run, RunStatus::Cancelled);
                    }
                    self.emit(ExecutorEvent::NodeCancelled(node_id));
                    return StepOutcome::Cancelled;
                }
                Err(ExecutorError::CdpAmbiguousTarget { target, candidates }) => {
                    // Agent-driven disambiguation: pick one candidate, stash
                    // the uid in the retry context, and re-run the node so the
                    // resolver short-circuits to the chosen uid.
                    let already_tried = ctx.read_cdp_ambiguity_overrides().contains_key(&target);
                    if already_tried {
                        let msg = format!(
                            "Disambiguation already attempted for '{}' but resolver remained ambiguous",
                            target
                        );
                        self.emit_error(format!("Node {} failed: {}", node_name, msg));
                        if let Some(run) = node_run {
                            self.finalize_run(run, RunStatus::Failed);
                        }
                        self.emit(ExecutorEvent::NodeFailed(node_id, msg));
                        return StepOutcome::Failed;
                    }

                    self.log(format!(
                        "Ambiguous CDP target '{}' — asking agent to pick among {} candidates",
                        target,
                        candidates.len()
                    ));
                    match self
                        .resolve_cdp_ambiguity(
                            node_name,
                            &target,
                            candidates,
                            mcp,
                            node_run.as_mut(),
                        )
                        .await
                    {
                        Ok(res) => {
                            self.log(format!(
                                "Agent picked uid='{}' for '{}': {}",
                                res.chosen_uid,
                                target,
                                Self::truncate_for_trace(&res.reasoning, 200)
                            ));
                            ctx.write_cdp_ambiguity_overrides()
                                .insert(target.clone(), res.chosen_uid.clone());
                            self.emit(ExecutorEvent::AmbiguityResolved {
                                node_id,
                                target,
                                candidates: res.candidates_with_rects,
                                chosen_uid: res.chosen_uid,
                                reasoning: res.reasoning,
                                viewport_width: res.viewport_width,
                                viewport_height: res.viewport_height,
                                screenshot_path: res.screenshot_path,
                                screenshot_base64: res.screenshot_base64,
                            });
                            continue;
                        }
                        Err(disambig_err) => {
                            let msg = format!("Disambiguation failed: {}", disambig_err);
                            self.emit_error(format!("Node {} failed: {}", node_name, msg));
                            if let Some(run) = node_run {
                                self.finalize_run(run, RunStatus::Failed);
                            }
                            self.emit(ExecutorEvent::NodeFailed(node_id, msg));
                            return StepOutcome::Failed;
                        }
                    }
                }
                Err(e) => {
                    let msg = e.to_string();
                    self.emit_error(format!("Node {} failed: {}", node_name, msg));
                    if let Some(run) = node_run {
                        self.finalize_run(run, RunStatus::Failed);
                    }
                    self.emit(ExecutorEvent::NodeFailed(node_id, msg));
                    return StepOutcome::Failed;
                }
            }
        }
    }

    /// Spawn an MCP client from the configured binary path and run the workflow.
    ///
    /// This is the main entry point used by the Tauri command layer. It spawns
    /// the MCP server process, then delegates to [`run_with_mcp`](Self::run_with_mcp).
    pub async fn run(&mut self, command_rx: Receiver<ExecutorCommand>) {
        self.emit(ExecutorEvent::StateChanged(ExecutorState::Running));
        self.log("Starting workflow execution");

        self.log_model_info("Agent", &self.agent).await;
        if let Some(fast) = &self.fast {
            self.log(format!("Fast model configured: {}", fast.model_name()));
            self.log_model_info("Fast", fast).await;
        } else if let Some(supervisor) = &self.supervision {
            self.log(format!(
                "Fast model not configured — using supervisor ({}) for vision",
                supervisor.model_name()
            ));
        } else {
            self.log("VLM not configured — images sent directly to agent");
        }

        let mcp_result = clickweave_mcp::McpClient::spawn(&self.mcp_binary_path, &[]).await;

        let mcp = match mcp_result {
            Ok(m) => m,
            Err(e) => {
                self.emit_error(format!("Failed to spawn MCP server: {}", e));
                self.emit(ExecutorEvent::StateChanged(ExecutorState::Idle));
                return;
            }
        };

        self.run_with_mcp(command_rx, &mcp).await;
    }

    /// Execute the workflow using an already-connected MCP client.
    ///
    /// Separated from [`run`](Self::run) so that callers (including tests) can
    /// inject a pre-constructed or stub MCP implementation.
    pub(crate) async fn run_with_mcp(
        &mut self,
        mut command_rx: Receiver<ExecutorCommand>,
        mcp: &(impl Mcp + ?Sized),
    ) {
        self.emit(ExecutorEvent::StateChanged(ExecutorState::Running));

        let tools = mcp.tools_as_openai();
        self.log(format!("MCP ready: {} tools", tools.len()));

        match self.storage.begin_execution() {
            Ok(exec_dir) => self.log(format!("Execution dir: {}", exec_dir)),
            Err(e) => {
                self.emit_error(format!("Failed to create execution directory: {}", e));
                self.emit(ExecutorEvent::StateChanged(ExecutorState::Idle));
                return;
            }
        }

        let entries = self.entry_points();
        if entries.is_empty() {
            self.emit_error("No entry point found in workflow".to_string());
            self.emit(ExecutorEvent::StateChanged(ExecutorState::Idle));
            return;
        }
        let mut current: Option<Uuid> = Some(entries[0]);

        self.log("Starting graph walk from entry point");

        let mut user_cancelled = false;
        let mut ctx = RetryContext::new();

        while let Some(node_id) = current {
            if self.is_cancelled() {
                self.log("Workflow cancelled by user");
                user_cancelled = true;
                break;
            }

            let Some(node) = self.workflow.find_node(node_id) else {
                self.log(format!("Node {} not found, stopping", node_id));
                break;
            };

            if !node.enabled {
                self.log(format!("Skipping disabled node: {}", node.name));
                current = self.follow_single_edge(node_id);
                continue;
            }

            if matches!(node.node_type, NodeType::Unknown) {
                let msg = format!(
                    "Unsupported node type '{}' ({}). This node was created with a newer version or uses removed features.",
                    node.name, node_id
                );
                tracing::warn!("{}", msg);
                self.log(msg.clone());
                self.emit(ExecutorEvent::NodeFailed(node_id, msg));
                user_cancelled = true; // Prevent false-positive "completed" status
                current = None;
                continue;
            }

            let node_name = node.name.clone();
            let node_auto_id = node.auto_id.clone();
            let node_type = node.node_type.clone();
            let node_role = node.role;
            let expected_outcome = node.expected_outcome.clone();

            // Regular execution nodes
            self.emit(ExecutorEvent::NodeStarted(node_id));
            ctx.supervision_hint = None;
            ctx.write_tried_click_indices().clear();
            ctx.write_tried_cdp_uids().clear();
            // Disambiguation overrides are per-node: a later node that happens
            // to share a target label (e.g. "Save") must re-resolve against
            // its own page rather than reuse an earlier node's chosen uid.
            ctx.write_cdp_ambiguity_overrides().clear();
            self.log(format!(
                "Executing node: {} ({})",
                node_name,
                node_type.display_name()
            ));

            let timeout_ms = node.timeout_ms;
            let settle_ms = node.settle_ms;
            let retries = node.retries;
            let supervision_retries = node.supervision_retries;
            let trace_level = node.trace_level;

            let mut node_run = self
                .storage
                .create_run(node_id, &node_name, trace_level)
                .ok();

            if let Some(ref run) = node_run {
                self.emit(ExecutorEvent::RunCreated(node_id, run.clone()));
            }
            self.record_event(
                node_run.as_ref(),
                "node_started",
                serde_json::json!({
                    "name": node_name,
                    "type": node_type.display_name(),
                }),
            );

            let outcome = self
                .execute_with_supervision(
                    node_id,
                    &node_name,
                    &node_auto_id,
                    &node_type,
                    node_role,
                    expected_outcome.as_deref(),
                    &tools,
                    mcp,
                    timeout_ms,
                    retries,
                    supervision_retries,
                    &mut node_run,
                    &mut ctx,
                    &mut command_rx,
                )
                .await;

            // URL-enter intent is only meaningful for the current PressKey node.
            // Clear it when we leave this node so it cannot leak into a later,
            // unrelated PressKey step after Skip/Retry paths.
            if matches!(node_type, NodeType::PressKey(_)) {
                ctx.last_typed_url = None;
            }

            match outcome {
                StepOutcome::Succeeded => {
                    if let Some(ms) = settle_ms.filter(|&ms| ms > 0) {
                        self.log(format!("Settling for {}ms", ms));
                        tokio::time::sleep(Duration::from_millis(ms)).await;
                    }

                    if let Some(ref mut run) = node_run {
                        self.finalize_run(run, RunStatus::Ok);
                    }
                    self.emit(ExecutorEvent::NodeCompleted(node_id));
                    ctx.completed_node_ids.push((
                        node_id,
                        clickweave_core::storage::sanitize_name(&node_auto_id),
                    ));

                    current = self.follow_single_edge(node_id);
                }
                StepOutcome::Aborted => {
                    if let Some(ref mut run) = node_run {
                        self.finalize_run(run, RunStatus::Failed);
                    }
                    self.emit(ExecutorEvent::NodeFailed(
                        node_id,
                        "Aborted by user during supervision".to_string(),
                    ));
                    break;
                }
                StepOutcome::Failed => {
                    // Error already emitted/finalized inside execute_with_supervision
                    break;
                }
                StepOutcome::Cancelled => {
                    self.log("Node cancelled");
                    user_cancelled = true;
                    break;
                }
                StepOutcome::VerificationFailed => {
                    if let Some(ref mut run) = node_run {
                        self.finalize_run(run, RunStatus::Failed);
                    }
                    self.emit(ExecutorEvent::NodeFailed(
                        node_id,
                        "Verification failed".to_string(),
                    ));
                    break;
                }
            }
        }

        // Emit accumulated runtime verdicts
        if !ctx.runtime_verdicts.is_empty() {
            for v in &ctx.runtime_verdicts {
                if let Err(e) = self.storage.save_node_verdict(v) {
                    tracing::warn!("Failed to persist verdict for '{}': {}", v.node_name, e);
                }
            }
            let verdicts: Vec<_> = ctx.runtime_verdicts.drain(..).collect();
            self.emit(ExecutorEvent::ChecksCompleted(verdicts));
        }

        // Save decision cache after Test mode runs
        if self.execution_mode == ExecutionMode::Test {
            let save_result = self.read_decision_cache().save(&self.storage.cache_path());
            match save_result {
                Ok(()) => self.log("Decision cache saved"),
                Err(e) => self.log(format!("Warning: failed to save decision cache: {}", e)),
            }
        }

        if !user_cancelled {
            self.log("Workflow execution completed");
            self.emit(ExecutorEvent::WorkflowCompleted);
        }
        self.emit(ExecutorEvent::StateChanged(ExecutorState::Idle));
    }

    /// Evaluate a Verification-role node and return its verdict.
    /// Returns `None` for non-read-only nodes or when screenshot capture fails.
    async fn evaluate_verification(
        &self,
        node_id: Uuid,
        node_name: &str,
        node_type: &NodeType,
        expected_outcome: Option<&str>,
        node_result: &Value,
        mcp: &(impl Mcp + ?Sized),
    ) -> Option<clickweave_core::NodeVerdict> {
        if matches!(node_type, NodeType::TakeScreenshot(_)) {
            let Some(outcome) = expected_outcome else {
                self.log(format!(
                    "Warning: TakeScreenshot node '{}' has Verification role but no expected_outcome",
                    node_name,
                ));
                return Some(super::verdict::missing_outcome_verdict(node_id, node_name));
            };
            let mut args = serde_json::json!({ "format": "png" });
            if let Some(ref name) = self.focused_app_name() {
                args["app_name"] = serde_json::Value::String(name.clone());
            }
            let screenshot_b64 = self.extract_screenshot_image(mcp, args).await;
            match screenshot_b64 {
                Some(img) => {
                    let verdict = if let Some(ref vb) = self.verdict_fast {
                        super::verdict::screenshot_verdict(vb, node_id, node_name, outcome, &img)
                            .await
                    } else {
                        let backend = self.vision_backend().unwrap_or(&self.agent);
                        super::verdict::screenshot_verdict(
                            backend, node_id, node_name, outcome, &img,
                        )
                        .await
                    };
                    Some(verdict)
                }
                None => {
                    self.log(format!(
                        "Verification '{}': screenshot capture failed — marking as FAIL",
                        node_name,
                    ));
                    Some(super::verdict::screenshot_capture_failed_verdict(
                        node_id, node_name,
                    ))
                }
            }
        } else if node_type.is_read_only() {
            Some(super::verdict::deterministic_verdict(
                node_id,
                node_name,
                node_type,
                node_result,
            ))
        } else {
            self.log(format!(
                "Warning: node '{}' has Verification role but is not a read-only type — skipping verdict",
                node_name,
            ));
            None
        }
    }
}
