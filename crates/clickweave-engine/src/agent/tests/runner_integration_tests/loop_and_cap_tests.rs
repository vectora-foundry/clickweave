use super::super::super::test_stubs::{ScriptedLlm, StaticMcp, llm_reply_tool};
use crate::agent::runner::StateRunner;
use crate::agent::trace_graph::AgentTraceGraph;
use crate::agent::types::{AgentConfig, AgentEvent, RunnerOutput, TerminalReason};
use crate::executor::Mcp;
use tokio::sync::mpsc;

fn cfg_with_steps(steps: usize) -> AgentConfig {
    AgentConfig {
        max_steps: steps,
        ..AgentConfig::default()
    }
}

/// Build an MCP stub that advertises a single destructive tool flagged
/// via `destructiveHint = true`. `cdp_find_elements` is also advertised
/// so the runner's observe phase returns an empty but well-formed page
/// (no schema-drift warning).
fn destructive_mcp(tool_name: &str) -> StaticMcp {
    let tools = serde_json::json!([
        {
            "type": "function",
            "function": {
                "name": tool_name,
                "description": "stub destructive",
                "parameters": {"type": "object", "properties": {}},
                "annotations": {"destructiveHint": true, "readOnlyHint": false}
            }
        },
        {
            "type": "function",
            "function": {
                "name": "cdp_find_elements",
                "description": "stub",
                "parameters": {"type": "object", "properties": {}}
            }
        }
    ]);
    let stub = StaticMcp::with_tools(&[tool_name, "cdp_find_elements"]).with_reply(
        "cdp_find_elements",
        r#"{"page_url":"about:blank","source":"cdp","matches":[]}"#,
    );
    // Replace the advertised tool list so the destructive annotation is
    // visible to `build_annotations_index` / `maybe_halt_on_destructive_cap`.
    stub.with_tools_override(tools.as_array().unwrap().clone())
}

/// Two identical failing `cdp_click` calls halt on the second turn with
/// `TerminalReason::LoopDetected`. Exercises the live-path loop detector
/// ported from `AgentRunner::handle_step_outcome`.
#[tokio::test]
async fn two_identical_tool_errors_in_a_row_halt_with_loop_detected() {
    let llm = ScriptedLlm::new(vec![
        llm_reply_tool("cdp_click", serde_json::json!({"uid": "d1"})),
        llm_reply_tool("cdp_click", serde_json::json!({"uid": "d1"})),
        // Guard: if loop detection somehow didn't fire, fall through to
        // agent_done so the test doesn't hang.
        llm_reply_tool("agent_done", serde_json::json!({"summary": "x"})),
    ]);
    let mcp = StaticMcp::with_tools(&["cdp_click"]).with_error("cdp_click", "element not found");
    let tools = mcp.tools_as_openai();
    let runner = StateRunner::new("goal".to_string(), cfg_with_steps(5));

    let state = runner
        .run(
            &llm,
            &mcp,
            "goal".to_string(),
            AgentTraceGraph::new(),
            tools,
            None,
        )
        .await
        .expect("run ok");

    match state.terminal_reason {
        Some(TerminalReason::LoopDetected { tool_name, error }) => {
            assert_eq!(tool_name, "cdp_click");
            assert_eq!(error, "element not found");
        }
        other => panic!("expected LoopDetected, got {:?}", other),
    }
    assert_eq!(
        state.steps.len(),
        2,
        "loop detection fires on the second identical failure"
    );
}

/// Different arguments for the same tool must NOT trigger loop detection
/// — the LLM is exploring, not looping. After two different-uid
/// failures the run should hit `MaxErrorsReached` (cfg max is 2) rather
/// than `LoopDetected`, pinning that the args comparison is live.
#[tokio::test]
async fn different_args_do_not_trigger_loop_detection() {
    let mut cfg = cfg_with_steps(5);
    cfg.max_consecutive_errors = 2;
    let llm = ScriptedLlm::new(vec![
        llm_reply_tool("cdp_click", serde_json::json!({"uid": "d1"})),
        llm_reply_tool("cdp_click", serde_json::json!({"uid": "d2"})),
        llm_reply_tool("agent_done", serde_json::json!({"summary": "x"})),
    ]);
    let mcp = StaticMcp::with_tools(&["cdp_click"]).with_error("cdp_click", "element not found");
    let tools = mcp.tools_as_openai();
    let runner = StateRunner::new("goal".to_string(), cfg);

    let state = runner
        .run(
            &llm,
            &mcp,
            "goal".to_string(),
            AgentTraceGraph::new(),
            tools,
            None,
        )
        .await
        .expect("run ok");

    match state.terminal_reason {
        Some(TerminalReason::MaxErrorsReached { consecutive_errors }) => {
            assert_eq!(consecutive_errors, 2);
        }
        other => panic!(
            "different args should NOT trip LoopDetected; got {:?}",
            other
        ),
    }
}

/// Three successful destructive tools in a row halt the run with
/// `TerminalReason::ConsecutiveDestructiveCap` and emit the matching
/// `ConsecutiveDestructiveCapHit` event.
#[tokio::test]
async fn consecutive_destructive_cap_halts_run() {
    let mut cfg = cfg_with_steps(10);
    cfg.consecutive_destructive_cap = 3;
    let llm = ScriptedLlm::new(vec![
        llm_reply_tool("quit_app", serde_json::json!({"app_name": "A"})),
        llm_reply_tool("quit_app", serde_json::json!({"app_name": "B"})),
        llm_reply_tool("quit_app", serde_json::json!({"app_name": "C"})),
        // Guard: destructive cap should halt before this runs.
        llm_reply_tool("agent_done", serde_json::json!({"summary": "x"})),
    ]);
    let mcp = destructive_mcp("quit_app");
    let tools = mcp.tools_as_openai();

    let (event_tx, mut event_rx) = mpsc::channel::<RunnerOutput>(32);
    let runner = StateRunner::new("goal".to_string(), cfg).with_events(event_tx);
    let state = runner
        .run(
            &llm,
            &mcp,
            "goal".to_string(),
            AgentTraceGraph::new(),
            tools,
            None,
        )
        .await
        .expect("run ok");

    match state.terminal_reason {
        Some(TerminalReason::ConsecutiveDestructiveCap {
            cap,
            recent_tool_names,
        }) => {
            assert_eq!(cap, 3);
            assert_eq!(recent_tool_names, vec!["quit_app", "quit_app", "quit_app"]);
        }
        other => panic!("expected ConsecutiveDestructiveCap, got {:?}", other),
    }

    let mut saw_cap_event = false;
    while let Ok(ev) = event_rx.try_recv() {
        let Some(ev) = ev.into_event() else {
            continue;
        };
        if matches!(ev, AgentEvent::ConsecutiveDestructiveCapHit { .. }) {
            saw_cap_event = true;
            break;
        }
    }
    assert!(
        saw_cap_event,
        "ConsecutiveDestructiveCapHit event must be emitted"
    );
}

/// A non-destructive (read-only) success in between destructive calls
/// resets the streak. With cap=3, the sequence destr/destr/read/destr
/// finishes with an agent_done rather than hitting the cap.
#[tokio::test]
async fn non_destructive_success_resets_destructive_streak() {
    let mut cfg = cfg_with_steps(10);
    cfg.consecutive_destructive_cap = 3;
    // Advertise both a destructive tool and a read-only probe so the
    // annotations index sees both hints.
    let tools = serde_json::json!([
        {
            "type": "function",
            "function": {
                "name": "quit_app",
                "description": "destructive",
                "parameters": {"type": "object", "properties": {}},
                "annotations": {"destructiveHint": true}
            }
        },
        {
            "type": "function",
            "function": {
                "name": "probe_app",
                "description": "read-only",
                "parameters": {"type": "object", "properties": {}},
                "annotations": {"readOnlyHint": true}
            }
        },
        {
            "type": "function",
            "function": {
                "name": "cdp_find_elements",
                "description": "stub",
                "parameters": {"type": "object", "properties": {}}
            }
        }
    ]);
    let mcp = StaticMcp::with_tools(&["quit_app", "probe_app", "cdp_find_elements"])
        .with_reply(
            "cdp_find_elements",
            r#"{"page_url":"about:blank","source":"cdp","matches":[]}"#,
        )
        .with_reply("quit_app", "quit-ok")
        .with_reply("probe_app", "{}")
        .with_tools_override(tools.as_array().unwrap().clone());

    let llm = ScriptedLlm::new(vec![
        llm_reply_tool("quit_app", serde_json::json!({"app_name": "A"})),
        llm_reply_tool("quit_app", serde_json::json!({"app_name": "B"})),
        llm_reply_tool("probe_app", serde_json::json!({"app_name": "A"})),
        llm_reply_tool("quit_app", serde_json::json!({"app_name": "C"})),
        llm_reply_tool("quit_app", serde_json::json!({"app_name": "D"})),
        llm_reply_tool("agent_done", serde_json::json!({"summary": "done"})),
    ]);
    let advertised = mcp.tools_as_openai();
    let runner = StateRunner::new("goal".to_string(), cfg);
    let state = runner
        .run(
            &llm,
            &mcp,
            "goal".to_string(),
            AgentTraceGraph::new(),
            advertised,
            None,
        )
        .await
        .expect("run ok");

    // Run completed via agent_done, not destructive cap.
    assert!(
        state.completed,
        "run should have completed, not been capped"
    );
    assert!(matches!(
        state.terminal_reason,
        Some(TerminalReason::Completed { .. })
    ));
}

/// `consecutive_destructive_cap == 0` disables the feature entirely:
/// many destructive tools in a row run without halting.
#[tokio::test]
async fn cap_zero_disables_destructive_feature() {
    let mut cfg = cfg_with_steps(20);
    cfg.consecutive_destructive_cap = 0;
    let llm = ScriptedLlm::new(vec![
        llm_reply_tool("quit_app", serde_json::json!({"app_name": "A"})),
        llm_reply_tool("quit_app", serde_json::json!({"app_name": "B"})),
        llm_reply_tool("quit_app", serde_json::json!({"app_name": "C"})),
        llm_reply_tool("quit_app", serde_json::json!({"app_name": "D"})),
        llm_reply_tool("quit_app", serde_json::json!({"app_name": "E"})),
        llm_reply_tool("agent_done", serde_json::json!({"summary": "done"})),
    ]);
    let mcp = destructive_mcp("quit_app").with_reply("quit_app", "quit-ok");
    let tools = mcp.tools_as_openai();
    let runner = StateRunner::new("goal".to_string(), cfg);
    let state = runner
        .run(
            &llm,
            &mcp,
            "goal".to_string(),
            AgentTraceGraph::new(),
            tools,
            None,
        )
        .await
        .expect("run ok");

    assert!(state.completed, "cap=0 should disable the halt");
    assert!(matches!(
        state.terminal_reason,
        Some(TerminalReason::Completed { .. })
    ));
}

/// `max_consecutive_errors = 2` + two different-args failures halts
/// with `TerminalReason::MaxErrorsReached`.
#[tokio::test]
async fn max_errors_reached_sets_correct_terminal_reason() {
    let mut cfg = cfg_with_steps(10);
    cfg.max_consecutive_errors = 2;
    let llm = ScriptedLlm::new(vec![
        llm_reply_tool("cdp_click", serde_json::json!({"uid": "d1"})),
        llm_reply_tool("cdp_click", serde_json::json!({"uid": "d2"})),
        llm_reply_tool("agent_done", serde_json::json!({"summary": "x"})),
    ]);
    let mcp = StaticMcp::with_tools(&["cdp_click"]).with_error("cdp_click", "elem not found");
    let tools = mcp.tools_as_openai();
    let runner = StateRunner::new("goal".to_string(), cfg);
    let state = runner
        .run(
            &llm,
            &mcp,
            "goal".to_string(),
            AgentTraceGraph::new(),
            tools,
            None,
        )
        .await
        .expect("run ok");

    match state.terminal_reason {
        Some(TerminalReason::MaxErrorsReached { consecutive_errors }) => {
            assert_eq!(consecutive_errors, 2);
        }
        other => panic!("expected MaxErrorsReached, got {:?}", other),
    }
}
