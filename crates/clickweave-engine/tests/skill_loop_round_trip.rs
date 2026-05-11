//! B2: Loop golden test.
//!
//! Builds a skill with a `Loop { until: StepCountReached, body, max_iterations }`
//! step, runs it via `run_skill_steps`, and asserts the loop body fires the
//! expected number of times before the predicate terminates the loop.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use clickweave_engine::agent::skills::types::{
    ActionSketchStep, ExpectedWorldModelDelta, LoopPredicate,
};
use clickweave_engine::executor::{Mcp, SkillRunContext, run_skill_steps};
use clickweave_mcp::ToolCallResult;
use serde_json::{Value, json};

struct CountingMcp {
    calls: Arc<Mutex<Vec<String>>>,
}

impl CountingMcp {
    fn new() -> Self {
        Self {
            calls: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn call_count(&self) -> usize {
        self.calls.lock().unwrap().len()
    }

    fn all_calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }
}

impl Mcp for CountingMcp {
    async fn call_tool(&self, name: &str, args: Option<Value>) -> anyhow::Result<ToolCallResult> {
        let _ = args;
        self.calls.lock().unwrap().push(name.to_string());
        Ok(ToolCallResult {
            content: vec![],
            is_error: Some(false),
        })
    }

    fn has_tool(&self, _: &str) -> bool {
        true
    }

    fn tools_as_openai(&self) -> Vec<Value> {
        vec![]
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
        captures_pre: vec![],
        captures: vec![],
        expected_world_model_delta: ExpectedWorldModelDelta::default(),
        requires_approval: None,
    }
}

/// The existing skill_runner unit tests cover `StepCountReached { count: 2 }`
/// → 2 body iterations. Here we exercise a count of 3 (body fires 3 times)
/// to confirm the first-class `Loop` primitive and the existing semantics
/// established by `loop_terminates_when_until_returns_true`.
///
/// Loop semantics: `StepCountReached { count: N }` — the predicate fires at
/// the TOP of each iteration (before the body). When `iter >= N` the loop
/// exits WITHOUT running the body that iteration. So body executes exactly
/// N times for count = N.
#[tokio::test]
async fn loop_body_fires_n_times_for_step_count_n() {
    let mcp = CountingMcp::new();

    let body = vec![tool_call("b_001", "click"), tool_call("b_002", "wait")];

    let steps = vec![ActionSketchStep::Loop {
        step_id: "poll_loop".to_string(),
        until: LoopPredicate::StepCountReached { count: 3 },
        body,
        max_iterations: 10,
        iteration_delay_ms: 0,
    }];

    let mut ctx = SkillRunContext::new(&mcp, HashMap::new());
    run_skill_steps(&mut ctx, &steps)
        .await
        .expect("loop should terminate");

    // Body has 2 steps; loop runs 3 times → 6 total tool calls.
    assert_eq!(
        mcp.call_count(),
        6,
        "body fires 3 times × 2 steps = 6 calls"
    );

    let calls = mcp.all_calls();
    // click and wait should alternate in pairs.
    assert_eq!(calls[0], "click");
    assert_eq!(calls[1], "wait");
    assert_eq!(calls[2], "click");
    assert_eq!(calls[3], "wait");
    assert_eq!(calls[4], "click");
    assert_eq!(calls[5], "wait");
}

/// Verify max_iterations caps a WorldModelDelta loop that never terminates
/// naturally (Phase 1.D: `WorldModelDelta` always returns false).
#[tokio::test]
async fn loop_caps_at_max_iterations_with_world_model_predicate() {
    let mcp = CountingMcp::new();

    let body = vec![tool_call("b_001", "take_screenshot")];

    let steps = vec![ActionSketchStep::Loop {
        step_id: "poll_screenshots".to_string(),
        until: LoopPredicate::WorldModelDelta {
            expr: "ui_changed".to_string(),
        },
        body,
        max_iterations: 5,
        iteration_delay_ms: 0,
    }];

    let mut ctx = SkillRunContext::new(&mcp, HashMap::new());
    let err = run_skill_steps(&mut ctx, &steps)
        .await
        .expect_err("WorldModelDelta always false → max_iterations exceeded");

    assert!(
        err.to_string().contains("poll_screenshots"),
        "error identifies loop step id"
    );
    assert_eq!(mcp.call_count(), 5, "body fires max_iterations=5 times");
}
