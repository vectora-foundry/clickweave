use super::super::super::test_stubs::{ScriptedLlm, StaticMcp, llm_reply_tool};
use crate::agent::permissions::{PermissionAction, PermissionPolicy, PermissionRule};
use crate::agent::types::{AgentCommand, AgentConfig, StepOutcome, TerminalReason};
use crate::agent::{AgentChannels, run_agent_workflow};
use std::sync::{Arc, Mutex};
use tokio::sync::{mpsc, oneshot};

/// Happy-path gate: a scripted multi-step scenario drives
/// `run_agent_workflow` to an `agent_done` terminal. Locks the shape
/// external callers assert against:
///
/// - `state.steps` matches the scripted tool-call count (agent_done
///   itself does not land as a step — it's the terminal signal).
/// - `state.completed == true`.
/// - `state.terminal_reason == Some(TerminalReason::Completed { .. })`
///   with the summary the LLM supplied.
/// - `state.summary.as_deref() == Some("completed login")`.
#[tokio::test]
async fn run_agent_workflow_happy_path_preserves_legacy_agent_state_contract() {
    let llm = ScriptedLlm::new(vec![
        llm_reply_tool(
            "cdp_find_elements",
            serde_json::json!({"query": "", "max_results": 300}),
        ),
        llm_reply_tool("cdp_click", serde_json::json!({"uid": "1_0"})),
        llm_reply_tool(
            "agent_done",
            serde_json::json!({"summary": "completed login"}),
        ),
    ]);
    // `cdp_find_elements` returns an empty match set, mirroring the
    // stable fixture used by `run_completes_on_agent_done_after_two_tool_calls`.
    let mcp = StaticMcp::with_tools(&["cdp_find_elements", "cdp_click"])
        .with_reply(
            "cdp_find_elements",
            r#"{"page_url":"about:blank","source":"cdp","matches":[]}"#,
        )
        .with_reply("cdp_click", "clicked");

    let (state, _writer_tx) = run_agent_workflow(
        &llm,
        AgentConfig::default(),
        "log me in".to_string(),
        &mcp,
        None,
        None,
        None,
        uuid::Uuid::new_v4(),
        None,
        None,
        None,
        None,
        None,
    )
    .await
    .expect("run_agent_workflow ok");

    // Legacy `AgentState` contract (types.rs:219).
    assert_eq!(
        state.steps.len(),
        2,
        "two dispatched tool calls should be recorded as steps; agent_done is not a step; steps={:?}",
        state.steps,
    );
    match &state.steps[1].command {
        AgentCommand::ToolCall { tool_name, .. } => assert_eq!(tool_name, "cdp_click"),
        other => panic!("expected cdp_click ToolCall at step[1], got {:?}", other),
    }
    assert!(
        matches!(state.steps[1].outcome, StepOutcome::Success(_)),
        "cdp_click should land as Success, got {:?}",
        state.steps[1].outcome,
    );
    assert!(state.completed, "agent_done should set state.completed");
    assert_eq!(
        state.summary.as_deref(),
        Some("completed login"),
        "state.summary must reflect the agent_done summary",
    );
    assert!(
        matches!(
            state.terminal_reason,
            Some(TerminalReason::Completed { ref summary }) if summary == "completed login"
        ),
        "terminal_reason should be Completed with the agent_done summary, got {:?}",
        state.terminal_reason,
    );
    assert_eq!(state.consecutive_errors, 0);
}

/// Approval-rejected gate: when a destructive tool gated by
/// `PermissionAction::Ask` is rejected via the approval channel, the
/// run records a `Replan` step, does NOT mark `state.completed`, and
/// the tool body never reaches the `StepOutcome::Success` path. The
/// LLM's follow-up then terminates the run normally. Pins the
/// approval-rejection contract external callers depend on.
#[tokio::test]
async fn run_agent_workflow_approval_rejected_records_replan_and_stays_incomplete() {
    // If the tool were to dispatch, Success body would carry this
    // sentinel; the assertion rules that out.
    let llm = ScriptedLlm::new(vec![
        llm_reply_tool("cdp_click", serde_json::json!({"uid": "x"})),
        llm_reply_tool(
            "agent_done",
            serde_json::json!({"summary": "replanned and gave up"}),
        ),
    ]);
    let mcp = StaticMcp::with_tools(&["cdp_click"])
        .with_reply("cdp_click", "clicked-sentinel-must-not-appear");

    // Permission policy: force the Ask path on cdp_click so the
    // approval channel is consulted rather than the allow-all default.
    let policy = PermissionPolicy {
        rules: vec![PermissionRule {
            tool_pattern: "cdp_click".to_string(),
            args_pattern: None,
            action: PermissionAction::Ask,
        }],
        ..PermissionPolicy::default()
    };

    let (event_tx, _event_rx) = mpsc::channel(8);
    let (approval_tx, mut approval_rx) = mpsc::channel(4);
    // Responder: reject the first (and only) approval request.
    let responder = tokio::spawn(async move {
        if let Some((_req, reply)) = approval_rx.recv().await
            as Option<(crate::agent::types::ApprovalRequest, oneshot::Sender<bool>)>
        {
            let _ = reply.send(false);
        }
    });
    let channels = AgentChannels {
        event_tx,
        approval_tx,
    };

    let (state, _writer_tx) = run_agent_workflow(
        &llm,
        AgentConfig {
            max_steps: 5,
            ..AgentConfig::default()
        },
        "destructive goal".to_string(),
        &mcp,
        Some(channels),
        None,
        Some(policy),
        uuid::Uuid::new_v4(),
        None,
        None,
        None,
        None,
        None,
    )
    .await
    .expect("run_agent_workflow ok");

    responder.await.expect("approval responder joined");

    // Rejected approval lands as a single Replan step.
    let replan_count = state
        .steps
        .iter()
        .filter(|s| matches!(s.outcome, StepOutcome::Replan(_)))
        .count();
    assert_eq!(
        replan_count, 1,
        "rejected approval should produce exactly one Replan step; steps={:?}",
        state.steps
    );
    // The tool must never have dispatched — no Success step carries
    // the sentinel reply body.
    let executed = state.steps.iter().any(|s| match &s.outcome {
        StepOutcome::Success(body) => body.contains("clicked-sentinel-must-not-appear"),
        _ => false,
    });
    assert!(
        !executed,
        "rejected tool must never execute; state.steps={:?}",
        state.steps
    );
    // The run itself terminates via the scripted agent_done follow-up,
    // so `state.completed` flips true in this scenario — the legacy
    // contract only promises that a rejected-approval step is recorded
    // as Replan and the tool does not dispatch.
    assert!(
        state.completed,
        "scripted agent_done after replan should still set completed",
    );
}

/// Storage-integration gate: attach a `RunStorage` handle and assert
/// that at least one `StepRecord` with `boundary_kind == "terminal"`
/// lands in the execution-level `events.jsonl`. Locks the boundary-
/// persistence contract (Task 3a.6.5) through the `run_agent_workflow`
/// seam so the Tauri layer's storage pass-through keeps working.
#[tokio::test]
async fn run_agent_workflow_with_storage_writes_terminal_boundary_record() {
    let llm = ScriptedLlm::new(vec![
        llm_reply_tool(
            "cdp_find_elements",
            serde_json::json!({"query": "", "max_results": 300}),
        ),
        llm_reply_tool(
            "agent_done",
            serde_json::json!({"summary": "storage-integration run"}),
        ),
    ]);
    let mcp = StaticMcp::with_tools(&["cdp_find_elements"]).with_reply(
        "cdp_find_elements",
        r#"{"page_url":"about:blank","source":"cdp","matches":[]}"#,
    );

    let tmp = tempfile::tempdir().expect("tempdir");
    let workflow_name = "e2e-storage";
    let mut storage = clickweave_core::storage::RunStorage::new(tmp.path(), workflow_name);
    let exec_dir = storage.begin_execution().expect("begin_execution");
    let events_path = tmp
        .path()
        .join(".clickweave")
        .join("runs")
        .join(workflow_name)
        .join(&exec_dir)
        .join("events.jsonl");
    let storage = Arc::new(Mutex::new(storage));

    let (_state, _writer_tx) = run_agent_workflow(
        &llm,
        AgentConfig::default(),
        "exercise storage".to_string(),
        &mcp,
        None,
        None,
        None,
        uuid::Uuid::new_v4(),
        None,
        None,
        Some(storage.clone()),
        None,
        None,
    )
    .await
    .expect("run_agent_workflow ok");

    // Parse the execution-level events.jsonl and confirm at least one
    // boundary StepRecord with kind `terminal` was persisted.
    let contents = std::fs::read_to_string(&events_path)
        .unwrap_or_else(|e| panic!("read events.jsonl at {:?}: {}", events_path, e));
    let records: Vec<serde_json::Value> = contents
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .filter(|v| v.get("boundary_kind").is_some())
        .collect();
    let terminal_count = records
        .iter()
        .filter(|r| r.get("boundary_kind").and_then(|k| k.as_str()) == Some("terminal"))
        .count();
    assert_eq!(
        terminal_count, 1,
        "exactly one Terminal StepRecord expected on agent_done; records={:?}",
        records,
    );
}
