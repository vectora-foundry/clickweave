use super::{ExecutorCommand, ExecutorEvent, ExecutorState, WorkflowExecutor};
use clickweave_core::{ExecutionMode, NodeRole, NodeRun, NodeType, RunStatus};
use clickweave_llm::ChatBackend;
use clickweave_mcp::McpClient;
use serde_json::Value;
use std::time::Duration;
use tokio::sync::mpsc::Receiver;
use uuid::Uuid;

enum SupervisionAction {
    Retry,
    Skip,
    Abort,
}

async fn wait_for_supervision_command(
    command_rx: &mut Receiver<ExecutorCommand>,
) -> SupervisionAction {
    match command_rx.recv().await {
        Some(ExecutorCommand::Resume) => SupervisionAction::Retry,
        Some(ExecutorCommand::Skip) => SupervisionAction::Skip,
        Some(ExecutorCommand::Abort) | Some(ExecutorCommand::Stop) | None => {
            SupervisionAction::Abort
        }
    }
}

impl<C: ChatBackend> WorkflowExecutor<C> {
    /// Execute a regular node with retry logic. Returns the result value on success.
    #[allow(clippy::too_many_arguments)]
    async fn execute_node_with_retries(
        &self,
        node_id: Uuid,
        node_name: &str,
        node_type: &NodeType,
        tools: &[Value],
        mcp: &McpClient,
        timeout_ms: Option<u64>,
        retries: u32,
        command_rx: &mut Receiver<ExecutorCommand>,
        node_run: &mut Option<NodeRun>,
    ) -> Result<Value, String> {
        let mut attempt = 0;

        loop {
            let result = match node_type {
                NodeType::AiStep(params) => {
                    self.execute_ai_step(
                        params,
                        tools,
                        mcp,
                        timeout_ms,
                        command_rx,
                        node_run.as_mut(),
                    )
                    .await
                }
                other => {
                    self.execute_deterministic(node_id, other, mcp, node_run.as_mut())
                        .await
                }
            };

            match result {
                Ok(value) => return Ok(value),
                Err(e) if attempt < retries => {
                    attempt += 1;
                    self.log(format!(
                        "Node {} failed (attempt {}/{}): {}. Retrying...",
                        node_name,
                        attempt,
                        retries + 1,
                        e
                    ));
                    self.evict_caches_for_node(node_type);
                    self.record_event(
                        node_run.as_ref(),
                        "retry",
                        serde_json::json!({
                            "attempt": attempt,
                            "error": e,
                        }),
                    );
                }
                Err(e) => return Err(e),
            }
        }
    }

    pub async fn run(&mut self, mut command_rx: Receiver<ExecutorCommand>) {
        self.emit(ExecutorEvent::StateChanged(ExecutorState::Running));
        self.log("Starting workflow execution");

        self.log_model_info("Agent", &self.agent).await;
        if let Some(vlm) = &self.vlm {
            self.log(format!("VLM enabled: {}", vlm.model_name()));
            self.log_model_info("VLM", vlm).await;
        } else {
            self.log("VLM not configured — images sent directly to agent");
        }

        let mcp = if self.mcp_command == "npx" {
            McpClient::spawn_npx().await
        } else {
            McpClient::spawn(&self.mcp_command, &[]).await
        };

        let mcp = match mcp {
            Ok(m) => m,
            Err(e) => {
                self.emit_error(format!("Failed to spawn MCP server: {}", e));
                self.emit(ExecutorEvent::StateChanged(ExecutorState::Idle));
                return;
            }
        };

        self.log(format!("MCP server ready with {} tools", mcp.tools().len()));

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

        let tools = mcp.tools_as_openai();

        let mut completed_normally = true;
        let mut verification_failed = false;

        while let Some(node_id) = current {
            if self.stop_requested(&mut command_rx) {
                self.log("Workflow stopped by user");
                completed_normally = false;
                break;
            }

            let Some(node) = self.workflow.find_node(node_id) else {
                self.log(format!("Node {} not found, stopping", node_id));
                break;
            };

            if !node.enabled {
                self.log(format!("Skipping disabled node: {}", node.name));
                current = self.follow_disabled_edge(node_id, &node.node_type);
                continue;
            }

            let node_name = node.name.clone();
            let node_type = node.node_type.clone();
            let node_role = node.role;
            let expected_outcome = node.expected_outcome.clone();

            // Control flow nodes: evaluate condition and follow edge
            if matches!(
                node_type,
                NodeType::If(_) | NodeType::Switch(_) | NodeType::Loop(_) | NodeType::EndLoop(_)
            ) {
                current = self.eval_control_flow(node_id, &node_name, &node_type);

                // Deferred loop-exit verification (Test mode only).
                // Read-only nodes inside loops skip supervision during
                // iterations; we verify the outcome once the loop exits.
                // Note: for nested loops this is safe — `.take()` consumes
                // the inner loop's pending exit before `eval_control_flow`
                // on the outer loop can set its own.
                if self.execution_mode == ExecutionMode::Test
                    && let Some(loop_exit) = self.pending_loop_exit.take()
                {
                    let verification = self.verify_loop_exit(&loop_exit, &mcp).await;
                    if verification.passed {
                        self.emit(ExecutorEvent::SupervisionPassed {
                            node_id: loop_exit.node_id,
                            node_name: loop_exit.loop_name.clone(),
                            summary: verification.reasoning,
                        });
                    } else {
                        self.emit(ExecutorEvent::SupervisionPaused {
                            node_id: loop_exit.node_id,
                            node_name: loop_exit.loop_name.clone(),
                            finding: verification.reasoning,
                            screenshot: verification.screenshot,
                        });

                        match wait_for_supervision_command(&mut command_rx).await {
                            SupervisionAction::Retry => {
                                // Can't re-run a loop; treat as skip
                                self.log("Supervision: user chose Retry (continuing past loop)");
                            }
                            SupervisionAction::Skip => {
                                self.log("Supervision: user chose Skip for loop exit");
                            }
                            SupervisionAction::Abort => {
                                self.log("Supervision: user chose Abort after loop exit");
                                completed_normally = false;
                                break;
                            }
                        }
                    }
                }

                continue;
            }

            // Regular execution nodes
            self.emit(ExecutorEvent::NodeStarted(node_id));
            self.log(format!(
                "Executing node: {} ({})",
                node_name,
                node_type.display_name()
            ));

            let timeout_ms = node.timeout_ms;
            let settle_ms = node.settle_ms;
            let retries = node.retries;
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

            // Inner loop: re-executes on supervision Retry.
            // Returns (succeeded, was_abort) to distinguish supervision
            // aborts from execution errors (which handle their own
            // finalization inside the Err arm).
            let (node_succeeded, was_supervision_abort) = loop {
                match self
                    .execute_node_with_retries(
                        node_id,
                        &node_name,
                        &node_type,
                        &tools,
                        &mcp,
                        timeout_ms,
                        retries,
                        &mut command_rx,
                        &mut node_run,
                    )
                    .await
                {
                    Ok(node_result) => {
                        self.extract_and_store_variables(
                            &node_name,
                            &node_result,
                            &node_type,
                            node_run.as_ref(),
                        );

                        // Inline verdict for Verification-role nodes
                        if node_role == NodeRole::Verification {
                            if let Some(v) = self
                                .evaluate_verification(
                                    node_id,
                                    &node_name,
                                    &node_type,
                                    expected_outcome.as_deref(),
                                    &node_result,
                                    &mcp,
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
                                self.runtime_verdicts.push(v);
                                if failed {
                                    self.emit_error(format!(
                                        "Verification failed: '{}'",
                                        node_name,
                                    ));
                                    verification_failed = true;
                                    break (false, false);
                                }
                            }
                        }

                        // Supervision (Test mode only)
                        if self.execution_mode == ExecutionMode::Test {
                            // Skip per-step supervision for nodes inside loops —
                            // individual steps (clicks, keypresses, condition
                            // checks) are verified in aggregate by
                            // verify_loop_exit when the loop completes.
                            let inside_loop = !self.context.loop_counters.is_empty();
                            if inside_loop {
                                self.log(format!(
                                    "Skipping supervision for '{}' (inside loop)",
                                    node_name
                                ));
                                break (true, false);
                            }

                            let verification = self.verify_step(&node_name, &node_type, &mcp).await;
                            if verification.passed {
                                self.emit(ExecutorEvent::SupervisionPassed {
                                    node_id,
                                    node_name: node_name.clone(),
                                    summary: verification.reasoning,
                                });
                                break (true, false);
                            } else {
                                self.emit(ExecutorEvent::SupervisionPaused {
                                    node_id,
                                    node_name: node_name.clone(),
                                    finding: verification.reasoning,
                                    screenshot: verification.screenshot,
                                });

                                match wait_for_supervision_command(&mut command_rx).await {
                                    SupervisionAction::Retry => {
                                        self.log("Supervision: user chose Retry");
                                        continue;
                                    }
                                    SupervisionAction::Skip => {
                                        self.log("Supervision: user chose Skip");
                                        break (true, false);
                                    }
                                    SupervisionAction::Abort => {
                                        self.log("Supervision: user chose Abort");
                                        break (false, true);
                                    }
                                }
                            }
                        } else {
                            break (true, false);
                        }
                    }
                    Err(e) => {
                        self.emit_error(format!("Node {} failed: {}", node_name, e));
                        if let Some(ref mut run) = node_run {
                            self.finalize_run(run, RunStatus::Failed);
                        }
                        self.emit(ExecutorEvent::NodeFailed(node_id, e));
                        break (false, false);
                    }
                }
            };

            if !node_succeeded {
                if verification_failed {
                    if let Some(ref mut run) = node_run {
                        self.finalize_run(run, RunStatus::Failed);
                    }
                    self.emit(ExecutorEvent::NodeFailed(
                        node_id,
                        "Verification failed".to_string(),
                    ));
                } else if was_supervision_abort {
                    // Only finalize + emit for supervision aborts; execution
                    // errors already handle this in the Err arm above.
                    if let Some(ref mut run) = node_run {
                        self.finalize_run(run, RunStatus::Failed);
                    }
                    self.emit(ExecutorEvent::NodeFailed(
                        node_id,
                        "Aborted by user during supervision".to_string(),
                    ));
                }
                completed_normally = false;
                break;
            }

            if let Some(ms) = settle_ms.filter(|&ms| ms > 0) {
                self.log(format!("Settling for {}ms", ms));
                tokio::time::sleep(Duration::from_millis(ms)).await;
            }

            if let Some(ref mut run) = node_run {
                self.finalize_run(run, RunStatus::Ok);
            }
            self.emit(ExecutorEvent::NodeCompleted(node_id));

            current = self.follow_single_edge(node_id);
        }

        // Emit accumulated runtime verdicts
        if !self.runtime_verdicts.is_empty() {
            for v in &self.runtime_verdicts {
                if let Err(e) = self.storage.save_node_verdict(v) {
                    tracing::warn!("Failed to persist verdict for '{}': {}", v.node_name, e);
                }
            }
            let verdicts: Vec<_> = self.runtime_verdicts.drain(..).collect();
            self.emit(ExecutorEvent::ChecksCompleted(verdicts));
        }

        // Save decision cache after Test mode runs
        if self.execution_mode == ExecutionMode::Test {
            let save_result = self
                .decision_cache
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .save(&self.storage.cache_path());
            match save_result {
                Ok(()) => self.log("Decision cache saved"),
                Err(e) => self.log(format!("Warning: failed to save decision cache: {}", e)),
            }
        }

        if completed_normally || verification_failed {
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
        mcp: &McpClient,
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
            if let Some(ref name) = self.focused_app.read().ok().and_then(|g| g.clone()) {
                args["app_name"] = serde_json::Value::String(name.clone());
            }
            let screenshot_b64 = self.extract_screenshot_image(mcp, args).await;
            match screenshot_b64 {
                Some(img) => {
                    let backend = self.vlm.as_ref().unwrap_or(&self.agent);
                    Some(
                        super::verdict::screenshot_verdict(
                            backend, node_id, node_name, outcome, &img,
                        )
                        .await,
                    )
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
