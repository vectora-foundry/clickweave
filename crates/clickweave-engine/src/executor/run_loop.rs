use super::{
    ExecutorCommand, ExecutorEvent, ExecutorState, LoopExitReason, PendingLoopExit,
    WorkflowExecutor, check_eval,
};
use clickweave_core::runtime::RuntimeContext;
use clickweave_core::{
    EdgeOutput, ExecutionMode, NodeRun, NodeType, RunStatus, sanitize_node_name,
};
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
    /// Find entry points: nodes with no incoming edges.
    /// EndLoop back-edges (edges where the source is an EndLoop node)
    /// are NOT counted as incoming edges — this prevents loops from breaking
    /// entry point detection.
    pub(crate) fn entry_points(&self) -> Vec<Uuid> {
        let endloop_nodes: HashSet<Uuid> = self
            .workflow
            .nodes
            .iter()
            .filter(|n| matches!(n.node_type, NodeType::EndLoop(_)))
            .map(|n| n.id)
            .collect();

        let targets: HashSet<Uuid> = self
            .workflow
            .edges
            .iter()
            .filter(|e| !endloop_nodes.contains(&e.from))
            .map(|e| e.to)
            .collect();

        self.workflow
            .nodes
            .iter()
            .filter(|n| !targets.contains(&n.id))
            .map(|n| n.id)
            .collect()
    }

    /// Follow the single outgoing edge from a regular node (output is None).
    pub(crate) fn follow_single_edge(&self, from: Uuid) -> Option<Uuid> {
        self.workflow
            .edges
            .iter()
            .find(|e| e.from == from && e.output.is_none())
            .map(|e| e.to)
    }

    /// Follow a specific labeled edge from a control flow node.
    pub(crate) fn follow_edge(&self, from: Uuid, output: &EdgeOutput) -> Option<Uuid> {
        self.workflow
            .edges
            .iter()
            .find(|e| e.from == from && e.output.as_ref() == Some(output))
            .map(|e| e.to)
    }

    /// Follow the "default" edge when a control flow node is disabled.
    /// Falls through to the non-executing branch: IfFalse, LoopDone, or
    /// the first available outgoing edge for Switch.
    fn follow_disabled_edge(&self, node_id: Uuid, node_type: &NodeType) -> Option<Uuid> {
        match node_type {
            NodeType::If(_) => self.follow_edge(node_id, &EdgeOutput::IfFalse),
            NodeType::Loop(_) => self.follow_edge(node_id, &EdgeOutput::LoopDone),
            NodeType::Switch(_) => self
                .follow_edge(node_id, &EdgeOutput::SwitchDefault)
                .or_else(|| {
                    // No default edge — pick the first case edge as fallthrough
                    self.workflow
                        .edges
                        .iter()
                        .find(|e| e.from == node_id && e.output.is_some())
                        .map(|e| e.to)
                }),
            // EndLoop and regular nodes: follow_single_edge is fine
            _ => self.follow_single_edge(node_id),
        }
    }

    /// Evaluate a control flow node and return the next node to visit.
    fn eval_control_flow(
        &mut self,
        node_id: Uuid,
        node_name: &str,
        node_type: &NodeType,
    ) -> Option<Uuid> {
        match node_type {
            NodeType::If(params) => {
                self.log(format!("Evaluating If: {}", node_name));
                let result = self.context.evaluate_condition(&params.condition);
                let resolved_left = self.context.resolve_value_ref(&params.condition.left);
                let resolved_right = self.context.resolve_value_ref(&params.condition.right);
                let output_taken = if result { "IfTrue" } else { "IfFalse" };

                self.record_event(
                    None,
                    "branch_evaluated",
                    serde_json::json!({
                        "node_id": node_id.to_string(),
                        "node_name": node_name,
                        "condition": format!("{:?} {:?} {:?}",
                            params.condition.left,
                            params.condition.operator,
                            params.condition.right),
                        "resolved_left": resolved_left,
                        "resolved_right": resolved_right,
                        "result": result,
                        "output_taken": output_taken,
                    }),
                );

                if result {
                    self.follow_edge(node_id, &EdgeOutput::IfTrue)
                } else {
                    self.follow_edge(node_id, &EdgeOutput::IfFalse)
                }
            }

            NodeType::Switch(params) => {
                self.log(format!("Evaluating Switch: {}", node_name));
                let matched = params
                    .cases
                    .iter()
                    .find(|c| self.context.evaluate_condition(&c.condition));

                let (output_taken, next) = match matched {
                    Some(case) => {
                        let name = case.name.clone();
                        (
                            format!("SwitchCase({})", name),
                            self.follow_edge(node_id, &EdgeOutput::SwitchCase { name }),
                        )
                    }
                    None => (
                        "SwitchDefault".to_string(),
                        self.follow_edge(node_id, &EdgeOutput::SwitchDefault),
                    ),
                };

                self.record_event(
                    None,
                    "branch_evaluated",
                    serde_json::json!({
                        "node_id": node_id.to_string(),
                        "node_name": node_name,
                        "output_taken": output_taken,
                    }),
                );

                if next.is_none() {
                    self.log(format!(
                        "Warning: Switch '{}' had no matching case and no default edge — workflow path ends here",
                        node_name
                    ));
                }

                next
            }

            // Do-while semantics: exit condition is NOT checked on iteration 0.
            // The loop body always runs at least once. This is intentional for UI
            // automation where the common pattern is "try action, check result,
            // retry if needed."
            NodeType::Loop(params) => {
                let iteration = *self.context.loop_counters.get(&node_id).unwrap_or(&0);

                let exit_reason = if iteration >= params.max_iterations {
                    self.log(format!(
                        "Loop '{}' hit max iterations ({}), exiting",
                        node_name, params.max_iterations
                    ));
                    self.record_event(
                        None,
                        "loop_exited",
                        serde_json::json!({
                            "node_id": node_id.to_string(),
                            "node_name": node_name,
                            "reason": "max_iterations",
                            "iterations_completed": iteration,
                        }),
                    );
                    Some(LoopExitReason::MaxIterations)
                } else if iteration > 0 && self.context.evaluate_condition(&params.exit_condition) {
                    self.log(format!(
                        "Loop '{}' exit condition met after {} iterations",
                        node_name, iteration
                    ));
                    self.record_event(
                        None,
                        "loop_exited",
                        serde_json::json!({
                            "node_id": node_id.to_string(),
                            "node_name": node_name,
                            "reason": "condition_met",
                            "iterations_completed": iteration,
                        }),
                    );
                    Some(LoopExitReason::ConditionMet)
                } else {
                    None
                };

                if let Some(reason) = exit_reason {
                    self.pending_loop_exit = Some(PendingLoopExit {
                        node_id,
                        loop_name: node_name.to_string(),
                        reason,
                        iterations: iteration,
                    });
                    self.context.loop_counters.remove(&node_id);
                    self.follow_edge(node_id, &EdgeOutput::LoopDone)
                } else {
                    self.log(format!("Loop '{}' iteration {}", node_name, iteration));
                    self.record_event(
                        None,
                        "loop_iteration",
                        serde_json::json!({
                            "node_id": node_id.to_string(),
                            "node_name": node_name,
                            "iteration": iteration,
                        }),
                    );
                    *self.context.loop_counters.entry(node_id).or_insert(0) += 1;
                    self.follow_edge(node_id, &EdgeOutput::LoopBody)
                }
            }

            NodeType::EndLoop(params) => {
                self.log(format!("EndLoop: jumping back to Loop {}", params.loop_id));
                Some(params.loop_id)
            }

            _ => unreachable!("eval_control_flow called on non-control-flow node"),
        }
    }

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

    /// Store node outputs in RuntimeContext for condition evaluation.
    fn extract_and_store_variables(
        &mut self,
        node_name: &str,
        node_result: &Value,
        node_type: &NodeType,
        node_run: Option<&NodeRun>,
    ) {
        let sanitized = sanitize_node_name(node_name);
        self.context.set_variable(
            format!("{}.success", sanitized),
            serde_json::Value::Bool(true),
        );
        self.record_event(
            node_run,
            "variable_set",
            serde_json::json!({
                "variable": format!("{}.success", sanitized),
                "value": true,
            }),
        );
        let extracted =
            extract_result_variables(&mut self.context, &sanitized, node_result, node_type);
        for (var_name, var_value) in &extracted {
            self.record_event(
                node_run,
                "variable_set",
                serde_json::json!({
                    "variable": var_name,
                    "value": var_value,
                }),
            );
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
                                self.log(
                                    "Supervision: user chose Retry (continuing past loop)"
                                        .to_string(),
                                );
                            }
                            SupervisionAction::Skip => {
                                self.log("Supervision: user chose Skip for loop exit".to_string());
                            }
                            SupervisionAction::Abort => {
                                self.log(
                                    "Supervision: user chose Abort after loop exit".to_string(),
                                );
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
                                        self.log("Supervision: user chose Retry".to_string());
                                        continue;
                                    }
                                    SupervisionAction::Skip => {
                                        self.log("Supervision: user chose Skip".to_string());
                                        break (true, false);
                                    }
                                    SupervisionAction::Abort => {
                                        self.log("Supervision: user chose Abort".to_string());
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
                // Only finalize + emit for supervision aborts; execution
                // errors already handle this in the Err arm above.
                if was_supervision_abort {
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
            self.log("Running post-workflow check evaluation...".to_string());

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

        // Save decision cache after Test mode runs
        if self.execution_mode == ExecutionMode::Test {
            let save_result = self
                .decision_cache
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .save(&self.storage.cache_path());
            match save_result {
                Ok(()) => self.log("Decision cache saved".to_string()),
                Err(e) => self.log(format!("Warning: failed to save decision cache: {}", e)),
            }
        }

        if completed_normally {
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

/// Extract type-specific variables from a tool result into the RuntimeContext.
///
/// Returns the list of `(variable_name, value)` pairs that were set, for tracing.
///
/// Contract:
/// - `.result` is always set as raw `Value` (JSON value for structured results,
///   string for text, empty string for null/AiStep).
/// - Objects: each top-level field → `<prefix>.<key>`, plus `.result` = raw Value.
/// - Arrays: `.found` (bool), `.count`, first-element fields, plus typed alias
///   (e.g. `.windows` for `ListWindows`), plus `.result` = raw Value.
/// - Strings: `.result` only.
/// - Null: `.result = ""`.
fn extract_result_variables(
    ctx: &mut RuntimeContext,
    prefix: &str,
    result: &Value,
    node_type: &NodeType,
) -> Vec<(String, Value)> {
    let mut vars: Vec<(String, Value)> = Vec::new();

    let mut set = |name: String, value: Value| {
        ctx.set_variable(name.clone(), value.clone());
        vars.push((name, value));
    };

    match result {
        Value::Object(map) => {
            for (key, value) in map {
                set(format!("{}.{}", prefix, key), value.clone());
            }
            set(format!("{}.result", prefix), result.clone());
        }
        Value::Array(arr) => {
            let found = !arr.is_empty();
            set(format!("{}.found", prefix), Value::Bool(found));
            set(
                format!("{}.count", prefix),
                Value::Number(serde_json::Number::from(arr.len())),
            );
            if let Some(Value::Object(first)) = arr.first() {
                for (key, value) in first {
                    set(format!("{}.{}", prefix, key), value.clone());
                }
            }
            // Typed alias for the full array based on node type
            if let Some(alias) = array_alias_for_node_type(node_type) {
                set(format!("{}.{}", prefix, alias), result.clone());
            }
            set(format!("{}.result", prefix), result.clone());
        }
        Value::String(s) => {
            set(format!("{}.result", prefix), Value::String(s.clone()));
        }
        Value::Null => {
            set(format!("{}.result", prefix), Value::String(String::new()));
        }
        other => {
            set(format!("{}.result", prefix), other.clone());
        }
    }

    vars
}

/// Returns a typed alias name for array results based on node type.
///
/// For example, `ListWindows` results get stored as `<prefix>.windows`.
fn array_alias_for_node_type(node_type: &NodeType) -> Option<&'static str> {
    match node_type {
        NodeType::ListWindows(_) => Some("windows"),
        NodeType::FindText(_) => Some("matches"),
        NodeType::FindImage(_) => Some("matches"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_variables_from_object() {
        let mut ctx = RuntimeContext::new();
        let result = serde_json::json!({"text": "Login", "x": 100.5, "y": 200.0});
        let node_type = NodeType::Click(clickweave_core::ClickParams::default());
        let vars = extract_result_variables(&mut ctx, "click", &result, &node_type);

        assert_eq!(
            ctx.get_variable("click.text"),
            Some(&Value::String("Login".into()))
        );
        assert!(ctx.get_variable("click.x").is_some());
        assert!(ctx.get_variable("click.y").is_some());
        // .result is the raw JSON Value
        assert_eq!(ctx.get_variable("click.result"), Some(&result));
        assert!(!vars.is_empty());
    }

    #[test]
    fn extract_variables_from_array_find_text() {
        let mut ctx = RuntimeContext::new();
        let result = serde_json::json!([
            {"text": "Login", "x": 100, "y": 200},
            {"text": "Logout", "x": 300, "y": 400}
        ]);
        let node_type = NodeType::FindText(clickweave_core::FindTextParams::default());
        let vars = extract_result_variables(&mut ctx, "find_text", &result, &node_type);

        assert_eq!(
            ctx.get_variable("find_text.found"),
            Some(&Value::Bool(true))
        );
        assert_eq!(
            ctx.get_variable("find_text.text"),
            Some(&Value::String("Login".into()))
        );
        assert_eq!(
            ctx.get_variable("find_text.count"),
            Some(&Value::Number(serde_json::Number::from(2)))
        );
        // .result is raw JSON Value (not stringified)
        assert_eq!(ctx.get_variable("find_text.result"), Some(&result));
        // .matches typed alias for the full array
        assert_eq!(ctx.get_variable("find_text.matches"), Some(&result));
        assert!(!vars.is_empty());
    }

    #[test]
    fn extract_variables_from_array_list_windows() {
        let mut ctx = RuntimeContext::new();
        let result = serde_json::json!([{"name": "Safari", "id": 1}]);
        let node_type = NodeType::ListWindows(clickweave_core::ListWindowsParams::default());
        extract_result_variables(&mut ctx, "list_windows", &result, &node_type);

        assert_eq!(
            ctx.get_variable("list_windows.found"),
            Some(&Value::Bool(true))
        );
        // .windows typed alias
        assert_eq!(ctx.get_variable("list_windows.windows"), Some(&result));
    }

    #[test]
    fn extract_variables_from_empty_array() {
        let mut ctx = RuntimeContext::new();
        let result = serde_json::json!([]);
        let node_type = NodeType::FindText(clickweave_core::FindTextParams::default());
        extract_result_variables(&mut ctx, "find_text", &result, &node_type);

        assert_eq!(
            ctx.get_variable("find_text.found"),
            Some(&Value::Bool(false))
        );
        assert_eq!(
            ctx.get_variable("find_text.count"),
            Some(&Value::Number(serde_json::Number::from(0)))
        );
    }

    #[test]
    fn extract_variables_from_string() {
        let mut ctx = RuntimeContext::new();
        let result = Value::String("screenshot taken".into());
        let node_type = NodeType::TakeScreenshot(clickweave_core::TakeScreenshotParams::default());
        extract_result_variables(&mut ctx, "screenshot", &result, &node_type);

        assert_eq!(
            ctx.get_variable("screenshot.result"),
            Some(&Value::String("screenshot taken".into()))
        );
    }

    #[test]
    fn extract_variables_null_sets_empty_result() {
        let mut ctx = RuntimeContext::new();
        let node_type = NodeType::Click(clickweave_core::ClickParams::default());
        let vars = extract_result_variables(&mut ctx, "node", &Value::Null, &node_type);
        assert_eq!(
            ctx.get_variable("node.result"),
            Some(&Value::String(String::new()))
        );
        assert_eq!(vars.len(), 1);
    }

    #[test]
    fn extract_variables_ai_step_returns_text() {
        let mut ctx = RuntimeContext::new();
        let result = Value::String("The login button is at the top right".into());
        let node_type = NodeType::AiStep(clickweave_core::AiStepParams::default());
        extract_result_variables(&mut ctx, "ai_step", &result, &node_type);

        assert_eq!(
            ctx.get_variable("ai_step.result"),
            Some(&Value::String(
                "The login button is at the top right".into()
            ))
        );
    }
}
