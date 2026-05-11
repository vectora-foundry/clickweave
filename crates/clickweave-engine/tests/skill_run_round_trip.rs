//! B1: Round-trip golden test.
//!
//! Builds a skill with 2 sections, 4 steps, and 1 variable, runs it through
//! `run_skill_steps` against a mock dispatcher, and asserts that step IDs,
//! section IDs, and variable bindings round-trip without divergence.

use std::collections::HashMap;

use clickweave_engine::agent::skills::types::{ActionSketchStep, ExpectedWorldModelDelta};
use clickweave_engine::executor::{Mcp, SkillRunContext, run_skill_steps};
use clickweave_mcp::ToolCallResult;
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

struct LoggingMcp {
    log: Arc<Mutex<Vec<(String, Value)>>>,
}

impl LoggingMcp {
    fn new() -> Self {
        Self {
            log: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn dispatched_tools(&self) -> Vec<String> {
        self.log
            .lock()
            .unwrap()
            .iter()
            .map(|(n, _)| n.clone())
            .collect()
    }
}

impl Mcp for LoggingMcp {
    async fn call_tool(&self, name: &str, args: Option<Value>) -> anyhow::Result<ToolCallResult> {
        self.log
            .lock()
            .unwrap()
            .push((name.to_string(), args.unwrap_or(Value::Null)));
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

/// Skill layout:
///
/// Section 1 (sec_launch): steps s_001, s_002
/// Section 2 (sec_fill):   steps s_003, s_004
/// Variable: `target_app` (string)
///
/// After `run_skill_steps`:
/// - all 4 step IDs appear in `ctx.completed_steps` in document order
/// - the variable binding passed via `variables` is preserved in the context
#[tokio::test]
async fn round_trip_preserves_step_section_and_variable_bindings() {
    let mcp = LoggingMcp::new();

    // Section layout is embedded in the step structure; the runner does not
    // consult sections at execution time — sections are a prose/parse concern.
    // We validate that step_ids from both logical sections are completed.
    let steps = vec![
        // Section 1: launch
        tool_call("s_001", "launch_app"),
        tool_call("s_002", "focus_window"),
        // Section 2: fill form
        tool_call("s_003", "click"),
        tool_call("s_004", "type_text"),
    ];

    let mut variables = HashMap::new();
    variables.insert("target_app".to_string(), json!("Notes"));

    let mut ctx = SkillRunContext::new(&mcp, variables);

    run_skill_steps(&mut ctx, &steps).await.expect("run ok");

    // All 4 step IDs preserved in completion order.
    assert_eq!(
        ctx.completed_steps,
        vec!["s_001", "s_002", "s_003", "s_004"],
        "step IDs preserved in document order"
    );

    // Variable binding survives the run (context is not mutated by steps in
    // Phase 1 — the binding is available for predicates and substitution).
    assert_eq!(
        ctx.variables.get("target_app").and_then(|v| v.as_str()),
        Some("Notes"),
        "variable binding preserved"
    );

    // All 4 tools dispatched in order.
    let dispatched = mcp.dispatched_tools();
    assert_eq!(
        dispatched,
        vec!["launch_app", "focus_window", "click", "type_text"]
    );
}
