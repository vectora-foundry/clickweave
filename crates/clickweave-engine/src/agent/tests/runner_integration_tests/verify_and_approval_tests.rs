use std::sync::Arc;

use crate::agent::trace_graph::AgentTraceGraph;
use clickweave_llm::DynChatBackend;
use tokio::sync::{mpsc, oneshot};

use super::super::super::test_stubs::{NoVlm, ScriptedLlm, StaticMcp, YesVlm, llm_reply_tool};
use crate::agent::runner::StateRunner;
use crate::agent::types::{
    AgentConfig, AgentEvent, ApprovalRequest, RunnerOutput, StepOutcome, TerminalReason,
};
use crate::executor::Mcp;

/// 1x1 transparent PNG, shared with `executor::screenshot` tests — the
/// smallest payload that round-trips through
/// `prepare_base64_image_for_vlm` without an external crate dependency.
const TINY_PNG_BASE64: &str =
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNkYAAAAAYAAjCB0C8AAAAASUVORK5CYII=";

/// MCP fixture for completion-verification tests: advertises
/// `take_screenshot` and returns the tiny PNG as image content so the
/// VLM path has a payload to prep.
fn mcp_with_screenshot() -> StaticMcp {
    StaticMcp::with_tools(&["take_screenshot"]).with_image_reply(
        "take_screenshot",
        TINY_PNG_BASE64,
        "image/png",
    )
}

fn cfg_with_steps(max_steps: usize) -> AgentConfig {
    AgentConfig {
        max_steps,
        ..AgentConfig::default()
    }
}

// -----------------------------------------------------------------
// VLM verification
// -----------------------------------------------------------------

/// VLM agrees (YES) → run completes normally.
#[tokio::test]
async fn vlm_yes_verdict_lets_agent_done_complete() {
    let vlm: Arc<dyn DynChatBackend> = Arc::new(YesVlm);
    let llm = ScriptedLlm::new(vec![llm_reply_tool(
        "agent_done",
        serde_json::json!({"summary": "goal achieved"}),
    )]);
    let mcp = mcp_with_screenshot();
    let tools = mcp.tools_as_openai();
    let runner = StateRunner::new("goal".to_string(), cfg_with_steps(3)).with_vision(vlm);

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

    assert!(state.completed, "YES verdict should allow completion");
    assert!(
        matches!(
            state.terminal_reason,
            Some(TerminalReason::Completed { .. })
        ),
        "expected Completed, got {:?}",
        state.terminal_reason,
    );
}

/// VLM disagrees (NO) → run halts with `CompletionDisagreement`.
/// Also asserts that the `CompletionDisagreement` event reaches the
/// event channel.
#[tokio::test]
async fn vlm_no_verdict_halts_with_completion_disagreement() {
    let vlm: Arc<dyn DynChatBackend> = Arc::new(NoVlm);
    let llm = ScriptedLlm::new(vec![llm_reply_tool(
        "agent_done",
        serde_json::json!({"summary": "claimed done"}),
    )]);
    let mcp = mcp_with_screenshot();
    let tools = mcp.tools_as_openai();

    let (event_tx, mut event_rx) = mpsc::channel::<RunnerOutput>(8);
    let runner = StateRunner::new("goal".to_string(), cfg_with_steps(3))
        .with_vision(vlm)
        .with_events(event_tx);

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

    assert!(!state.completed, "NO verdict must not mark completed");
    match state.terminal_reason {
        Some(TerminalReason::CompletionDisagreement {
            ref agent_summary, ..
        }) => {
            assert_eq!(agent_summary, "claimed done");
        }
        other => panic!("expected CompletionDisagreement, got {:?}", other),
    }

    // Drain events and look for the CompletionDisagreement one.
    let mut saw_disagreement = false;
    while let Ok(ev) = event_rx.try_recv() {
        let Some(ev) = ev.into_event() else {
            continue;
        };
        if matches!(ev, AgentEvent::CompletionDisagreement { .. }) {
            saw_disagreement = true;
        }
    }
    assert!(
        saw_disagreement,
        "CompletionDisagreement event must be emitted on event_tx"
    );
}

/// When no VLM backend is configured, `agent_done` completes normally
/// — the verification step is a no-op.
#[tokio::test]
async fn no_vision_backend_lets_agent_done_complete_unchecked() {
    let llm = ScriptedLlm::new(vec![llm_reply_tool(
        "agent_done",
        serde_json::json!({"summary": "ok"}),
    )]);
    let mcp = mcp_with_screenshot();
    let tools = mcp.tools_as_openai();
    let runner = StateRunner::new("goal".to_string(), cfg_with_steps(3));

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

    assert!(state.completed);
    assert!(matches!(
        state.terminal_reason,
        Some(TerminalReason::Completed { .. })
    ));
}

/// Verification artifacts (PNG + JSON) must be persisted to the
/// configured dir for every VLM call.
#[tokio::test]
async fn verify_completion_persists_artifacts_when_dir_set() {
    let vlm: Arc<dyn DynChatBackend> = Arc::new(YesVlm);
    let dir = tempfile::tempdir().expect("tempdir");
    let llm = ScriptedLlm::new(vec![llm_reply_tool(
        "agent_done",
        serde_json::json!({"summary": "done"}),
    )]);
    let mcp = mcp_with_screenshot();
    let tools = mcp.tools_as_openai();
    let runner = StateRunner::new("goal".to_string(), cfg_with_steps(3))
        .with_vision(vlm)
        .with_verification_artifacts_dir(dir.path().to_path_buf());

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
    assert!(state.completed);

    let entries: Vec<_> = std::fs::read_dir(dir.path())
        .expect("read_dir")
        .filter_map(Result::ok)
        .collect();
    assert!(
        !entries.is_empty(),
        "verification artifacts must be persisted"
    );
    // At least one PNG and one JSON should land in the dir.
    let has_png = entries
        .iter()
        .any(|e| e.file_name().to_string_lossy().ends_with(".png"));
    let has_json = entries
        .iter()
        .any(|e| e.file_name().to_string_lossy().ends_with(".json"));
    assert!(has_png, "verification PNG must be written");
    assert!(has_json, "verification JSON must be written");
}

// -----------------------------------------------------------------
// Approval gate on the live dispatch path
// -----------------------------------------------------------------

/// Rejected approval on a live tool call → the tool is not executed
/// and a Replan step is recorded. The run then loops back to the
/// LLM, which emits `agent_done` to terminate.
#[tokio::test]
async fn approval_rejected_replans_without_executing_tool() {
    // cdp_click would be dispatched if approval approved. The MCP
    // stub is configured with a sentinel reply; if the tool runs, the
    // step outcome would be Success("clicked-sentinel") — the
    // assertion rules that out.
    let llm = ScriptedLlm::new(vec![
        llm_reply_tool("cdp_click", serde_json::json!({"uid": "x"})),
        llm_reply_tool("agent_done", serde_json::json!({"summary": "end"})),
    ]);
    let mcp = StaticMcp::with_tools(&["cdp_click"]).with_reply("cdp_click", "clicked-sentinel");
    let tools = mcp.tools_as_openai();

    let (approval_tx, mut approval_rx) =
        mpsc::channel::<(ApprovalRequest, oneshot::Sender<bool>)>(4);
    let responder = tokio::spawn(async move {
        if let Some((_req, reply)) = approval_rx.recv().await {
            let _ = reply.send(false);
        }
    });

    let runner = StateRunner::new("goal".to_string(), cfg_with_steps(5)).with_approval(approval_tx);
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

    responder.await.unwrap();

    // Exactly one step should be a Replan for the rejected cdp_click;
    // no step should carry the Success sentinel body, confirming the
    // tool never dispatched.
    let replan_count = state
        .steps
        .iter()
        .filter(|s| matches!(s.outcome, StepOutcome::Replan(_)))
        .count();
    assert_eq!(
        replan_count, 1,
        "rejected approval should produce exactly one Replan step"
    );
    let executed = state.steps.iter().any(|s| match &s.outcome {
        StepOutcome::Success(body) => body.contains("clicked-sentinel"),
        _ => false,
    });
    assert!(!executed, "rejected tool must never execute");
}

/// Approval channel gone → terminal `ApprovalUnavailable`, the LLM
/// is never consulted again after the gate failure.
#[tokio::test]
async fn approval_unavailable_halts_run() {
    let llm = ScriptedLlm::new(vec![llm_reply_tool(
        "cdp_click",
        serde_json::json!({"uid": "x"}),
    )]);
    let mcp = StaticMcp::with_tools(&["cdp_click"]).with_reply("cdp_click", "clicked");
    let tools = mcp.tools_as_openai();

    // Drop the receiver before the runner starts so the first send
    // fails deterministically.
    let (approval_tx, approval_rx) = mpsc::channel::<(ApprovalRequest, oneshot::Sender<bool>)>(1);
    drop(approval_rx);

    let runner = StateRunner::new("goal".to_string(), cfg_with_steps(5)).with_approval(approval_tx);
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

    assert!(matches!(
        state.terminal_reason,
        Some(TerminalReason::ApprovalUnavailable)
    ));
}

/// Approved approval on a live call → the tool IS executed. Pins the
/// happy-path pass-through so regressions in the gate wiring surface.
#[tokio::test]
async fn approved_live_approval_lets_tool_execute() {
    let llm = ScriptedLlm::new(vec![
        llm_reply_tool("cdp_click", serde_json::json!({"uid": "x"})),
        llm_reply_tool("agent_done", serde_json::json!({"summary": "done"})),
    ]);
    let mcp = StaticMcp::with_tools(&["cdp_click"]).with_reply("cdp_click", "clicked-ok");
    let tools = mcp.tools_as_openai();

    let (approval_tx, mut approval_rx) =
        mpsc::channel::<(ApprovalRequest, oneshot::Sender<bool>)>(4);
    let responder = tokio::spawn(async move {
        if let Some((_req, reply)) = approval_rx.recv().await {
            let _ = reply.send(true);
        }
    });

    let runner = StateRunner::new("goal".to_string(), cfg_with_steps(5)).with_approval(approval_tx);
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
    responder.await.unwrap();

    let executed = state.steps.iter().any(|s| match &s.outcome {
        StepOutcome::Success(body) => body.contains("clicked-ok"),
        _ => false,
    });
    assert!(executed, "approved tool should dispatch and succeed");
    assert!(state.completed);
}
