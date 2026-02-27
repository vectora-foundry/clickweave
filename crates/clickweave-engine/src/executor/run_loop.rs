use super::{ExecutorCommand, ExecutorEvent, ExecutorState, WorkflowExecutor, check_eval};
use clickweave_core::{ExecutionMode, NodeRole, NodeRun, NodeType, RunStatus};
use clickweave_llm::ChatBackend;
use clickweave_mcp::McpClient;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
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

    /// Capture a screenshot of the focused app (or full screen) and save as artifact.
    /// Saves directly via `storage.save_artifact`, bypassing trace-level gating —
    /// check screenshots are evaluation evidence, not trace data.
    async fn capture_check_screenshot(&self, mcp: &McpClient, node_run: &mut NodeRun) {
        use base64::Engine;
        use clickweave_core::ArtifactKind;
        use clickweave_mcp::ToolContent;

        let app_name = self.focused_app.read().ok().and_then(|g| g.clone());

        let mut args = serde_json::json!({ "format": "png" });
        if let Some(ref name) = app_name {
            args["app_name"] = serde_json::Value::String(name.clone());
        }

        self.log(format!(
            "Capturing check screenshot{}",
            app_name
                .as_deref()
                .map_or(String::new(), |n| format!(" (app: {})", n))
        ));

        match mcp.call_tool("take_screenshot", Some(args)).await {
            Ok(result) => {
                for (idx, content) in result.content.iter().enumerate() {
                    if let ToolContent::Image { data, mime_type } = content {
                        let ext = if mime_type.contains("png") {
                            "png"
                        } else {
                            "jpg"
                        };
                        let filename = format!("check_screenshot_{}.{}", idx, ext);
                        if let Ok(decoded) = base64::engine::general_purpose::STANDARD.decode(data)
                        {
                            match self.storage.save_artifact(
                                node_run,
                                ArtifactKind::Screenshot,
                                &filename,
                                &decoded,
                                serde_json::Value::Null,
                            ) {
                                Ok(artifact) => node_run.artifacts.push(artifact),
                                Err(e) => {
                                    tracing::warn!("Failed to save check screenshot: {}", e)
                                }
                            }
                        }
                    }
                }
            }
            Err(e) => {
                self.log(format!(
                    "Warning: failed to capture check screenshot: {}",
                    e
                ));
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
            let checks = node.checks.clone();
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

                        // Capture post-node screenshot and track checked nodes
                        let has_checks = !checks.is_empty() || expected_outcome.is_some();
                        if has_checks {
                            if let Some(ref mut run) = node_run {
                                self.capture_check_screenshot(&mcp, run).await;
                            }
                            self.completed_checks.push((
                                node_id,
                                checks.clone(),
                                expected_outcome.clone(),
                            ));
                        }

                        // Inline verdict for Verification-role nodes
                        if node_role == NodeRole::Verification
                            && node_type.is_read_only()
                            && !matches!(node_type, NodeType::TakeScreenshot(_))
                        {
                            let v = super::verdict::deterministic_verdict(
                                node_id,
                                &node_name,
                                &node_type,
                                &node_result,
                            );
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
                                self.emit_error(format!("Verification failed: '{}'", node_name,));
                                verification_failed = true;
                                break (false, false);
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

        // --- Check evaluation pass (runs for all completed checked nodes) ---
        if !self.completed_checks.is_empty() {
            self.log("Running post-workflow check evaluation...");

            // Deduplicate: keep only the last entry per node_id (loop iterations
            // overwrite trace/screenshot on disk, so only the last is meaningful).
            let mut seen = HashSet::new();
            let deduped: Vec<_> = self
                .completed_checks
                .iter()
                .rev()
                .filter(|(id, _, _)| seen.insert(*id))
                .cloned()
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect();

            let mut node_names = HashMap::new();
            let mut trace_summaries = HashMap::new();
            let mut screenshots = HashMap::new();
            for (id, _, _) in &deduped {
                let name = self
                    .workflow
                    .find_node(*id)
                    .map(|n| n.name.clone())
                    .unwrap_or_default();
                if let Some(trace) = self.read_trace_summary(&name) {
                    trace_summaries.insert(*id, trace);
                }
                if let Some(img) = self.read_check_screenshot(&name) {
                    screenshots.insert(*id, img);
                }
                node_names.insert(*id, name);
            }

            let backend = self.vlm.as_ref().unwrap_or(&self.agent);
            let verdicts = check_eval::run_check_pass(
                backend,
                &deduped,
                &node_names,
                &trace_summaries,
                &screenshots,
                |msg| self.log(msg),
            )
            .await;

            if !verdicts.is_empty() {
                for v in &verdicts {
                    if let Err(e) = self.storage.save_node_verdict(v) {
                        tracing::warn!("Failed to persist verdict for '{}': {}", v.node_name, e);
                    }
                }
                self.emit(ExecutorEvent::ChecksCompleted(verdicts));
            }
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

    /// Read trace events for a node and produce a text summary for the LLM.
    fn read_trace_summary(&self, node_name: &str) -> Option<String> {
        let sanitized = clickweave_core::storage::sanitize_name(node_name);
        let exec_dir = self.storage.execution_dir_name()?;
        let events_path = self
            .storage
            .base_path()
            .join(exec_dir)
            .join(&sanitized)
            .join("events.jsonl");

        let content = std::fs::read_to_string(&events_path).ok()?;
        // Take the last 20 tool events so loop re-executions use the
        // latest iteration's evidence, not the earliest.
        let all_tool_lines: Vec<&str> = content
            .lines()
            .filter(|line| {
                serde_json::from_str::<Value>(line)
                    .ok()
                    .and_then(|v| v.get("event_type")?.as_str().map(String::from))
                    .is_some_and(|et| et == "tool_call" || et == "tool_result")
            })
            .collect();
        let summary: Vec<&str> = all_tool_lines
            .into_iter()
            .rev()
            .take(20)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();

        if summary.is_empty() {
            None
        } else {
            Some(summary.join("\n"))
        }
    }

    /// Read the check screenshot for a node and return as base64.
    fn read_check_screenshot(&self, node_name: &str) -> Option<String> {
        use base64::Engine;
        let sanitized = clickweave_core::storage::sanitize_name(node_name);
        let exec_dir = self.storage.execution_dir_name()?;
        let screenshot_path = self
            .storage
            .base_path()
            .join(exec_dir)
            .join(&sanitized)
            .join("artifacts")
            .join("check_screenshot_0.png");

        let data = std::fs::read(&screenshot_path).ok()?;
        Some(base64::engine::general_purpose::STANDARD.encode(&data))
    }
}
