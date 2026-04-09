use super::retry_context::{ExecutionHistoryEntry, RetryContext};
use super::{LoopExitReason, PendingLoopExit, WorkflowExecutor};
use clickweave_core::{EdgeOutput, NodeType};
use clickweave_llm::ChatBackend;
use uuid::Uuid;

impl<C: ChatBackend> WorkflowExecutor<C> {
    /// Evaluate a control flow node and return the next node to visit.
    pub(crate) fn eval_control_flow(
        &mut self,
        node_id: Uuid,
        node_name: &str,
        node_type: &NodeType,
        retry_ctx: &mut RetryContext,
    ) -> Option<Uuid> {
        match node_type {
            NodeType::If(params) => {
                self.log(format!("Evaluating If: {}", node_name));
                let result = self.context.evaluate_condition(&params.condition);
                let resolved_left = self.context.resolve_output_ref(&params.condition.left);
                let resolved_right = self
                    .context
                    .resolve_condition_value(&params.condition.right);
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

                retry_ctx
                    .execution_history
                    .push(ExecutionHistoryEntry::BranchTaken {
                        node_name: node_name.to_string(),
                        outcome: output_taken.to_string(),
                    });

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

                retry_ctx
                    .execution_history
                    .push(ExecutionHistoryEntry::BranchTaken {
                        node_name: node_name.to_string(),
                        outcome: output_taken,
                    });

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

                let exit_reason = if iteration > 0 && iteration >= params.max_iterations {
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
                    let reason_str = match reason {
                        LoopExitReason::MaxIterations => "max_iterations",
                        LoopExitReason::ConditionMet => "condition_met",
                    };
                    retry_ctx
                        .execution_history
                        .push(ExecutionHistoryEntry::LoopExited {
                            node_name: node_name.to_string(),
                            reason: reason_str.to_string(),
                            iterations: iteration,
                        });
                    retry_ctx.pending_loop_exit = Some(PendingLoopExit {
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
                    retry_ctx
                        .execution_history
                        .push(ExecutionHistoryEntry::LoopIteration {
                            node_name: node_name.to_string(),
                            iteration,
                        });
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
}
