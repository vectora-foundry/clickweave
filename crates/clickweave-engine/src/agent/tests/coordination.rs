//! Cross-task coordination integration tests.
//!
//! Each test plays the role the Tauri forwarders play in production so
//! the engine's response to realistic cross-task event sequences is
//! covered at the `AgentChannels` contract boundary.

use super::{CapturingMockAgent, MockAgent, MockMcp};
use crate::agent::loop_runner::AgentRunner;
use crate::agent::types::*;
use crate::executor::Mcp;
use clickweave_llm::Message;
use clickweave_mcp::{ToolCallResult, ToolContent};
use serde_json::Value;
use std::sync::{Arc, Mutex};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;

// ---------------------------------------------------------------------------
// Shared test helpers
// ---------------------------------------------------------------------------

fn cdp_button_page(url: &str) -> ToolCallResult {
    ToolCallResult {
        content: vec![ToolContent::Text {
            text: serde_json::json!({
                "page_url": url,
                "source": "cdp",
                "matches": [{
                    "uid": "1_0",
                    "role": "button",
                    "label": "Submit",
                    "tag": "button"
                }]
            })
            .to_string(),
        }],
        is_error: None,
    }
}

fn cdp_empty_page() -> ToolCallResult {
    ToolCallResult {
        content: vec![ToolContent::Text {
            text: serde_json::json!({
                "page_url": "",
                "source": "cdp",
                "matches": []
            })
            .to_string(),
        }],
        is_error: None,
    }
}

fn text_result(text: &str) -> ToolCallResult {
    ToolCallResult {
        content: vec![ToolContent::Text {
            text: text.to_string(),
        }],
        is_error: None,
    }
}

type ApprovalChannelTx = mpsc::Sender<(ApprovalRequest, oneshot::Sender<bool>)>;
type ApprovalChannelRx = mpsc::Receiver<(ApprovalRequest, oneshot::Sender<bool>)>;

fn approval_channel() -> (ApprovalChannelTx, ApprovalChannelRx) {
    mpsc::channel(1)
}

/// Drain every pending approval request and answer `true`.
fn spawn_auto_approver(mut rx: ApprovalChannelRx) -> JoinHandle<()> {
    tokio::spawn(async move {
        while let Some((_req, resp_tx)) = rx.recv().await {
            let _ = resp_tx.send(true);
        }
    })
}

/// Auto-approver that records the tool name of each request seen.
fn spawn_recording_approver(
    mut rx: ApprovalChannelRx,
) -> (JoinHandle<()>, Arc<Mutex<Vec<String>>>) {
    let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let seen_clone = seen.clone();
    let handle = tokio::spawn(async move {
        while let Some((req, resp_tx)) = rx.recv().await {
            seen_clone.lock().unwrap().push(req.tool_name.clone());
            let _ = resp_tx.send(true);
        }
    });
    (handle, seen)
}

fn drain_events(rx: &mut mpsc::Receiver<AgentEvent>) -> Vec<AgentEvent> {
    let mut out = Vec::new();
    while let Ok(ev) = rx.try_recv() {
        out.push(ev);
    }
    out
}

// ---------------------------------------------------------------------------
// Scenario 1: Stop during approval wait
// ---------------------------------------------------------------------------

#[tokio::test]
async fn stop_during_approval_wait_sends_rejection_not_channel_drop() {
    let agent_llm = MockAgent::new(vec![
        MockAgent::tool_call_response("click", r#"{"x": 10, "y": 20}"#, "call_0"),
        MockAgent::done_response("Found another way after cancel"),
    ]);
    let mcp = MockMcp::with_click_tool();

    let config = AgentConfig {
        max_steps: 5,
        build_workflow: false,
        use_cache: false,
        ..Default::default()
    };

    let (event_tx, _event_rx) = mpsc::channel::<AgentEvent>(64);
    let (approval_tx, mut approval_rx) = approval_channel();

    // Receive the approval request, then send `false` on the oneshot
    // instead of dropping it — the engine must observe an explicit
    // rejection (Replan) rather than ApprovalUnavailable.
    tokio::spawn(async move {
        if let Some((_req, resp_tx)) = approval_rx.recv().await {
            let _ = resp_tx.send(false);
        }
    });

    let mut runner = AgentRunner::new(&agent_llm, config)
        .with_events(event_tx)
        .with_approval(approval_tx);
    let workflow = clickweave_core::Workflow::new("Stop-during-approval");
    let mcp_tools = mcp.tools_as_openai();

    let state = runner
        .run(
            "Click it".to_string(),
            workflow,
            &mcp,
            None,
            mcp_tools,
            None,
            &[],
        )
        .await
        .unwrap();

    assert!(
        matches!(state.steps[0].outcome, StepOutcome::Replan(_)),
        "Expected Replan after force_stop rejection, got {:?}",
        state.steps[0].outcome,
    );
    assert!(
        !matches!(
            state.terminal_reason,
            Some(TerminalReason::ApprovalUnavailable)
        ),
        "force_stop must not surface as ApprovalUnavailable, got {:?}",
        state.terminal_reason,
    );
}

// ---------------------------------------------------------------------------
// Scenario 2: Back-to-back runs do not leak events between channels
// ---------------------------------------------------------------------------

#[tokio::test]
async fn buffered_events_do_not_leak_between_runs() {
    let llm_a = MockAgent::new(vec![
        MockAgent::tool_call_response("click", r#"{"x": 1, "y": 2}"#, "call_a0"),
        MockAgent::done_response("run A done"),
    ]);
    let mcp_a = MockMcp::with_click_tool();

    let (event_tx_a, mut event_rx_a) = mpsc::channel::<AgentEvent>(64);
    let (approval_tx_a, approval_rx_a) = approval_channel();
    let approver_a = spawn_auto_approver(approval_rx_a);

    let mut runner_a = AgentRunner::new(&llm_a, AgentConfig::default())
        .with_events(event_tx_a)
        .with_approval(approval_tx_a);
    runner_a
        .run(
            "goal A".to_string(),
            clickweave_core::Workflow::new("A"),
            &mcp_a,
            None,
            mcp_a.tools_as_openai(),
            None,
            &[],
        )
        .await
        .unwrap();
    approver_a.abort();

    let run_a_events = drain_events(&mut event_rx_a);
    assert!(!run_a_events.is_empty(), "Run A must have produced events");

    // Run B uses fresh channels — A's channel is already dropped, so
    // B's receiver is physically isolated from A's history.
    let llm_b = MockAgent::new(vec![MockAgent::done_response("run B done")]);
    let mcp_b = MockMcp::with_click_tool();

    let (event_tx_b, mut event_rx_b) = mpsc::channel::<AgentEvent>(64);
    let (approval_tx_b, _approval_rx_b) = approval_channel();
    let mut runner_b = AgentRunner::new(&llm_b, AgentConfig::default())
        .with_events(event_tx_b)
        .with_approval(approval_tx_b);
    runner_b
        .run(
            "goal B".to_string(),
            clickweave_core::Workflow::new("B"),
            &mcp_b,
            None,
            mcp_b.tools_as_openai(),
            None,
            &[],
        )
        .await
        .unwrap();

    let run_b_events = drain_events(&mut event_rx_b);

    // B's GoalComplete must carry B's summary. This assertion also
    // implicitly verifies A's completion reached A's channel and not B's.
    let b_summary = run_b_events.iter().find_map(|e| match e {
        AgentEvent::GoalComplete { summary } => Some(summary.clone()),
        _ => None,
    });
    assert_eq!(b_summary.as_deref(), Some("run B done"));
    assert!(
        run_a_events
            .iter()
            .any(|e| matches!(e, AgentEvent::GoalComplete { .. })),
        "Run A should have emitted GoalComplete on its own channel"
    );
}

// ---------------------------------------------------------------------------
// Scenario 3: Cached launch_app replay runs approval and post-tool hooks
// ---------------------------------------------------------------------------

#[tokio::test]
async fn cached_launch_app_replay_requests_approval_and_runs_post_tool_hook() {
    let mut cache = AgentCache::default();
    let elements = vec![clickweave_core::cdp::CdpFindElementMatch {
        uid: "1_0".to_string(),
        role: "button".to_string(),
        label: "Submit".to_string(),
        tag: "button".to_string(),
        disabled: false,
        parent_role: None,
        parent_name: None,
    }];
    cache.store(
        "launch Calculator",
        &elements,
        "launch_app".to_string(),
        serde_json::json!({"app_name": "Calculator"}),
    );

    let agent_llm = MockAgent::new(vec![MockAgent::done_response("Launched via cache")]);

    // probe_app returns "NativeApp" so auto_connect_cdp short-circuits
    // before hitting the slow quit/relaunch path.
    let tools = vec![
        serde_json::json!({
            "type": "function",
            "function": {
                "name": "launch_app",
                "description": "Launch an app",
                "parameters": {
                    "type": "object",
                    "properties": {"app_name": {"type": "string"}},
                    "required": ["app_name"]
                }
            }
        }),
        serde_json::json!({
            "type": "function",
            "function": {
                "name": "probe_app",
                "description": "Probe an app",
                "parameters": {
                    "type": "object",
                    "properties": {"app_name": {"type": "string"}},
                    "required": ["app_name"]
                }
            }
        }),
        serde_json::json!({
            "type": "function",
            "function": {
                "name": "cdp_connect",
                "description": "Connect to CDP",
                "parameters": {
                    "type": "object",
                    "properties": {"port": {"type": "number"}},
                    "required": ["port"]
                }
            }
        }),
    ];

    let results = vec![
        cdp_button_page("https://example.com"),
        text_result("Launched"),
        text_result("NativeApp"),
        cdp_button_page("https://example.com/next"),
    ];
    let mcp = MockMcp::new(results, tools);

    let config = AgentConfig {
        max_steps: 5,
        build_workflow: false,
        use_cache: true,
        ..Default::default()
    };

    let (event_tx, mut event_rx) = mpsc::channel::<AgentEvent>(64);
    let (approval_tx, approval_rx) = approval_channel();
    let (approver, seen_approvals) = spawn_recording_approver(approval_rx);

    let mut runner = AgentRunner::with_cache(&agent_llm, config, cache)
        .with_events(event_tx)
        .with_approval(approval_tx);
    let workflow = clickweave_core::Workflow::new("Cache-replay launch");
    let mcp_tools = mcp.tools_as_openai();

    let state = runner
        .run(
            "launch Calculator".to_string(),
            workflow,
            &mcp,
            None,
            mcp_tools,
            None,
            &[],
        )
        .await
        .unwrap();
    approver.abort();

    let approvals = seen_approvals.lock().unwrap();
    assert!(
        approvals.iter().any(|name| name == "launch_app"),
        "Cached launch_app replay must re-request approval, saw {:?}",
        *approvals,
    );

    let events = drain_events(&mut event_rx);
    let saw_probe_sub_action = events.iter().any(|ev| {
        matches!(
            ev,
            AgentEvent::SubAction { tool_name, .. } if tool_name == "probe_app"
        )
    });
    assert!(
        saw_probe_sub_action,
        "Cached launch_app replay must run the post-tool hook (probe_app SubAction)"
    );

    assert!(state.completed);
}

// ---------------------------------------------------------------------------
// Scenario 4: Native / no-CDP path does not read or write the cache
// ---------------------------------------------------------------------------

#[tokio::test]
async fn empty_elements_skips_cache_read_and_write() {
    // Pre-seed the cache with an entry keyed on *empty* elements. If the
    // engine ever looked up using empty elements, this entry would hit
    // and trigger a replay.
    let mut cache = AgentCache::default();
    cache.store(
        "click somewhere",
        &[],
        "click".to_string(),
        serde_json::json!({"x": 99, "y": 99}),
    );

    let capturing: Arc<Mutex<Vec<Vec<Message>>>> = Arc::new(Mutex::new(Vec::new()));
    let llm = CapturingMockAgent::new(
        vec![
            MockAgent::tool_call_response("click", r#"{"x": 5, "y": 5}"#, "call_native_0"),
            MockAgent::done_response("Done without cache"),
        ],
        capturing.clone(),
    );

    let tools = vec![serde_json::json!({
        "type": "function",
        "function": {
            "name": "click",
            "description": "Click",
            "parameters": {
                "type": "object",
                "properties": {"x": {"type": "number"}, "y": {"type": "number"}},
                "required": ["x", "y"]
            }
        }
    })];
    let results = vec![cdp_empty_page(), text_result("clicked"), cdp_empty_page()];
    let mcp = MockMcp::new(results, tools);

    let config = AgentConfig {
        max_steps: 5,
        build_workflow: false,
        use_cache: true,
        ..Default::default()
    };

    let (event_tx, _event_rx) = mpsc::channel::<AgentEvent>(64);
    let (approval_tx, approval_rx) = approval_channel();
    let approver = spawn_auto_approver(approval_rx);

    let mut runner = AgentRunner::with_cache(&llm, config, cache)
        .with_events(event_tx)
        .with_approval(approval_tx);
    let workflow = clickweave_core::Workflow::new("Native-no-cache");
    let mcp_tools = mcp.tools_as_openai();
    let state = runner
        .run(
            "click somewhere".to_string(),
            workflow,
            &mcp,
            None,
            mcp_tools,
            None,
            &[],
        )
        .await
        .unwrap();
    approver.abort();

    // If cache replay had fired, the LLM would be called once (done only);
    // with replay skipped, the LLM is called for both click and done.
    let calls = capturing.lock().unwrap();
    assert_eq!(
        calls.len(),
        2,
        "LLM should be called twice (click + done) — empty elements must skip cache lookup"
    );

    // Read-side guard: the first step's tool_call_id comes from the LLM,
    // not a synthetic "cache-0" id that would indicate a replay.
    match &state.steps[0].command {
        AgentCommand::ToolCall { tool_call_id, .. } => {
            assert_eq!(
                tool_call_id, "call_native_0",
                "Empty-elements observation must not trigger cache replay — \
                 first step should come from the LLM, not the cache"
            );
        }
        other => panic!("Expected first step to be a ToolCall, got {:?}", other),
    }

    // Write-side guard: `store()` bumps hit_count. If the click had
    // written into the cache on success, the seed's hit_count would be 2.
    drop(state);
    let final_cache = runner.into_cache();
    let seeded = final_cache
        .lookup("click somewhere", &[])
        .expect("pre-seeded entry should still exist");
    assert_eq!(
        seeded.hit_count, 1,
        "empty-elements click must not touch the cache — hit_count should stay at 1"
    );
}

// ---------------------------------------------------------------------------
// Scenario 5: Workflow-mapping miss emits Warning, not Error; run continues
// ---------------------------------------------------------------------------

#[tokio::test]
async fn workflow_mapping_miss_emits_warning_and_run_continues() {
    let agent_llm = MockAgent::new(vec![
        MockAgent::tool_call_response("weird_tool", r#"{"foo": "bar"}"#, "call_weird"),
        MockAgent::done_response("Completed after mapping miss"),
    ]);

    // No tools advertised — any LLM tool name is unknown at workflow-node
    // construction time.
    let results = vec![
        cdp_button_page("https://example.com"),
        text_result("executed"),
        cdp_button_page("https://example.com/next"),
    ];
    let mcp = MockMcp::new(results, Vec::<Value>::new());

    let config = AgentConfig {
        max_steps: 5,
        build_workflow: true,
        use_cache: false,
        ..Default::default()
    };

    let (event_tx, mut event_rx) = mpsc::channel::<AgentEvent>(64);
    let (approval_tx, approval_rx) = approval_channel();
    let approver = spawn_auto_approver(approval_rx);

    let mut runner = AgentRunner::new(&agent_llm, config)
        .with_events(event_tx)
        .with_approval(approval_tx);
    let workflow = clickweave_core::Workflow::new("Mapping-miss");
    let mcp_tools = mcp.tools_as_openai();
    let state = runner
        .run(
            "trigger mapping miss".to_string(),
            workflow,
            &mcp,
            None,
            mcp_tools,
            None,
            &[],
        )
        .await
        .unwrap();
    approver.abort();

    assert!(
        state.completed,
        "Mapping miss must not abort the loop, state={:?}",
        state.terminal_reason,
    );

    let events = drain_events(&mut event_rx);
    assert!(
        events
            .iter()
            .any(|ev| matches!(ev, AgentEvent::Warning { .. })),
        "Mapping miss must emit AgentEvent::Warning"
    );
    assert!(
        !events
            .iter()
            .any(|ev| matches!(ev, AgentEvent::Error { .. })),
        "Mapping miss must not emit AgentEvent::Error (the run continues)"
    );
}
