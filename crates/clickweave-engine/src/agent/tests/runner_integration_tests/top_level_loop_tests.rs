use super::super::super::test_stubs::{ScriptedLlm, StaticMcp, llm_reply_tool};
use crate::agent::runner::StateRunner;
use crate::agent::types::{AgentConfig, TerminalReason};
use crate::executor::Mcp;

#[tokio::test]
async fn run_completes_on_agent_done_after_two_tool_calls() {
    let llm = ScriptedLlm::new(vec![
        llm_reply_tool(
            "cdp_find_elements",
            serde_json::json!({"query": "", "max_results": 300}),
        ),
        llm_reply_tool("cdp_click", serde_json::json!({"uid": "d1"})),
        llm_reply_tool("agent_done", serde_json::json!({"summary": "ok"})),
    ]);
    let mcp = StaticMcp::with_tools(&["cdp_find_elements", "cdp_click"])
        .with_reply(
            "cdp_find_elements",
            r#"{"page_url":"about:blank","source":"cdp","matches":[]}"#,
        )
        .with_reply("cdp_click", "clicked");

    let tools = mcp.tools_as_openai();
    let runner = StateRunner::new("goal".to_string(), AgentConfig::default());
    let state = runner
        .run(
            &llm,
            &mcp,
            "goal".to_string(),
            crate::agent::trace_graph::AgentTraceGraph::new(),
            tools,
            None,
        )
        .await
        .expect("run ok");

    assert_eq!(
        state.steps.len(),
        2,
        "two dispatched tool calls should be recorded as steps"
    );
    assert!(state.completed, "agent_done should mark state.completed");
    assert!(
        matches!(
            state.terminal_reason,
            Some(TerminalReason::Completed { .. })
        ),
        "terminal reason should be Completed, got {:?}",
        state.terminal_reason,
    );
}

#[tokio::test]
async fn run_terminates_at_max_steps_without_completion() {
    let llm = ScriptedLlm::repeat(|| {
        llm_reply_tool(
            "cdp_find_elements",
            serde_json::json!({"query": "", "max_results": 300}),
        )
    });
    let mcp = StaticMcp::with_tools(&["cdp_find_elements"]).with_reply(
        "cdp_find_elements",
        r#"{"page_url":"about:blank","source":"cdp","matches":[]}"#,
    );

    let tools = mcp.tools_as_openai();
    let cfg = AgentConfig {
        max_steps: 3,
        ..AgentConfig::default()
    };
    let runner = StateRunner::new("goal".to_string(), cfg);
    let state = runner
        .run(
            &llm,
            &mcp,
            "goal".to_string(),
            crate::agent::trace_graph::AgentTraceGraph::new(),
            tools,
            None,
        )
        .await
        .expect("run ok");

    assert_eq!(state.steps.len(), 3);
    assert!(!state.completed);
    assert!(
        matches!(
            state.terminal_reason,
            Some(TerminalReason::MaxStepsReached { steps_executed: 3 })
        ),
        "terminal reason should be MaxStepsReached {{3}}, got {:?}",
        state.terminal_reason,
    );
}

#[tokio::test]
async fn run_records_tool_error_as_step_error() {
    // cdp_click is asked to fail by the stub: the MCP returns is_error
    // via a tool that does not exist. Instead we use NullMcp-style
    // behaviour via StaticMcp without the right tool; but StaticMcp
    // falls back to "ok". Simulate a tool error by having the stub
    // return a reply through has_tool=false path — the McpToolExecutor
    // surfaces the bail! as an error body.
    use super::super::super::test_stubs::NullMcp;
    let llm = ScriptedLlm::new(vec![
        llm_reply_tool("cdp_click", serde_json::json!({"uid": "d1"})),
        llm_reply_tool("agent_done", serde_json::json!({"summary": "stop"})),
    ]);
    let mcp = NullMcp;
    let runner = StateRunner::new("goal".to_string(), AgentConfig::default());
    let state = runner
        .run(
            &llm,
            &mcp,
            "goal".to_string(),
            crate::agent::trace_graph::AgentTraceGraph::new(),
            Vec::new(),
            None,
        )
        .await
        .expect("run ok");

    assert_eq!(state.steps.len(), 1, "the failing tool call is recorded");
    let step = &state.steps[0];
    assert!(matches!(
        step.outcome,
        crate::agent::types::StepOutcome::Error(_)
    ));
    assert!(state.completed);
}

/// Phase 3a port is complete — no deferred-work markers remain.
///
/// Tasks 3a.3 (VLM verification + approval gate),
/// 3a.4 (loop detection, destructive cap, terminal-reason mapping),
/// 3a.5 (workflow-graph emission), 3a.6 (CDP auto-connect + synthetic
/// focus_window skip), and 3a.6.5 (exactly-once boundary `StepRecord`
/// writes) have all landed. Each task removed its corresponding
/// `TODO(task-3a.N)` marker from `runner.rs` when its behaviour was
/// wired into `StateRunner::run`. This test pins the zero-marker
/// contract so a regression that re-introduces deferred work would
/// fail loudly.
///
/// Tasks 3a.7 (legacy test migration), 3a.8 (end-to-end test), and
/// 3a.9 (specta derives) do not touch `runner.rs` semantics — they
/// are testing / binding concerns, not deferred runtime hooks, so
/// they never planted markers here.
#[test]
fn runner_source_has_no_deferred_task_markers() {
    let runner_src = include_str!("../../runner/mod.rs");
    // Scan only the non-doc portion of the file — the doc-comment on
    // `parse_agent_turn` historically references `TODO(task-3a.2)` as
    // forward-looking narrative, which must not be interpreted as a
    // deferred-work pin. The canonical marker shape planted by
    // earlier tasks was a line-comment `// TODO(task-3a.N):`; only
    // match that exact form.
    let offenders: Vec<&str> = runner_src
        .lines()
        .filter(|line| line.trim_start().starts_with("// TODO(task-3a."))
        .collect();
    assert!(
        offenders.is_empty(),
        "expected zero `// TODO(task-3a.N):` markers in runner/mod.rs but found: {:?}",
        offenders,
    );
}
