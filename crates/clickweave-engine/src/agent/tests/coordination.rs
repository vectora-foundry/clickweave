//! Cross-task coordination integration tests.
//!
//! These exercise the `AgentChannels` contract end-to-end — a harness task
//! plays the role the Tauri forwarders play in production, and the
//! assertions verify the engine responds to realistic cross-task event
//! sequences. See `issue-101-agent-integration-tests` for the scenarios.

use super::{CapturingMockAgent, MockAgent, MockMcp};
use crate::agent::loop_runner::AgentRunner;
use crate::agent::types::*;
use crate::executor::Mcp;
use clickweave_llm::Message;
use clickweave_mcp::{ToolCallResult, ToolContent};
use serde_json::Value;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};

// ---------------------------------------------------------------------------
// Test helpers
// ---------------------------------------------------------------------------

/// Build a successful CDP find-elements response with a single Submit button.
/// Used as the standard "approval-gated" observation fixture.
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

/// Plain-text CDP find-elements response with zero matches.
/// Used for native / no-CDP paths where elements are empty.
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

// ---------------------------------------------------------------------------
// Scenario 1: Stop during approval wait
//
// The Tauri approval forwarder's `AgentHandle::force_stop()` must drive the
// engine to a `Rejected` outcome, not `ApprovalUnavailable`. The harness
// below simulates the exact drop_on_cancel-vs-send_false difference:
// dropping the oneshot sender surfaces as `ApprovalUnavailable`, while
// explicitly sending `false` surfaces as `Rejected` → `Replan`.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn stop_during_approval_wait_sends_rejection_not_channel_drop() {
    // The LLM proposes a click that needs approval, then after the replan
    // declares the goal done (so the run can reach a terminal state).
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
    let (approval_tx, mut approval_rx) =
        mpsc::channel::<(ApprovalRequest, oneshot::Sender<bool>)>(1);

    // Harness: mimic `AgentHandle::force_stop()` — receive the approval
    // request, then instead of dropping the oneshot, send `false` on it
    // (the fix) so the engine observes an explicit rejection and replans
    // rather than tearing down with `ApprovalUnavailable`.
    tokio::spawn(async move {
        if let Some((_req, resp_tx)) = approval_rx.recv().await {
            // Simulate a brief approval-wait window.
            tokio::time::sleep(Duration::from_millis(10)).await;
            let _ = resp_tx.send(false);
        }
    });

    let mut runner = AgentRunner::new(&agent_llm, config)
        .with_events(event_tx)
        .with_approval(approval_tx);
    let workflow = clickweave_core::Workflow::new("Stop-during-approval");
    let mcp_tools = mcp.tools_as_openai();

    let state = runner
        .run("Click it".to_string(), workflow, &mcp, None, mcp_tools)
        .await
        .unwrap();

    // Explicit rejection must surface as Replan (the terminal state of the
    // cancelled step), never as ApprovalUnavailable.
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
// Scenario 2: Buffered events drained between runs do not leak
//
// The event forwarder in Tauri drains remaining events on cancel. The
// engine-level contract is: whatever the forwarder sees on the event
// channel is tagged with the run_id it began forwarding for. This test
// demonstrates that two back-to-back runs each have their own event
// streams — channel A's events cannot appear on channel B.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn buffered_events_do_not_leak_between_runs() {
    // Run A — produce several events, then let them sit in the buffer.
    let llm_a = MockAgent::new(vec![
        MockAgent::tool_call_response("click", r#"{"x": 1, "y": 2}"#, "call_a0"),
        MockAgent::done_response("run A done"),
    ]);
    let mcp_a = MockMcp::with_click_tool();

    let (event_tx_a, mut event_rx_a) = mpsc::channel::<AgentEvent>(64);
    // Auto-approve so the run progresses without external coordination.
    let (approval_tx_a, mut approval_rx_a) =
        mpsc::channel::<(ApprovalRequest, oneshot::Sender<bool>)>(1);
    tokio::spawn(async move {
        while let Some((_req, resp_tx)) = approval_rx_a.recv().await {
            let _ = resp_tx.send(true);
        }
    });

    let mut runner_a = AgentRunner::new(&llm_a, AgentConfig::default())
        .with_events(event_tx_a)
        .with_approval(approval_tx_a);
    let _ = runner_a
        .run(
            "goal A".to_string(),
            clickweave_core::Workflow::new("A"),
            &mcp_a,
            None,
            mcp_a.tools_as_openai(),
        )
        .await
        .unwrap();

    // Drain run A's events — all events collected here are tagged with
    // run A's lifecycle (the channel is exclusively A's).
    let mut run_a_events = Vec::new();
    while let Ok(ev) = event_rx_a.try_recv() {
        run_a_events.push(ev);
    }
    assert!(!run_a_events.is_empty(), "Run A must have produced events");

    // Run B — fresh channels. Run A's channel is dropped; B's sender
    // cannot carry A's history.
    let llm_b = MockAgent::new(vec![MockAgent::done_response("run B done")]);
    let mcp_b = MockMcp::with_click_tool();

    let (event_tx_b, mut event_rx_b) = mpsc::channel::<AgentEvent>(64);
    let (approval_tx_b, _approval_rx_b) =
        mpsc::channel::<(ApprovalRequest, oneshot::Sender<bool>)>(1);
    let mut runner_b = AgentRunner::new(&llm_b, AgentConfig::default())
        .with_events(event_tx_b)
        .with_approval(approval_tx_b);
    let _ = runner_b
        .run(
            "goal B".to_string(),
            clickweave_core::Workflow::new("B"),
            &mcp_b,
            None,
            mcp_b.tools_as_openai(),
        )
        .await
        .unwrap();

    // Collect run B's events. The count of B's events must match what B
    // produced, with zero contamination from A (the channels are isolated
    // so this is trivially true at the engine layer — we document the
    // contract that Tauri's forwarder relies on).
    let mut run_b_events = Vec::new();
    while let Ok(ev) = event_rx_b.try_recv() {
        run_b_events.push(ev);
    }
    // Both runs should have emitted GoalComplete. A's GoalComplete must
    // have arrived on A's channel only.
    let a_has_goal_complete = run_a_events
        .iter()
        .any(|e| matches!(e, AgentEvent::GoalComplete { .. }));
    let b_has_goal_complete = run_b_events
        .iter()
        .any(|e| matches!(e, AgentEvent::GoalComplete { .. }));
    assert!(
        a_has_goal_complete,
        "Run A should have emitted GoalComplete"
    );
    assert!(
        b_has_goal_complete,
        "Run B should have emitted GoalComplete"
    );

    // Run B's GoalComplete must carry B's summary, never A's.
    let b_summary = run_b_events.iter().find_map(|e| match e {
        AgentEvent::GoalComplete { summary } => Some(summary.clone()),
        _ => None,
    });
    assert_eq!(b_summary.as_deref(), Some("run B done"));
}

// ---------------------------------------------------------------------------
// Scenario 3: Cached launch_app replay runs approval and post-tool hooks
//
// When a previous run cached a `launch_app` decision, replay must:
//   - Re-request approval (state-changing tool).
//   - Run the `maybe_cdp_connect` post-tool hook (probe_app, etc.).
// Asserting the approval request arrives and the probe_app call fires
// proves both behaviors wire through the cache-replay path.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn cached_launch_app_replay_requests_approval_and_runs_post_tool_hook() {
    // Seed the cache with a `launch_app` decision for a known page.
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

    // LLM only needs to handle the `done` step after replay.
    let agent_llm = MockAgent::new(vec![MockAgent::done_response("Launched via cache")]);

    // MCP: advertises launch_app, probe_app, and cdp_connect so the
    // post-tool hook has all its entry points. probe_app returns a
    // "NativeApp" probe result so auto_connect_cdp short-circuits
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

    // Results: (1) first observation, (2) cached launch_app result,
    // (3) probe_app returns NativeApp, (4) second observation,
    // (5) fallbacks for any extra queries.
    let results = vec![
        cdp_button_page("https://example.com"),
        ToolCallResult {
            content: vec![ToolContent::Text {
                text: "Launched".to_string(),
            }],
            is_error: None,
        },
        ToolCallResult {
            content: vec![ToolContent::Text {
                text: "NativeApp".to_string(),
            }],
            is_error: None,
        },
        cdp_button_page("https://example.com/next"),
    ];
    let mcp = MockMcp::new(results, tools);

    let config = AgentConfig {
        max_steps: 5,
        build_workflow: false,
        use_cache: true,
        ..Default::default()
    };

    // Approval harness — record whether an approval request was seen and
    // auto-approve so the run progresses.
    let (event_tx, mut event_rx) = mpsc::channel::<AgentEvent>(64);
    let (approval_tx, mut approval_rx) =
        mpsc::channel::<(ApprovalRequest, oneshot::Sender<bool>)>(1);
    let seen_approvals: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let seen_clone = seen_approvals.clone();
    tokio::spawn(async move {
        while let Some((req, resp_tx)) = approval_rx.recv().await {
            seen_clone.lock().unwrap().push(req.tool_name.clone());
            let _ = resp_tx.send(true);
        }
    });

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
        )
        .await
        .unwrap();

    // The replayed launch_app step must have passed through the approval
    // gate — the harness observed a `launch_app` approval request.
    let approvals = seen_approvals.lock().unwrap();
    assert!(
        approvals.iter().any(|name| name == "launch_app"),
        "Cached launch_app replay must re-request approval, saw {:?}",
        *approvals,
    );

    // The post-tool hook must have fired — a SubAction event for probe_app
    // is emitted from `auto_connect_cdp` before any Electron/CDP branching.
    let mut saw_probe_sub_action = false;
    while let Ok(ev) = event_rx.try_recv() {
        if let AgentEvent::SubAction { tool_name, .. } = &ev
            && tool_name == "probe_app"
        {
            saw_probe_sub_action = true;
        }
    }
    assert!(
        saw_probe_sub_action,
        "Cached launch_app replay must run the post-tool hook (probe_app SubAction)"
    );

    assert!(state.completed);
}

// ---------------------------------------------------------------------------
// Scenario 4: Native / no-CDP path does not read or write the cache
//
// When `cdp_find_elements` returns no matches (native paths without CDP),
// the cache lookup and cache store are both skipped — otherwise every
// native step would collide on a degenerate empty-elements key.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn empty_elements_skips_cache_read_and_write() {
    // Pre-seed the cache with an entry keyed on *empty* elements. If the
    // engine ever looked up using empty elements, this entry would hit
    // and trigger a replay. The test proves it does not.
    let mut cache = AgentCache::default();
    cache.store(
        "click somewhere",
        &[],
        "click".to_string(),
        serde_json::json!({"x": 99, "y": 99}),
    );

    // LLM proposes a click, then declares done. The pre-seeded cache
    // must NOT be replayed (if it were, the LLM would only be invoked
    // once — for the done step).
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
    let results = vec![
        cdp_empty_page(), // step 0 observation (empty → no cache lookup)
        ToolCallResult {
            content: vec![ToolContent::Text {
                text: "clicked".to_string(),
            }],
            is_error: None,
        },
        cdp_empty_page(), // step 1 observation (still empty)
    ];
    let mcp = MockMcp::new(results, tools);

    let config = AgentConfig {
        max_steps: 5,
        build_workflow: false,
        use_cache: true,
        ..Default::default()
    };

    let (event_tx, _event_rx) = mpsc::channel::<AgentEvent>(64);
    let (approval_tx, mut approval_rx) =
        mpsc::channel::<(ApprovalRequest, oneshot::Sender<bool>)>(1);
    tokio::spawn(async move {
        while let Some((_req, resp_tx)) = approval_rx.recv().await {
            let _ = resp_tx.send(true);
        }
    });

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
        )
        .await
        .unwrap();

    // The LLM was called twice — once for the click choice and once for
    // the done call. If cache replay had fired (bug), the first step
    // would be a cache-replayed click instead, and the LLM would be
    // called for a NEW click in a later step (bug path).
    let calls = capturing.lock().unwrap();
    assert_eq!(
        calls.len(),
        2,
        "LLM should be called twice (click + done) — empty elements must skip cache lookup"
    );

    // Read-side guard: the first step's tool_call_id must be from the
    // LLM response ("call_native_0"), not from the cache ("cache-0").
    // If the cache read fired against empty elements, the pre-seeded
    // entry would have replayed with a synthetic "cache-0" id.
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

    // Write-side guard: the pre-seeded entry's hit_count must not have
    // incremented. `store()` bumps hit_count, so if the click's empty
    // observation triggered a cache write on success, the seed would
    // have been overwritten with hit_count=2.
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
//
// When the LLM calls a tool whose name is not mappable to a `NodeType`
// and is not advertised via `known_tools`, the engine emits an
// `AgentEvent::Warning` and the run proceeds — it must NOT terminate
// with an error.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn workflow_mapping_miss_emits_warning_and_run_continues() {
    // LLM picks a tool name that is NOT in `known_tools`. The LLM can
    // call any name via the tool_calls list, so we simulate a model that
    // hallucinates a tool that the MCP doesn't advertise.
    let agent_llm = MockAgent::new(vec![
        MockAgent::tool_call_response("weird_tool", r#"{"foo": "bar"}"#, "call_weird"),
        MockAgent::done_response("Completed after mapping miss"),
    ]);

    // MCP: no tools advertised — anything the LLM calls will map to an
    // unknown-tool error during workflow-node construction.
    let results = vec![
        cdp_button_page("https://example.com"),
        ToolCallResult {
            content: vec![ToolContent::Text {
                text: "executed".to_string(),
            }],
            is_error: None,
        },
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
    let (approval_tx, mut approval_rx) =
        mpsc::channel::<(ApprovalRequest, oneshot::Sender<bool>)>(1);
    tokio::spawn(async move {
        while let Some((_req, resp_tx)) = approval_rx.recv().await {
            let _ = resp_tx.send(true);
        }
    });

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
        )
        .await
        .unwrap();

    // Run completed — the mapping miss did NOT abort the loop.
    assert!(
        state.completed,
        "Mapping miss must not abort the loop, state={:?}",
        state.terminal_reason,
    );

    // A Warning event was emitted; no terminal Error was emitted.
    let mut saw_warning = false;
    let mut saw_error = false;
    while let Ok(ev) = event_rx.try_recv() {
        match ev {
            AgentEvent::Warning { .. } => saw_warning = true,
            AgentEvent::Error { .. } => saw_error = true,
            _ => {}
        }
    }
    assert!(saw_warning, "Mapping miss must emit AgentEvent::Warning");
    assert!(
        !saw_error,
        "Mapping miss must not emit AgentEvent::Error (the run continues)"
    );
}
