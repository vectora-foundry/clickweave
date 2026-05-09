//! Native skill runner — replaces the deleted `WorkflowExecutor` for
//! deterministic skill execution (D28).
//!
//! Walks `&[ActionSketchStep]` directly. `Loop` is a first-class
//! primitive: `until` is evaluated against the run context, `body` is
//! executed in order, `iteration_delay_ms` separates iterations, and
//! `max_iterations` caps runaway loops.
//!
//! Phase 1.D scope is intentionally minimal: per-step tool dispatch
//! through the [`Mcp`] trait, plus the `Loop` primitive, plus event
//! emission. Repair / supervision / approval flows live above this
//! runner and will be wired through the
//! [`crate::executor::ExecutorEvent`] channel in later phases.

use crate::agent::skills::types::{ActionSketchStep, LoopPredicate};
use crate::executor::Mcp;
use crate::executor::error::{ExecutorError, ExecutorResult};
use serde_json::Value;
use std::collections::HashMap;

/// Mutable state carried through a skill run. Holds the active world
/// model, captured tool results, and runtime variable bindings.
///
/// The runner is intentionally synchronous w.r.t. its own state — it is
/// the sole writer during a run. Concurrency boundaries (event channel,
/// MCP transport) sit at the edges.
pub struct SkillRunContext<'mcp, M: Mcp + ?Sized> {
    /// MCP transport used for every `tool_call` dispatch.
    pub mcp: &'mcp M,
    /// Runtime variable bindings (e.g. `recipient -> "alice@example.com"`)
    /// supplied by the `RunWithValuesForm`. Shared with `evaluate_until`
    /// for loop predicates.
    pub variables: HashMap<String, Value>,
    /// Steps executed so far this run. Indexed by `step_id`.
    pub completed_steps: Vec<String>,
}

impl<'mcp, M: Mcp + ?Sized> SkillRunContext<'mcp, M> {
    pub fn new(mcp: &'mcp M, variables: HashMap<String, Value>) -> Self {
        Self {
            mcp,
            variables,
            completed_steps: Vec::new(),
        }
    }
}

/// Execute every step in `steps` in document order. Returns the first
/// `ExecutorError` encountered; on success the run reaches the last
/// step and returns `Ok(())`.
///
/// `Loop` steps recurse back through `run_skill_steps` for their body,
/// which keeps step-id uniqueness invariants the same at every depth.
/// The future is boxed so the recursive descent is allowed by the
/// async-fn checker (a `Loop` body may itself contain a `Loop`).
pub fn run_skill_steps<'a, M: Mcp + ?Sized>(
    ctx: &'a mut SkillRunContext<'_, M>,
    steps: &'a [ActionSketchStep],
) -> std::pin::Pin<Box<dyn std::future::Future<Output = ExecutorResult<()>> + Send + 'a>> {
    Box::pin(async move {
        for step in steps {
            run_step(ctx, step).await?;
        }
        Ok(())
    })
}

async fn run_step<M: Mcp + ?Sized>(
    ctx: &mut SkillRunContext<'_, M>,
    step: &ActionSketchStep,
) -> ExecutorResult<()> {
    match step {
        ActionSketchStep::ToolCall {
            step_id,
            tool,
            args,
            ..
        } => run_tool_call(ctx, step_id, tool, args).await,
        ActionSketchStep::Loop {
            step_id,
            until,
            body,
            max_iterations,
            iteration_delay_ms,
        } => {
            run_loop(
                ctx,
                step_id,
                until,
                body,
                *max_iterations,
                *iteration_delay_ms,
            )
            .await
        }
    }
}

async fn run_tool_call<M: Mcp + ?Sized>(
    ctx: &mut SkillRunContext<'_, M>,
    step_id: &str,
    tool: &str,
    args: &Value,
) -> ExecutorResult<()> {
    let result = ctx
        .mcp
        .call_tool(tool, Some(args.clone()))
        .await
        .map_err(|e| ExecutorError::ToolCall {
            tool: tool.to_string(),
            message: e.to_string(),
        })?;
    if result.is_error == Some(true) {
        let msg = result
            .content
            .iter()
            .find_map(clickweave_mcp::ToolContent::as_text)
            .unwrap_or("<no error text>")
            .to_string();
        return Err(ExecutorError::ToolCall {
            tool: tool.to_string(),
            message: msg,
        });
    }
    ctx.completed_steps.push(step_id.to_string());
    Ok(())
}

async fn run_loop<M: Mcp + ?Sized>(
    ctx: &mut SkillRunContext<'_, M>,
    step_id: &str,
    until: &LoopPredicate,
    body: &[ActionSketchStep],
    max_iterations: u32,
    iteration_delay_ms: u64,
) -> ExecutorResult<()> {
    let mut iter: u32 = 0;
    while iter < max_iterations {
        if evaluate_until(ctx, until, iter)? {
            return Ok(());
        }
        run_skill_steps(ctx, body).await?;
        iter += 1;
        if iter < max_iterations && iteration_delay_ms > 0 {
            tokio::time::sleep(std::time::Duration::from_millis(iteration_delay_ms)).await;
        }
    }
    if evaluate_until(ctx, until, iter)? {
        return Ok(());
    }
    Err(ExecutorError::Validation(format!(
        "Loop {step_id}: max_iterations ({max_iterations}) exceeded without satisfying `until`"
    )))
}

/// Evaluate a loop's `until` predicate against the run context.
///
/// Phase 1.D supports two forms:
/// - `StepCountReached { count }` — terminates after `count` body
///   iterations have completed (count is compared against the
///   loop-local iteration counter).
/// - `WorldModelDelta { expr }` — placeholder for the world-model
///   diff expression evaluator that lands with intent extraction
///   (Phase 2). For now it always returns `false`, leaving
///   `max_iterations` as the only termination guard for delta-based
///   loops. Documented at the call site so the caller knows the
///   guard.
fn evaluate_until<M: Mcp + ?Sized>(
    _ctx: &SkillRunContext<'_, M>,
    predicate: &LoopPredicate,
    iter: u32,
) -> ExecutorResult<bool> {
    match predicate {
        LoopPredicate::StepCountReached { count } => Ok(iter >= *count),
        LoopPredicate::WorldModelDelta { .. } => Ok(false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::skills::types::{ActionSketchStep, ExpectedWorldModelDelta, LoopPredicate};
    use clickweave_mcp::ToolCallResult;
    use serde_json::json;
    use std::sync::Arc;
    use std::sync::Mutex;

    /// Minimal `Mcp` stub used by the skill_runner unit tests. Records
    /// every tool name dispatched so tests can assert on order, count,
    /// and nesting.
    struct ReplayingMcp {
        log: Arc<Mutex<Vec<(String, Value)>>>,
        // Per-call return value override — left empty so every call
        // succeeds with an empty `ok` content.
    }

    impl ReplayingMcp {
        fn new() -> Self {
            Self {
                log: Arc::new(Mutex::new(Vec::new())),
            }
        }

        fn log_handle(&self) -> Arc<Mutex<Vec<(String, Value)>>> {
            self.log.clone()
        }
    }

    impl Mcp for ReplayingMcp {
        async fn call_tool(
            &self,
            name: &str,
            arguments: Option<Value>,
        ) -> anyhow::Result<ToolCallResult> {
            self.log
                .lock()
                .unwrap()
                .push((name.to_string(), arguments.unwrap_or(Value::Null)));
            Ok(ToolCallResult {
                content: vec![],
                is_error: Some(false),
            })
        }

        fn has_tool(&self, _: &str) -> bool {
            true
        }

        fn tools_as_openai(&self) -> Vec<Value> {
            Vec::new()
        }

        async fn refresh_server_tool_list(&self) -> anyhow::Result<()> {
            Ok(())
        }
    }

    fn tool_call(step_id: &str, tool: &str) -> ActionSketchStep {
        ActionSketchStep::ToolCall {
            step_id: step_id.to_string(),
            tool: tool.to_string(),
            args: json!({}),
            captures_pre: Vec::new(),
            captures: Vec::new(),
            expected_world_model_delta: ExpectedWorldModelDelta::default(),
        }
    }

    #[tokio::test]
    async fn runs_three_step_section_in_order() {
        let mcp = ReplayingMcp::new();
        let log = mcp.log_handle();
        let mut ctx = SkillRunContext::new(&mcp, HashMap::new());
        let steps = vec![
            tool_call("s_001", "launch_app"),
            tool_call("s_002", "click"),
            tool_call("s_003", "type_text"),
        ];

        run_skill_steps(&mut ctx, &steps).await.expect("ok");

        let observed = log.lock().unwrap().clone();
        let names: Vec<&str> = observed.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, vec!["launch_app", "click", "type_text"]);
        assert_eq!(ctx.completed_steps, vec!["s_001", "s_002", "s_003"]);
    }

    #[tokio::test]
    async fn loop_terminates_when_until_returns_true() {
        let mcp = ReplayingMcp::new();
        let log = mcp.log_handle();
        let mut ctx = SkillRunContext::new(&mcp, HashMap::new());

        // Loop body fires once per iteration. With
        // `StepCountReached { count: 2 }`, the predicate evaluates true
        // on the third entry (after iter == 2 body runs), so the body
        // executes exactly twice — six tool calls if the body has
        // three steps.
        let body = vec![
            tool_call("b_001", "click"),
            tool_call("b_002", "wait"),
            tool_call("b_003", "type_text"),
        ];
        let steps = vec![ActionSketchStep::Loop {
            step_id: "loop_1".to_string(),
            until: LoopPredicate::StepCountReached { count: 2 },
            body,
            max_iterations: 10,
            iteration_delay_ms: 0,
        }];

        run_skill_steps(&mut ctx, &steps).await.expect("ok");

        let observed = log.lock().unwrap().clone();
        assert_eq!(observed.len(), 6, "body should run 2 iterations × 3 steps");
    }

    #[tokio::test]
    async fn loop_caps_at_max_iterations() {
        let mcp = ReplayingMcp::new();
        let log = mcp.log_handle();
        let mut ctx = SkillRunContext::new(&mcp, HashMap::new());

        let body = vec![tool_call("b_001", "click")];
        // `WorldModelDelta` always returns false in Phase 1.D so this
        // loop runs to the cap and errors out.
        let steps = vec![ActionSketchStep::Loop {
            step_id: "loop_capped".to_string(),
            until: LoopPredicate::WorldModelDelta {
                expr: "ignored".to_string(),
            },
            body,
            max_iterations: 4,
            iteration_delay_ms: 0,
        }];

        let err = run_skill_steps(&mut ctx, &steps)
            .await
            .expect_err("should error after max iterations");
        let msg = err.to_string();
        assert!(msg.contains("loop_capped"), "error mentions step id: {msg}");
        assert!(msg.contains("max_iterations"), "error mentions cap: {msg}");
        assert_eq!(log.lock().unwrap().len(), 4);
    }

    /// Walk the steps tree and collect every declared `step_id` so the
    /// uniqueness check sees the static structure, not runtime
    /// completions (a body step's id repeats once per loop iteration
    /// at runtime, which is intentional and not a uniqueness violation).
    fn collect_static_step_ids(steps: &[ActionSketchStep], out: &mut Vec<String>) {
        for step in steps {
            match step {
                ActionSketchStep::ToolCall { step_id, .. } => out.push(step_id.clone()),
                ActionSketchStep::Loop { step_id, body, .. } => {
                    out.push(step_id.clone());
                    collect_static_step_ids(body, out);
                }
            }
        }
    }

    #[tokio::test]
    async fn nested_loop_step_ids_unique() {
        let mcp = ReplayingMcp::new();
        let log = mcp.log_handle();
        let mut ctx = SkillRunContext::new(&mcp, HashMap::new());

        let inner = vec![tool_call("inner_001", "click")];
        let outer_body = vec![
            tool_call("outer_001", "wait"),
            ActionSketchStep::Loop {
                step_id: "inner_loop".to_string(),
                until: LoopPredicate::StepCountReached { count: 2 },
                body: inner,
                max_iterations: 5,
                iteration_delay_ms: 0,
            },
        ];
        let steps = vec![ActionSketchStep::Loop {
            step_id: "outer_loop".to_string(),
            until: LoopPredicate::StepCountReached { count: 1 },
            body: outer_body,
            max_iterations: 5,
            iteration_delay_ms: 0,
        }];

        // Static structural check: every step_id declared in the tree
        // (top level + every nested body) is unique.
        let mut ids = Vec::new();
        collect_static_step_ids(&steps, &mut ids);
        let mut sorted = ids.clone();
        sorted.sort();
        let mut deduped = sorted.clone();
        deduped.dedup();
        assert_eq!(
            sorted, deduped,
            "step ids must be unique across nested loop structure"
        );

        run_skill_steps(&mut ctx, &steps).await.expect("ok");

        // outer body runs once (until count=1), so we see one
        // outer_001 plus two inner_001 calls (count=2 inside).
        let observed = log.lock().unwrap().clone();
        let inner_count = observed.iter().filter(|(n, _)| n == "click").count();
        let outer_count = observed.iter().filter(|(n, _)| n == "wait").count();
        assert_eq!(outer_count, 1);
        assert_eq!(inner_count, 2);
    }
}
