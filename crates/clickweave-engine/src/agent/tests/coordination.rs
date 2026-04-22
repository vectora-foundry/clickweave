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
// Scenario 3: Cached state-transition tools (launch_app / focus_window) must
// NOT replay — replaying them re-fires an app/window transition against
// stale CDP elements, producing duplicate step_completed events and
// duplicate workflow nodes. The cache read-side filter falls through to
// the LLM in this case.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn cached_launch_app_is_not_replayed_and_falls_through_to_llm() {
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
        !approvals.iter().any(|name| name == "launch_app"),
        "Cached launch_app must NOT replay (no approval prompt expected), saw {:?}",
        *approvals,
    );

    let events = drain_events(&mut event_rx);
    let saw_launch_step = events.iter().any(|ev| {
        matches!(
            ev,
            AgentEvent::StepCompleted { tool_name, .. } if tool_name == "launch_app"
        )
    });
    assert!(
        !saw_launch_step,
        "Cached launch_app must NOT replay — no StepCompleted for launch_app expected"
    );

    // The LLM was given a chance to decide and emitted `done` — the run
    // should complete through that path, not through cache replay.
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

// ---------------------------------------------------------------------------
// Scenario 6: Runner-side focus_window guard for AX dispatch targets
//
// When the MCP server exposes the full macOS AX dispatch toolset AND a
// prior `launch_app` response classified the target as `"Native"`, a
// subsequent `focus_window` against that same app must be suppressed —
// AX dispatch is focus-preserving and the real tool would only steal
// foreground from the user. Prompt-only guidance (see `prompt.rs`) is
// non-deterministic on local models like gemma; the guard enforces the
// invariant regardless of what the LLM chose.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn focus_window_after_native_launch_is_suppressed_with_ax_toolset() {
    let agent_llm = MockAgent::new(vec![
        // Step 0: agent launches Calculator. MCP response carries
        // `{"kind": "Native"}` so the runner records the kind.
        MockAgent::tool_call_response("launch_app", r#"{"app_name": "Calculator"}"#, "call_launch"),
        // Step 1: agent — despite the prompt — issues focus_window. This
        // is the focus-stealing call the guard must suppress.
        MockAgent::tool_call_response(
            "focus_window",
            r#"{"app_name": "Calculator"}"#,
            "call_focus",
        ),
        // Step 2: wrap up.
        MockAgent::done_response("Reached Calculator via AX"),
    ]);

    // MCP advertises the full macOS AX dispatch toolset plus the
    // state-transition tools the agent will invoke. Crucially NO
    // `cdp_connect` — this is a native-app scenario; auto_connect_cdp
    // must short-circuit on the "Native" kind hint regardless.
    let ax_tool_schema = |name: &str| {
        serde_json::json!({
            "type": "function",
            "function": {
                "name": name,
                "description": format!("{} stub", name),
                "parameters": {
                    "type": "object",
                    "properties": {},
                }
            }
        })
    };
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
                "name": "focus_window",
                "description": "Focus a window",
                "parameters": {
                    "type": "object",
                    "properties": {"app_name": {"type": "string"}},
                    "required": ["app_name"]
                }
            }
        }),
        ax_tool_schema("take_ax_snapshot"),
        ax_tool_schema("ax_click"),
        ax_tool_schema("ax_set_value"),
        ax_tool_schema("ax_select"),
    ];

    // Three observation rounds (one per LLM step) interleaved with
    // one real tool-call result for the initial launch_app. The
    // focus_window step is suppressed by the runner and therefore does
    // NOT consume an MCP result.
    let native_launch_result = ToolCallResult {
        content: vec![ToolContent::Text {
            text: serde_json::json!({
                "app_name": "Calculator",
                "pid": 9001,
                "kind": "Native",
                "status": "launched"
            })
            .to_string(),
        }],
        is_error: None,
    };
    let results = vec![
        cdp_empty_page(),     // step 0 observation
        native_launch_result, // launch_app call
        cdp_empty_page(),     // step 1 observation
        // step 1's focus_window never hits MCP — no result consumed.
        cdp_empty_page(), // step 2 observation
    ];
    let mcp = MockMcp::new(results, tools);

    let config = AgentConfig {
        max_steps: 5,
        build_workflow: true,
        use_cache: false,
        ..Default::default()
    };

    let (event_tx, mut event_rx) = mpsc::channel::<AgentEvent>(64);
    let (approval_tx, approval_rx) = approval_channel();
    let (approver, seen_approvals) = spawn_recording_approver(approval_rx);

    let mut runner = AgentRunner::new(&agent_llm, config)
        .with_events(event_tx)
        .with_approval(approval_tx);
    let workflow = clickweave_core::Workflow::new("focus-window-skip");
    let mcp_tools = mcp.tools_as_openai();

    let state = runner
        .run(
            "reach Calculator via AX".to_string(),
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

    // Primary assertion: the focus_window step DID run and succeeded
    // with the synthetic skip message — it did not fall through to MCP
    // or fail.
    let focus_step = state
        .steps
        .iter()
        .find(|s| match &s.command {
            AgentCommand::ToolCall { tool_name, .. } => tool_name == "focus_window",
            _ => false,
        })
        .expect("focus_window step must be recorded even when skipped");
    match &focus_step.outcome {
        StepOutcome::Success(text) => assert!(
            text.contains("skipped focus_window"),
            "Skipped focus_window must carry the synthetic skip message, got {:?}",
            text,
        ),
        other => panic!("Expected Success for skipped focus_window, got {:?}", other),
    }

    // Workflow-node assertion: the graph must NOT contain a FocusWindow
    // node for the skipped call — that node never actually ran.
    use clickweave_core::NodeType;
    let focus_node_count = state
        .workflow
        .nodes
        .iter()
        .filter(|n| matches!(n.node_type, NodeType::FocusWindow(_)))
        .count();
    assert_eq!(
        focus_node_count, 0,
        "Skipped focus_window must not produce a FocusWindow workflow node, graph = {:?}",
        state.workflow.nodes,
    );
    // Sanity: launch_app's LaunchApp node SHOULD be recorded — the guard
    // is scoped to focus_window only.
    let launch_node_count = state
        .workflow
        .nodes
        .iter()
        .filter(|n| matches!(n.node_type, NodeType::LaunchApp(_)))
        .count();
    assert_eq!(
        launch_node_count, 1,
        "launch_app must still produce a LaunchApp workflow node",
    );

    // Event assertion: the UI must see a SubAction that surfaces the skip
    // so the user understands why focus_window appears to "run" without
    // raising the window.
    let events = drain_events(&mut event_rx);
    let saw_sub_action = events.iter().any(|ev| {
        matches!(
            ev,
            AgentEvent::SubAction { tool_name, summary }
                if tool_name == "focus_window" && summary.contains("skipped")
        )
    });
    assert!(
        saw_sub_action,
        "Skipped focus_window must emit a SubAction event so the UI can surface it, events = {:?}",
        events,
    );

    // Approval assertion: a suppressed call must never prompt the user.
    let approvals = seen_approvals.lock().unwrap();
    assert!(
        !approvals.iter().any(|name| name == "focus_window"),
        "Skipped focus_window must not request user approval, saw {:?}",
        *approvals,
    );

    assert!(state.completed);
}

#[tokio::test]
async fn focus_window_still_runs_when_app_kind_is_unknown() {
    // Guard-off path: no prior structured response classified the app.
    // The runner must execute focus_window normally so cross-platform
    // and first-ever-focus flows are preserved. The MCP server does
    // expose the full AX toolset (so the kind check is the only thing
    // left to gate on), but without a recorded "Native" kind the guard
    // defers to the real tool call.
    let agent_llm = MockAgent::new(vec![
        MockAgent::tool_call_response("focus_window", r#"{"app_name": "UnseenApp"}"#, "call_focus"),
        MockAgent::done_response("Focused UnseenApp"),
    ]);

    let ax_tool_schema = |name: &str| {
        serde_json::json!({
            "type": "function",
            "function": {
                "name": name,
                "description": format!("{} stub", name),
                "parameters": {"type": "object", "properties": {}}
            }
        })
    };
    let tools = vec![
        serde_json::json!({
            "type": "function",
            "function": {
                "name": "focus_window",
                "description": "Focus a window",
                "parameters": {
                    "type": "object",
                    "properties": {"app_name": {"type": "string"}},
                    "required": ["app_name"]
                }
            }
        }),
        ax_tool_schema("take_ax_snapshot"),
        ax_tool_schema("ax_click"),
        ax_tool_schema("ax_set_value"),
        ax_tool_schema("ax_select"),
    ];

    let focus_result = ToolCallResult {
        content: vec![ToolContent::Text {
            // Older MCP text response — no structured JSON, no kind
            // hint, no way for the guard to know if this is Native.
            text: "Window focused successfully".to_string(),
        }],
        is_error: None,
    };
    let results = vec![
        cdp_empty_page(), // step 0 observation
        focus_result,     // real focus_window call
        cdp_empty_page(), // step 1 observation
    ];
    let mcp = MockMcp::new(results, tools);

    let config = AgentConfig {
        max_steps: 5,
        build_workflow: true,
        use_cache: false,
        ..Default::default()
    };

    let (event_tx, _event_rx) = mpsc::channel::<AgentEvent>(64);
    let (approval_tx, approval_rx) = approval_channel();
    let approver = spawn_auto_approver(approval_rx);

    let mut runner = AgentRunner::new(&agent_llm, config)
        .with_events(event_tx)
        .with_approval(approval_tx);
    let workflow = clickweave_core::Workflow::new("focus-unknown-kind");
    let mcp_tools = mcp.tools_as_openai();

    let state = runner
        .run(
            "focus UnseenApp".to_string(),
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

    let focus_step = state
        .steps
        .iter()
        .find(|s| match &s.command {
            AgentCommand::ToolCall { tool_name, .. } => tool_name == "focus_window",
            _ => false,
        })
        .expect("focus_window step must be recorded");
    match &focus_step.outcome {
        StepOutcome::Success(text) => {
            assert!(
                !text.contains("skipped"),
                "Unknown-kind focus_window must NOT be suppressed, got {:?}",
                text,
            );
            assert!(
                text.contains("Window focused"),
                "Expected the real MCP response text, got {:?}",
                text,
            );
        }
        other => panic!("Expected Success from real focus_window, got {:?}", other),
    }

    use clickweave_core::NodeType;
    let focus_node_count = state
        .workflow
        .nodes
        .iter()
        .filter(|n| matches!(n.node_type, NodeType::FocusWindow(_)))
        .count();
    assert_eq!(
        focus_node_count, 1,
        "Non-skipped focus_window must produce its FocusWindow workflow node",
    );

    assert!(state.completed);
}

// ---------------------------------------------------------------------------
// Scenario 7: Runner-side focus_window guard for Electron / CDP targets
//
// Once a CDP session is live for a Signal-like Electron app, subsequent
// `focus_window` calls against that same app must be suppressed — CDP
// dispatch operates on backgrounded windows without stealing focus, so
// the real `focus_window` would only disrupt the user's foreground.
// The pre-CDP-connect case is still exercised by the unit-level
// `should_skip_focus_window_defers_for_electron_or_chrome_without_live_cdp`
// test (see loop_runner.rs); this integration test covers the
// post-connect dispatch flow at the `AgentChannels` boundary.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn focus_window_after_cdp_connected_is_suppressed_for_electron_target() {
    // The agent — despite prompt guidance — issues focus_window against
    // a Signal-like Electron target after CDP has already landed. The
    // runner must suppress the real tool call and return the CDP skip
    // sentinel.
    let agent_llm = MockAgent::new(vec![
        MockAgent::tool_call_response("focus_window", r#"{"app_name": "Signal"}"#, "call_focus"),
        MockAgent::done_response("Reached Signal via CDP"),
    ]);

    // Only the bare-minimum CDP dispatch toolset is exposed — the skip
    // must not depend on also having AX tools. focus_window is declared
    // so the LLM can call it; cdp_find_elements / cdp_click satisfy the
    // `mcp_has_cdp_dispatch_toolset` gate.
    let simple_tool = |name: &str| {
        serde_json::json!({
            "type": "function",
            "function": {
                "name": name,
                "description": format!("{} stub", name),
                "parameters": {
                    "type": "object",
                    "properties": {"app_name": {"type": "string"}},
                }
            }
        })
    };
    let tools = vec![
        simple_tool("focus_window"),
        simple_tool("cdp_find_elements"),
        simple_tool("cdp_click"),
    ];

    // Two observation rounds, one per LLM step. The focus_window step
    // is suppressed by the runner and therefore does NOT consume an
    // MCP tool result.
    let results = vec![
        cdp_empty_page(), // step 0 observation
        // step 1's focus_window never hits MCP — no result consumed.
        cdp_empty_page(), // step 1 observation
    ];
    let mcp = MockMcp::new(results, tools);

    let config = AgentConfig {
        max_steps: 5,
        build_workflow: true,
        use_cache: false,
        ..Default::default()
    };

    let (event_tx, mut event_rx) = mpsc::channel::<AgentEvent>(64);
    let (approval_tx, approval_rx) = approval_channel();
    let (approver, seen_approvals) = spawn_recording_approver(approval_rx);

    let mut runner = AgentRunner::new(&agent_llm, config)
        .with_events(event_tx)
        .with_approval(approval_tx);
    // Short-circuit the full launch → auto_connect_cdp choreography
    // (which would need probe_app, quit, relaunch-with-debug-port, and
    // cdp_connect wired up end-to-end) by seeding the runner with the
    // exact state it would hold post-cdp_connect: kind=ElectronApp +
    // a live CDP session bound to "Signal".
    runner.seed_cdp_live_for_test("Signal", "ElectronApp");

    let workflow = clickweave_core::Workflow::new("focus-window-cdp-skip");
    let mcp_tools = mcp.tools_as_openai();

    let state = runner
        .run(
            "reach Signal via CDP".to_string(),
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

    // Primary assertion: the focus_window step ran, succeeded, and
    // carries the CDP skip sentinel — not the AX sentinel, and not a
    // real MCP response.
    let focus_step = state
        .steps
        .iter()
        .find(|s| match &s.command {
            AgentCommand::ToolCall { tool_name, .. } => tool_name == "focus_window",
            _ => false,
        })
        .expect("focus_window step must be recorded even when skipped");
    match &focus_step.outcome {
        StepOutcome::Success(text) => {
            assert!(
                text.contains("skipped focus_window"),
                "Skipped focus_window must carry a synthetic skip message, got {:?}",
                text,
            );
            assert!(
                text.contains("CDP"),
                "Electron skip must use the CDP sentinel, not the AX one, got {:?}",
                text,
            );
        }
        other => panic!("Expected Success for skipped focus_window, got {:?}", other),
    }

    // Workflow-node assertion: the graph must NOT contain a FocusWindow
    // node — a suppressed call never actually ran.
    use clickweave_core::NodeType;
    let focus_node_count = state
        .workflow
        .nodes
        .iter()
        .filter(|n| matches!(n.node_type, NodeType::FocusWindow(_)))
        .count();
    assert_eq!(
        focus_node_count, 0,
        "Skipped focus_window must not produce a FocusWindow workflow node, graph = {:?}",
        state.workflow.nodes,
    );

    // Event assertion: SubAction must describe the CDP-live reason so
    // the UI can surface it to the user.
    let events = drain_events(&mut event_rx);
    let saw_sub_action = events.iter().any(|ev| {
        matches!(
            ev,
            AgentEvent::SubAction { tool_name, summary }
                if tool_name == "focus_window" && summary.contains("CDP")
        )
    });
    assert!(
        saw_sub_action,
        "Skipped focus_window must emit a CDP SubAction event, events = {:?}",
        events,
    );

    // Approval assertion: a suppressed call must never prompt the user.
    let approvals = seen_approvals.lock().unwrap();
    assert!(
        !approvals.iter().any(|name| name == "focus_window"),
        "Skipped focus_window must not request user approval, saw {:?}",
        *approvals,
    );

    assert!(state.completed);
}

// ---------------------------------------------------------------------------
// Scenario 8: User policy — `allow_focus_window = false`
//
// When the operator explicitly disables `focus_window` via `AgentConfig`,
// every call is suppressed unconditionally — no probe for app kind, no
// CDP-connected check. This covers the "run this workflow entirely in
// the background" policy: even for cases that would normally defer
// (unknown app kind + no CDP + no AX toolset), `focus_window` must not
// steal focus. The LLM-facing skip text must steer it toward AX / CDP
// dispatch primitives so it doesn't silently try a coordinate click
// against a backgrounded window.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn focus_window_suppressed_when_allow_focus_window_policy_is_false() {
    // Otherwise-would-defer case: unknown app kind (no prior launch_app
    // response), no CDP session, minimal toolset (only focus_window
    // declared, no AX dispatch, no CDP dispatch). Without the policy
    // flag the runner would fall through to the real MCP call — we are
    // explicitly verifying that the policy short-circuits ahead of the
    // kind-aware branches.
    let agent_llm = MockAgent::new(vec![
        MockAgent::tool_call_response("focus_window", r#"{"app_name": "UnseenApp"}"#, "call_focus"),
        MockAgent::done_response("Used background dispatch instead"),
    ]);

    let tools = vec![serde_json::json!({
        "type": "function",
        "function": {
            "name": "focus_window",
            "description": "Focus a window",
            "parameters": {
                "type": "object",
                "properties": {"app_name": {"type": "string"}},
                "required": ["app_name"]
            }
        }
    })];

    // Only the two observation rounds are consumed — the focus_window
    // call is short-circuited by the policy and never reaches MCP, so
    // no third tool result is provided.
    let results = vec![
        cdp_empty_page(), // step 0 observation
        // step 1's focus_window is policy-suppressed — no MCP hit.
        cdp_empty_page(), // step 1 observation
    ];
    let mcp = MockMcp::new(results, tools);

    let config = AgentConfig {
        max_steps: 5,
        build_workflow: true,
        use_cache: false,
        allow_focus_window: false,
        ..Default::default()
    };

    let (event_tx, mut event_rx) = mpsc::channel::<AgentEvent>(64);
    let (approval_tx, approval_rx) = approval_channel();
    let (approver, seen_approvals) = spawn_recording_approver(approval_rx);

    let mut runner = AgentRunner::new(&agent_llm, config)
        .with_events(event_tx)
        .with_approval(approval_tx);
    let workflow = clickweave_core::Workflow::new("focus-window-policy-off");
    let mcp_tools = mcp.tools_as_openai();

    let state = runner
        .run(
            "complete task in background".to_string(),
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

    // Primary assertion: focus_window succeeded with the policy skip
    // sentinel — not the AX sentinel, not the CDP sentinel, and not a
    // real MCP response.
    let focus_step = state
        .steps
        .iter()
        .find(|s| match &s.command {
            AgentCommand::ToolCall { tool_name, .. } => tool_name == "focus_window",
            _ => false,
        })
        .expect("focus_window step must be recorded even when policy-suppressed");
    match &focus_step.outcome {
        StepOutcome::Success(text) => {
            assert!(
                text.contains("focus_window skipped"),
                "Policy-suppressed focus_window must carry the policy skip sentinel, got {:?}",
                text,
            );
            assert!(
                text.contains("agent policy"),
                "Skip text must name the policy so the LLM understands why, got {:?}",
                text,
            );
            // The LLM must see the dispatch primitives it should use
            // instead of falling through to coordinate tools against a
            // backgrounded window.
            assert!(
                text.contains("ax_click") || text.contains("cdp_click"),
                "Skip text must nudge the LLM toward AX / CDP dispatch, got {:?}",
                text,
            );
        }
        other => panic!(
            "Expected Success for policy-suppressed focus_window, got {:?}",
            other,
        ),
    }

    // Workflow-node assertion: no FocusWindow node — the suppressed
    // call never actually ran, so it must stay invisible to the graph.
    use clickweave_core::NodeType;
    let focus_node_count = state
        .workflow
        .nodes
        .iter()
        .filter(|n| matches!(n.node_type, NodeType::FocusWindow(_)))
        .count();
    assert_eq!(
        focus_node_count, 0,
        "Policy-suppressed focus_window must not produce a FocusWindow workflow node, graph = {:?}",
        state.workflow.nodes,
    );

    // Event assertion: SubAction must announce the policy reason so
    // the UI can surface why focus_window appears to run without
    // raising the window.
    let events = drain_events(&mut event_rx);
    let saw_sub_action = events.iter().any(|ev| {
        matches!(
            ev,
            AgentEvent::SubAction { tool_name, summary }
                if tool_name == "focus_window" && summary.contains("policy")
        )
    });
    assert!(
        saw_sub_action,
        "Policy-suppressed focus_window must emit a SubAction event naming the policy, events = {:?}",
        events,
    );

    // Approval assertion: a suppressed call must never prompt the user
    // — same contract as the AX / CDP-live skip paths.
    let approvals = seen_approvals.lock().unwrap();
    assert!(
        !approvals.iter().any(|name| name == "focus_window"),
        "Policy-suppressed focus_window must not request user approval, saw {:?}",
        *approvals,
    );

    assert!(state.completed);
}
