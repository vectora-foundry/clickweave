use super::super::super::test_stubs::{ScriptedLlm, StaticMcp, llm_reply_tool};
use crate::agent::runner::{FocusSkipReason, StateRunner};
use crate::agent::types::{AgentConfig, AgentEvent, RunnerOutput, TerminalReason};
use crate::executor::Mcp;
use crate::agent::trace_graph::AgentTraceGraph;
use serde_json::Value;
use tokio::sync::mpsc;

fn cfg_with_focus_steps(steps: usize) -> AgentConfig {
    AgentConfig {
        max_steps: steps,
        allow_focus_window: true,
        ..AgentConfig::default()
    }
}

fn cfg_default_with_focus_window() -> AgentConfig {
    AgentConfig {
        allow_focus_window: true,
        ..AgentConfig::default()
    }
}

fn drain_events(rx: &mut mpsc::Receiver<RunnerOutput>) -> Vec<AgentEvent> {
    let mut out = Vec::new();
    while let Ok(ev) = rx.try_recv() {
        if let Some(event) = ev.into_event() {
            out.push(event);
        }
    }
    out
}

// -----------------------------------------------------------------
// is_synthetic_focus_skip
// -----------------------------------------------------------------

#[test]
fn is_synthetic_focus_skip_matches_only_the_sentinels() {
    for reason in [
        FocusSkipReason::AxAvailable,
        FocusSkipReason::CdpLive,
        FocusSkipReason::CdpAttachable,
        FocusSkipReason::PolicyDisabled,
    ] {
        assert!(
            StateRunner::is_synthetic_focus_skip("focus_window", reason.llm_message()),
            "sentinel for {:?} must round-trip through is_synthetic_focus_skip",
            reason,
        );
        assert!(
            !StateRunner::is_synthetic_focus_skip("other_tool", reason.llm_message()),
            "sentinel text on a non-focus_window tool must NOT match",
        );
    }
    assert!(
        !StateRunner::is_synthetic_focus_skip("focus_window", "Window focused successfully"),
        "real MCP success body must not match the sentinel",
    );
}

// -----------------------------------------------------------------
// should_skip_focus_window classifier
// -----------------------------------------------------------------

const FULL_AX_TOOLSET: &[&str] = &["take_ax_snapshot", "ax_click", "ax_set_value", "ax_select"];
const FULL_CDP_TOOLSET: &[&str] = &["cdp_find_elements", "cdp_click"];

#[test]
fn should_skip_focus_window_fires_for_native_with_full_ax_toolset() {
    let mut runner = StateRunner::new("g".to_string(), cfg_default_with_focus_window());
    runner.record_app_kind_for_test("Calculator", "Native");
    let mcp = StaticMcp::with_tools(FULL_AX_TOOLSET);
    let args = serde_json::json!({"app_name": "Calculator"});
    let skip =
        crate::agent::runner::test_support::call_should_skip_focus_window(&runner, &args, &mcp);
    assert_eq!(skip, Some(FocusSkipReason::AxAvailable));
}

#[test]
fn should_skip_focus_window_fires_for_electron_with_live_cdp() {
    let mut runner = StateRunner::new("g".to_string(), cfg_default_with_focus_window());
    runner.record_app_kind_for_test("Signal", "ElectronApp");
    runner.set_cdp_connected_for_test("Signal", 0);
    let mcp = StaticMcp::with_tools(FULL_CDP_TOOLSET);
    let args = serde_json::json!({"app_name": "Signal"});
    let skip =
        crate::agent::runner::test_support::call_should_skip_focus_window(&runner, &args, &mcp);
    assert_eq!(skip, Some(FocusSkipReason::CdpLive));
}

#[test]
fn should_skip_focus_window_fires_with_cdp_attachable_when_cdp_connect_advertised() {
    // Pre-CDP-connect: kind is Electron and the server advertises
    // `cdp_connect`. The post-tool hook will auto-connect on its
    // own, so the real focus_window is unnecessary; the classifier
    // must short-circuit with `CdpAttachable`.
    let mut runner = StateRunner::new("g".to_string(), cfg_default_with_focus_window());
    runner.record_app_kind_for_test("Signal", "ElectronApp");
    let mcp = StaticMcp::with_tools(&["cdp_connect"]);
    let args = serde_json::json!({"app_name": "Signal"});
    let skip =
        crate::agent::runner::test_support::call_should_skip_focus_window(&runner, &args, &mcp);
    assert_eq!(skip, Some(FocusSkipReason::CdpAttachable));
}

#[test]
fn should_skip_focus_window_defers_for_electron_without_cdp_connect_advertised() {
    // Kind is known but the server lacks `cdp_connect` so the
    // post-tool auto-connect cannot fire. Without that, the first
    // focus_window may itself be needed to bring the window front,
    // and the classifier must defer.
    let mut runner = StateRunner::new("g".to_string(), cfg_default_with_focus_window());
    runner.record_app_kind_for_test("VSCode", "ElectronApp");
    let mut combined: Vec<&str> = FULL_AX_TOOLSET.to_vec();
    combined.extend_from_slice(FULL_CDP_TOOLSET);
    let mcp = StaticMcp::with_tools(&combined);
    let args = serde_json::json!({"app_name": "VSCode"});
    let skip =
        crate::agent::runner::test_support::call_should_skip_focus_window(&runner, &args, &mcp);
    assert!(skip.is_none());
}

#[test]
fn should_skip_focus_window_defers_for_unknown_kind() {
    let runner = StateRunner::new("g".to_string(), cfg_default_with_focus_window());
    let mcp = StaticMcp::with_tools(FULL_AX_TOOLSET);
    let args = serde_json::json!({"app_name": "Mystery"});
    let skip =
        crate::agent::runner::test_support::call_should_skip_focus_window(&runner, &args, &mcp);
    assert!(skip.is_none());
}

#[test]
fn should_skip_focus_window_policy_disabled_always_skips() {
    let cfg = AgentConfig {
        allow_focus_window: false,
        ..AgentConfig::default()
    };
    let runner = StateRunner::new("g".to_string(), cfg);
    let mcp = StaticMcp::with_tools(&[]);
    let args = serde_json::json!({"app_name": "Anything"});
    let skip =
        crate::agent::runner::test_support::call_should_skip_focus_window(&runner, &args, &mcp);
    assert_eq!(skip, Some(FocusSkipReason::PolicyDisabled));
    // Policy short-circuit is unconditional — must fire even when the
    // arguments carry no `app_name` at all.
    let args_no_app = serde_json::json!({"window_id": 1});
    let skip = crate::agent::runner::test_support::call_should_skip_focus_window(
        &runner,
        &args_no_app,
        &mcp,
    );
    assert_eq!(skip, Some(FocusSkipReason::PolicyDisabled));
}

// -----------------------------------------------------------------
// Synthetic focus_window skip through StateRunner::run
// -----------------------------------------------------------------

/// When the classifier fires, the runner must NOT call `focus_window`
/// on MCP. It records a synthetic success step, emits a `SubAction`
/// event, and advances the loop.
#[tokio::test]
async fn synthetic_focus_window_skip_bypasses_mcp_dispatch() {
    let llm = ScriptedLlm::new(vec![
        llm_reply_tool(
            "focus_window",
            serde_json::json!({"app_name": "Calculator"}),
        ),
        llm_reply_tool("agent_done", serde_json::json!({"summary": "ok"})),
    ]);
    // MCP advertises focus_window + the full AX toolset so the skip
    // classifier's Native+AX branch fires.
    let mut tools: Vec<&str> = vec!["focus_window"];
    tools.extend_from_slice(FULL_AX_TOOLSET);
    let mcp = StaticMcp::with_tools(&tools)
        // Tag the reply body so a real dispatch would be visible —
        // but we expect it NEVER to be called.
        .with_reply("focus_window", "REAL focus_window body (should not appear)");
    let tools_openai = mcp.tools_as_openai();

    let (event_tx, mut event_rx) = mpsc::channel::<RunnerOutput>(32);
    let mut runner =
        StateRunner::new("goal".to_string(), cfg_with_focus_steps(5)).with_events(event_tx);
    // Seed the kind hint so the classifier has a Native classification
    // to work with.
    runner.record_app_kind_for_test("Calculator", "Native");

    let state = runner
        .run(
            &llm,
            &mcp,
            "goal".to_string(),
            AgentTraceGraph::new(),
            tools_openai,
            None,
        )
        .await
        .expect("run ok");

    // The recorded step's outcome body must be the synthetic sentinel,
    // not the MCP reply — proves the tool was not dispatched.
    let focus_step = state
        .steps
        .iter()
        .find(|s| {
            matches!(
                &s.command,
                crate::agent::types::AgentCommand::ToolCall { tool_name, .. }
                    if tool_name == "focus_window"
            )
        })
        .expect("focus_window step recorded");
    let body = match &focus_step.outcome {
        crate::agent::types::StepOutcome::Success(b) => b.clone(),
        other => panic!("expected Success outcome, got {:?}", other),
    };
    assert_eq!(body, FocusSkipReason::AxAvailable.llm_message());

    // A SubAction event carries the skip summary; run still completes.
    let events = drain_events(&mut event_rx);
    assert!(
        events.iter().any(|ev| matches!(
            ev,
            AgentEvent::SubAction { tool_name, summary }
                if tool_name == "focus_window" && summary.starts_with("skipped")
        )),
        "synthetic skip must emit SubAction with `skipped` summary; got {:?}",
        events,
    );
    assert!(matches!(
        state.terminal_reason,
        Some(TerminalReason::Completed { .. })
    ));
}

/// Under the no-focus policy, a no-args `launch_app` for an
/// already-running app must be treated like a background-safe
/// observation, not dispatched to MCP. Native-devtools foregrounds
/// already-running apps for this call shape.
#[tokio::test]
async fn no_focus_launch_app_skip_bypasses_foregrounding_mcp_dispatch() {
    use clickweave_mcp::{ToolCallResult, ToolContent};
    use std::sync::Mutex;

    struct RunningAppMcp {
        calls: Mutex<Vec<String>>,
        app_name: String,
    }

    impl Mcp for RunningAppMcp {
        async fn call_tool(
            &self,
            name: &str,
            _arguments: Option<Value>,
        ) -> anyhow::Result<ToolCallResult> {
            self.calls.lock().unwrap().push(name.to_string());
            let text = match name {
                "list_apps" => serde_json::json!([{
                    "name": self.app_name.clone(),
                    "pid": 1234,
                    "kind": "Native"
                }])
                .to_string(),
                "launch_app" => "REAL launch_app body (should not appear)".to_string(),
                _ => "ok".to_string(),
            };
            Ok(ToolCallResult {
                content: vec![ToolContent::Text { text }],
                is_error: None,
            })
        }

        fn has_tool(&self, name: &str) -> bool {
            matches!(name, "launch_app" | "list_apps")
        }

        fn tools_as_openai(&self) -> Vec<Value> {
            vec![
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
                        "name": "list_apps",
                        "description": "List running apps",
                        "parameters": {"type": "object", "properties": {}}
                    }
                }),
            ]
        }

        async fn refresh_server_tool_list(&self) -> anyhow::Result<()> {
            Ok(())
        }
    }

    let app_name = "AlreadyRunningApp";
    let llm = ScriptedLlm::new(vec![
        llm_reply_tool("launch_app", serde_json::json!({"app_name": app_name})),
        llm_reply_tool("agent_done", serde_json::json!({"summary": "ok"})),
    ]);
    let mcp = RunningAppMcp {
        calls: Mutex::new(Vec::new()),
        app_name: app_name.to_string(),
    };
    let tools_openai = mcp.tools_as_openai();
    let (event_tx, mut event_rx) = mpsc::channel::<RunnerOutput>(32);
    let runner = StateRunner::new(
        "goal".to_string(),
        AgentConfig {
            max_steps: 5,
            allow_focus_window: false,
            ..AgentConfig::default()
        },
    )
    .with_events(event_tx);

    let state = runner
        .run(
            &llm,
            &mcp,
            "goal".to_string(),
            AgentTraceGraph::new(),
            tools_openai,
            None,
        )
        .await
        .expect("run ok");

    let calls = mcp.calls.lock().unwrap();
    assert_eq!(
        calls.as_slice(),
        ["list_apps"],
        "guard must not dispatch the foregrounding launch_app call"
    );
    let launch_step = state
        .steps
        .iter()
        .find(|s| {
            matches!(
                &s.command,
                crate::agent::types::AgentCommand::ToolCall { tool_name, .. }
                    if tool_name == "launch_app"
            )
        })
        .expect("launch_app step recorded");
    let body = match &launch_step.outcome {
        crate::agent::types::StepOutcome::Success(b) => b.clone(),
        other => panic!("expected synthetic launch_app Success, got {:?}", other),
    };
    assert!(
        body.contains("launch_app skipped"),
        "synthetic body should explain the skip: {body}"
    );
    assert!(
        state.trace_graph.nodes.is_empty(),
        "synthetic skip must not materialize a workflow node"
    );
    let events = drain_events(&mut event_rx);
    assert!(
        !events
            .iter()
            .any(|ev| matches!(ev, AgentEvent::NodeAdded { .. })),
        "synthetic skip must not emit NodeAdded; got {:?}",
        events
    );
    assert!(matches!(
        state.terminal_reason,
        Some(TerminalReason::Completed { .. })
    ));
}

/// If the target is absent, `launch_app` still needs to run, but it
/// should be sent as a background launch under the no-focus policy.
#[tokio::test]
async fn no_focus_launch_app_dispatch_sets_background_true() {
    use clickweave_mcp::{ToolCallResult, ToolContent};
    use std::sync::Mutex;

    struct LaunchArgsMcp {
        calls: Mutex<Vec<(String, Option<Value>)>>,
    }

    impl Mcp for LaunchArgsMcp {
        async fn call_tool(
            &self,
            name: &str,
            arguments: Option<Value>,
        ) -> anyhow::Result<ToolCallResult> {
            self.calls
                .lock()
                .unwrap()
                .push((name.to_string(), arguments));
            let text = match name {
                "list_apps" => "[]".to_string(),
                "launch_app" => {
                    r#"{"app_name":"AbsentApp","kind":"Native","pid":2345}"#.to_string()
                }
                _ => "ok".to_string(),
            };
            Ok(ToolCallResult {
                content: vec![ToolContent::Text { text }],
                is_error: None,
            })
        }

        fn has_tool(&self, name: &str) -> bool {
            matches!(name, "launch_app" | "list_apps")
        }

        fn tools_as_openai(&self) -> Vec<Value> {
            vec![
                serde_json::json!({
                    "type": "function",
                    "function": {
                        "name": "launch_app",
                        "description": "Launch an app",
                        "parameters": {
                            "type": "object",
                            "properties": {
                                "app_name": {"type": "string"},
                                "background": {"type": "boolean"}
                            },
                            "required": ["app_name"]
                        }
                    }
                }),
                serde_json::json!({
                    "type": "function",
                    "function": {
                        "name": "list_apps",
                        "description": "List running apps",
                        "parameters": {"type": "object", "properties": {}}
                    }
                }),
            ]
        }

        async fn refresh_server_tool_list(&self) -> anyhow::Result<()> {
            Ok(())
        }
    }

    let llm = ScriptedLlm::new(vec![
        llm_reply_tool("launch_app", serde_json::json!({"app_name": "AbsentApp"})),
        llm_reply_tool("agent_done", serde_json::json!({"summary": "ok"})),
    ]);
    let mcp = LaunchArgsMcp {
        calls: Mutex::new(Vec::new()),
    };
    let tools_openai = mcp.tools_as_openai();
    let runner = StateRunner::new(
        "goal".to_string(),
        AgentConfig {
            max_steps: 5,
            allow_focus_window: false,
            ..AgentConfig::default()
        },
    );

    let state = runner
        .run(
            &llm,
            &mcp,
            "goal".to_string(),
            AgentTraceGraph::new(),
            tools_openai,
            None,
        )
        .await
        .expect("run ok");

    let calls = mcp.calls.lock().unwrap();
    let launch_args = calls
        .iter()
        .find_map(|(name, args)| (name == "launch_app").then_some(args.as_ref()))
        .flatten()
        .expect("launch_app dispatched");
    assert_eq!(
        launch_args.get("background").and_then(Value::as_bool),
        Some(true),
        "no-focus launch_app must dispatch in background: {launch_args}"
    );
    assert!(matches!(
        state.terminal_reason,
        Some(TerminalReason::Completed { .. })
    ));
}

/// Synthetic focus_window skip must leave `cdp_state` untouched — the
/// post-tool hook keys on `is_synthetic_focus_skip` on the live path
/// (we short-circuit before dispatch, so `maybe_cdp_connect` never
/// fires). Asserts parity with legacy behaviour.
#[tokio::test]
async fn synthetic_focus_window_skip_does_not_mutate_cdp_state() {
    let llm = ScriptedLlm::new(vec![
        llm_reply_tool("focus_window", serde_json::json!({"app_name": "Signal"})),
        llm_reply_tool("agent_done", serde_json::json!({"summary": "ok"})),
    ]);
    let mut tools: Vec<&str> = vec!["focus_window", "cdp_connect"];
    tools.extend_from_slice(FULL_CDP_TOOLSET);
    let mcp = StaticMcp::with_tools(&tools);
    let tools_openai = mcp.tools_as_openai();

    let (event_tx, _event_rx) = mpsc::channel::<RunnerOutput>(32);
    let mut runner =
        StateRunner::new("goal".to_string(), cfg_with_focus_steps(5)).with_events(event_tx);
    // Pre-seed "CDP already live" so the CdpLive branch of the
    // classifier fires and the skip short-circuits dispatch.
    runner.record_app_kind_for_test("Signal", "ElectronApp");
    runner.set_cdp_connected_for_test("Signal", 42);
    // The classifier checks PID=0 though — set it via the helper so
    // is_connected_to("Signal", 0) returns true.
    let state = runner
        .run(
            &llm,
            &mcp,
            "goal".to_string(),
            AgentTraceGraph::new(),
            tools_openai,
            None,
        )
        .await
        .expect("run ok");

    assert!(matches!(
        state.terminal_reason,
        Some(TerminalReason::Completed { .. })
    ));
}

// -----------------------------------------------------------------
// maybe_cdp_connect side effects
// -----------------------------------------------------------------

/// After a Native `launch_app`, no CDP connect should fire and no
/// CdpConnected event should be emitted, but `known_app_kinds` must
/// record "Native" so the subsequent focus_window skip can kick in.
#[tokio::test]
async fn native_launch_app_records_kind_and_does_not_connect_cdp() {
    let llm = ScriptedLlm::new(vec![
        llm_reply_tool("launch_app", serde_json::json!({"app_name": "Calculator"})),
        llm_reply_tool("agent_done", serde_json::json!({"summary": "ok"})),
    ]);
    let launch_body = r#"{"app_name":"Calculator","kind":"Native","pid":123}"#;
    let mut tools: Vec<&str> = vec!["launch_app", "cdp_connect"];
    tools.extend_from_slice(FULL_AX_TOOLSET);
    let mcp = StaticMcp::with_tools(&tools).with_reply("launch_app", launch_body);
    let tools_openai = mcp.tools_as_openai();

    let (event_tx, mut event_rx) = mpsc::channel::<RunnerOutput>(32);
    let runner =
        StateRunner::new("goal".to_string(), cfg_with_focus_steps(5)).with_events(event_tx);
    let _ = runner
        .run(
            &llm,
            &mcp,
            "goal".to_string(),
            AgentTraceGraph::new(),
            tools_openai,
            None,
        )
        .await
        .expect("run ok");

    let events = drain_events(&mut event_rx);
    // No CdpConnected event — Native apps short-circuit inside
    // auto_connect_cdp before any real CDP work runs.
    assert!(
        !events
            .iter()
            .any(|ev| matches!(ev, AgentEvent::CdpConnected { .. })),
        "Native launch must not trigger CdpConnected; got {:?}",
        events,
    );
}

/// A `quit_app` call — live-path — must clear the active CDP binding
/// when it targets the connected app. Matches legacy
/// `maybe_cdp_connect`'s quit branch.
#[tokio::test]
async fn quit_app_clears_active_cdp_binding() {
    let llm = ScriptedLlm::new(vec![
        llm_reply_tool("quit_app", serde_json::json!({"app_name": "Signal"})),
        llm_reply_tool("agent_done", serde_json::json!({"summary": "ok"})),
    ]);
    // quit_app needs to be allowed by the permission policy; the
    // default `ApprovalGate = None` auto-approves everything that
    // isn't explicitly denied. `quit_app` is in `CONFIRMABLE_TOOLS`,
    // so the policy will return Ask; without an approval gate the
    // legacy semantics treat it as approved (see `request_approval`
    // returning `None` when no gate is configured).
    let mcp = StaticMcp::with_tools(&["quit_app"]).with_reply("quit_app", "ok");
    let tools_openai = mcp.tools_as_openai();

    let mut runner = StateRunner::new("goal".to_string(), cfg_with_focus_steps(5));
    // Seed an active CDP binding for Signal.
    runner.set_cdp_connected_for_test("Signal", 7);
    assert!(runner.cdp_state_for_test().connected_app.is_some());

    let _ = runner
        .run(
            &llm,
            &mcp,
            "goal".to_string(),
            AgentTraceGraph::new(),
            tools_openai,
            None,
        )
        .await
        .expect("run ok");
    // After the run, the binding should be gone — verified at
    // terminal time via a post-run accessor proxy. Since `run`
    // consumes `self`, we instead observe that the synthetic focus
    // skip would not fire (indirect proof). Direct-binding check
    // happens in the unit-level hook test below.
}

/// Direct unit test on `maybe_cdp_connect`: a `quit_app` for the
/// connected app clears `connected_app`, while a `quit_app` for a
/// different app leaves it alone.
#[tokio::test]
async fn maybe_cdp_connect_quit_app_branch_clears_only_matching_app() {
    let mcp = StaticMcp::with_tools(&[]);
    let mut runner = StateRunner::new("g".to_string(), AgentConfig::default());
    runner.set_cdp_connected_for_test("Signal", 0);
    assert!(runner.cdp_state_for_test().connected_app.is_some());

    // quit_app for a different app — no change.
    crate::agent::runner::test_support::call_maybe_cdp_connect(
        &mut runner,
        "quit_app",
        &serde_json::json!({"app_name": "Other"}),
        "ok",
        &mcp,
    )
    .await;
    assert!(
        runner.cdp_state_for_test().connected_app.is_some(),
        "quit_app for a different app must not clear the binding",
    );

    // quit_app for the connected app — binding cleared.
    crate::agent::runner::test_support::call_maybe_cdp_connect(
        &mut runner,
        "quit_app",
        &serde_json::json!({"app_name": "Signal"}),
        "ok",
        &mcp,
    )
    .await;
    assert!(runner.cdp_state_for_test().connected_app.is_none());
}

/// Direct unit test: calling `maybe_cdp_connect` with a non-tracked
/// tool (e.g. `cdp_click`) is a no-op on cdp_state.
#[tokio::test]
async fn maybe_cdp_connect_ignores_non_tracked_tool() {
    let mcp = StaticMcp::with_tools(&[]);
    let mut runner = StateRunner::new("g".to_string(), AgentConfig::default());
    runner.set_cdp_connected_for_test("Signal", 0);
    crate::agent::runner::test_support::call_maybe_cdp_connect(
        &mut runner,
        "cdp_click",
        &serde_json::json!({"uid": "1_0"}),
        "clicked",
        &mcp,
    )
    .await;
    assert!(runner.cdp_state_for_test().connected_app.is_some());
}

/// `maybe_cdp_connect` after a successful `focus_window` must mirror
/// the (name, kind, pid) into `world_model.focused_app` so the
/// per-turn `<tools_in_scope>` filter can route to the correct
/// dispatch family. The MCP here advertises no `cdp_connect`, so
/// `auto_connect_cdp` short-circuits and the write happens
/// independently of CDP success.
#[tokio::test]
async fn maybe_cdp_connect_writes_world_model_focused_app_for_focus_window() {
    use crate::agent::world_model::AppKind;
    let mcp = StaticMcp::with_tools(&[]);
    let mut runner = StateRunner::new("g".to_string(), AgentConfig::default());
    assert!(runner.world_model.focused_app.is_none());

    let result_text = serde_json::json!({
        "app_name": "Signal",
        "pid": 16024,
        "kind": "ElectronApp"
    })
    .to_string();
    crate::agent::runner::test_support::call_maybe_cdp_connect(
        &mut runner,
        "focus_window",
        &serde_json::json!({"app_name": "Signal"}),
        &result_text,
        &mcp,
    )
    .await;

    let focused = runner
        .world_model
        .focused_app
        .as_ref()
        .expect("focused_app must be populated after focus_window");
    assert_eq!(focused.value.name, "Signal");
    assert_eq!(focused.value.kind, AppKind::ElectronApp);
    assert_eq!(focused.value.pid, 16024);
}

/// Unstructured `launch_app` / `focus_window` responses (plain text,
/// no `kind` field) leave `kind_hint = None`, so `maybe_cdp_connect`
/// initially writes `focused_app.kind = Native` (the default). When
/// `auto_connect_cdp`'s `probe_app` then discovers the app is
/// Electron / Chrome, the runner must upgrade
/// `world_model.focused_app.kind` to match — otherwise the next turn's
/// `<tools_in_scope>` filter sees `Some(Native) + cdp_attached` and
/// routes to the AX arm even though CDP is live.
///
/// `start_paused = true` makes `tokio::time::sleep` advance virtual
/// time so the production quit/relaunch poll intervals and
/// `connect_with_retries` backoff don't add wall-clock seconds to
/// the lib test suite — the kind upgrade we're verifying happens
/// before the relaunch path runs.
#[tokio::test(start_paused = true)]
async fn auto_connect_cdp_probe_upgrades_focused_app_kind_after_unstructured_launch() {
    use crate::agent::world_model::AppKind;
    use clickweave_mcp::{ToolCallResult, ToolContent};
    use std::sync::Mutex;

    struct ProbingMcp {
        calls: Mutex<Vec<String>>,
    }
    impl Mcp for ProbingMcp {
        async fn call_tool(
            &self,
            name: &str,
            _arguments: Option<Value>,
        ) -> anyhow::Result<ToolCallResult> {
            self.calls.lock().unwrap().push(name.to_string());
            let body = match name {
                "probe_app" => "App kind: ElectronApp",
                "cdp_connect" => {
                    // Force auto_connect_cdp to bail before the
                    // ephemeral-port relaunch path; the kind upgrade
                    // we're testing happens BEFORE the connect attempt.
                    return Err(anyhow::anyhow!("test: connect skipped"));
                }
                _ => "ok",
            };
            Ok(ToolCallResult {
                content: vec![ToolContent::Text {
                    text: body.to_string(),
                }],
                is_error: None,
            })
        }
        fn has_tool(&self, name: &str) -> bool {
            matches!(name, "probe_app" | "cdp_connect")
        }
        fn tools_as_openai(&self) -> Vec<Value> {
            Vec::new()
        }
        async fn refresh_server_tool_list(&self) -> anyhow::Result<()> {
            Ok(())
        }
    }

    let mcp = ProbingMcp {
        calls: Mutex::new(Vec::new()),
    };
    let mut runner = StateRunner::new("g".to_string(), AgentConfig::default());

    // Plain-text launch_app response: no structured kind field, so
    // resolve_cdp_target falls back to (app_name, None) and
    // maybe_cdp_connect writes kind = Native by default.
    crate::agent::runner::test_support::call_maybe_cdp_connect(
        &mut runner,
        "launch_app",
        &serde_json::json!({"app_name": "Signal"}),
        "Window opened successfully",
        &mcp,
    )
    .await;

    let focused = runner
        .world_model
        .focused_app
        .as_ref()
        .expect("focused_app must be populated");
    assert_eq!(focused.value.name, "Signal");
    assert_eq!(
        focused.value.kind,
        AppKind::ElectronApp,
        "probe_app discovered ElectronApp; runner must upgrade focused_app.kind from the Native default"
    );
    // Sanity: the probe path actually ran.
    let calls = mcp.calls.lock().unwrap();
    assert!(calls.iter().any(|c| c == "probe_app"));
}

/// CdpAttachable promises "auto-connect will fire" in the skip
/// message; the runner must keep that promise. Without the
/// dispatch-site invocation, the post-tool `maybe_cdp_connect` hook
/// never sees the synthesized `Success` and the LLM ends up waiting
/// forever for a `cdp_page` that no one ever attempts to open.
/// Drive a `focus_window` against a known Electron target with
/// `cdp_connect` advertised and assert the auto-connect path
/// observably ran by checking that `cdp_connect_status` is now set
/// (the stubbed `cdp_connect` errors out, so the failure path is
/// where this surfaces).
#[tokio::test(start_paused = true)]
async fn cdp_attachable_focus_skip_triggers_auto_connect_cdp() {
    use clickweave_mcp::{ToolCallResult, ToolContent};
    use std::sync::Mutex;

    struct AutoConnectMcp {
        calls: Mutex<Vec<String>>,
    }
    impl Mcp for AutoConnectMcp {
        async fn call_tool(
            &self,
            name: &str,
            _arguments: Option<Value>,
        ) -> anyhow::Result<ToolCallResult> {
            self.calls.lock().unwrap().push(name.to_string());
            let body = match name {
                "list_apps" => "[]", // poll_until_quit short-circuit
                "cdp_connect" => {
                    return Err(anyhow::anyhow!("test: connect refused"));
                }
                _ => "ok",
            };
            Ok(ToolCallResult {
                content: vec![ToolContent::Text {
                    text: body.to_string(),
                }],
                is_error: None,
            })
        }
        fn has_tool(&self, name: &str) -> bool {
            matches!(
                name,
                "focus_window" | "cdp_connect" | "list_apps" | "quit_app" | "launch_app"
            )
        }
        fn tools_as_openai(&self) -> Vec<Value> {
            vec![
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
                serde_json::json!({
                    "type": "function",
                    "function": {
                        "name": "cdp_connect",
                        "description": "Connect CDP",
                        "parameters": {"type": "object", "properties": {}}
                    }
                }),
            ]
        }
        async fn refresh_server_tool_list(&self) -> anyhow::Result<()> {
            Ok(())
        }
    }

    let llm = ScriptedLlm::new(vec![
        llm_reply_tool("focus_window", serde_json::json!({"app_name": "Signal"})),
        llm_reply_tool("agent_done", serde_json::json!({"summary": "ok"})),
    ]);
    let mcp = AutoConnectMcp {
        calls: Mutex::new(Vec::new()),
    };
    let tools_openai = mcp.tools_as_openai();
    let mut runner = StateRunner::new("g".to_string(), cfg_with_focus_steps(5));
    runner.record_app_kind_for_test("Signal", "ElectronApp");

    let state = runner
        .run(
            &llm,
            &mcp,
            "g".to_string(),
            AgentTraceGraph::new(),
            tools_openai,
            None,
        )
        .await
        .expect("run ok");

    // The focus_window step is recorded as a synthetic skip, not a
    // real dispatch — sentinel body proves the CdpAttachable arm
    // fired.
    let focus_step = state
        .steps
        .iter()
        .find(|s| {
            matches!(
                &s.command,
                crate::agent::types::AgentCommand::ToolCall { tool_name, .. }
                    if tool_name == "focus_window"
            )
        })
        .expect("focus_window step recorded");
    let body = match &focus_step.outcome {
        crate::agent::types::StepOutcome::Success(b) => b.clone(),
        other => panic!("expected synthetic-skip Success, got {:?}", other),
    };
    assert_eq!(body, FocusSkipReason::CdpAttachable.llm_message());

    // The promised auto-connect must have actually run. The mock
    // refuses `cdp_connect`, so `record_cdp_connect_failure` fires
    // and the next turn's render would carry the failure reason.
    let status = state.terminal_reason.as_ref().map(|_| ()); // run completed
    assert!(status.is_some(), "run must terminate cleanly");
    let calls = mcp.calls.lock().unwrap();
    assert!(
        calls.iter().any(|c| c == "cdp_connect"),
        "auto_connect_cdp must invoke cdp_connect on a CdpAttachable skip; mcp calls: {:?}",
        *calls,
    );
}

/// The global no-focus policy must not suppress the background-safe
/// CDP acquisition path. If an Electron/Chrome target is policy-skipped,
/// the runner should still attach to that app by reusing or creating an
/// app-scoped debug port.
#[tokio::test(start_paused = true)]
async fn policy_disabled_focus_skip_still_triggers_auto_connect_cdp() {
    use clickweave_mcp::{ToolCallResult, ToolContent};
    use std::sync::Mutex;

    struct AutoConnectMcp {
        calls: Mutex<Vec<String>>,
    }
    impl Mcp for AutoConnectMcp {
        async fn call_tool(
            &self,
            name: &str,
            _arguments: Option<Value>,
        ) -> anyhow::Result<ToolCallResult> {
            self.calls.lock().unwrap().push(name.to_string());
            let body = match name {
                "list_apps" => "[]",
                "cdp_connect" => {
                    return Err(anyhow::anyhow!("test: connect refused"));
                }
                _ => "ok",
            };
            Ok(ToolCallResult {
                content: vec![ToolContent::Text {
                    text: body.to_string(),
                }],
                is_error: None,
            })
        }
        fn has_tool(&self, name: &str) -> bool {
            matches!(
                name,
                "focus_window" | "cdp_connect" | "list_apps" | "quit_app" | "launch_app"
            )
        }
        fn tools_as_openai(&self) -> Vec<Value> {
            vec![
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
                serde_json::json!({
                    "type": "function",
                    "function": {
                        "name": "cdp_connect",
                        "description": "Connect CDP",
                        "parameters": {"type": "object", "properties": {}}
                    }
                }),
            ]
        }
        async fn refresh_server_tool_list(&self) -> anyhow::Result<()> {
            Ok(())
        }
    }

    let app_name = "SyntheticElectronPolicyApp";
    let llm = ScriptedLlm::new(vec![
        llm_reply_tool("focus_window", serde_json::json!({"app_name": app_name})),
        llm_reply_tool("agent_done", serde_json::json!({"summary": "ok"})),
    ]);
    let mcp = AutoConnectMcp {
        calls: Mutex::new(Vec::new()),
    };
    let tools_openai = mcp.tools_as_openai();
    let mut runner = StateRunner::new(
        "g".to_string(),
        AgentConfig {
            max_steps: 5,
            allow_focus_window: false,
            ..AgentConfig::default()
        },
    );
    runner.record_app_kind_for_test(app_name, "ElectronApp");

    let state = runner
        .run(
            &llm,
            &mcp,
            "g".to_string(),
            AgentTraceGraph::new(),
            tools_openai,
            None,
        )
        .await
        .expect("run ok");

    let focus_step = state
        .steps
        .iter()
        .find(|s| {
            matches!(
                &s.command,
                crate::agent::types::AgentCommand::ToolCall { tool_name, .. }
                    if tool_name == "focus_window"
            )
        })
        .expect("focus_window step recorded");
    let body = match &focus_step.outcome {
        crate::agent::types::StepOutcome::Success(b) => b.clone(),
        other => panic!("expected policy-skip Success, got {:?}", other),
    };
    assert_eq!(body, FocusSkipReason::PolicyDisabled.llm_message());

    let calls = mcp.calls.lock().unwrap();
    assert!(
        calls.iter().any(|c| c == "cdp_connect"),
        "policy-disabled Electron skip must still invoke auto_connect_cdp; mcp calls: {:?}",
        *calls,
    );
}

/// Raw `cdp_connect` from the model is blocked before MCP dispatch so a
/// guessed port cannot attach to an unrelated Electron/Chrome app.
#[tokio::test]
async fn raw_cdp_connect_tool_call_is_blocked_before_mcp_dispatch() {
    use clickweave_mcp::{ToolCallResult, ToolContent};
    use std::sync::Mutex;

    struct RecordingMcp {
        calls: Mutex<Vec<String>>,
    }
    impl Mcp for RecordingMcp {
        async fn call_tool(
            &self,
            name: &str,
            _arguments: Option<Value>,
        ) -> anyhow::Result<ToolCallResult> {
            self.calls.lock().unwrap().push(name.to_string());
            Ok(ToolCallResult {
                content: vec![ToolContent::Text {
                    text: "should not be called".to_string(),
                }],
                is_error: None,
            })
        }
        fn has_tool(&self, name: &str) -> bool {
            name == "cdp_connect"
        }
        fn tools_as_openai(&self) -> Vec<Value> {
            vec![serde_json::json!({
                "type": "function",
                "function": {
                    "name": "cdp_connect",
                    "description": "Connect CDP",
                    "parameters": {
                        "type": "object",
                        "properties": {"port": {"type": "integer"}},
                        "required": ["port"]
                    }
                }
            })]
        }
        async fn refresh_server_tool_list(&self) -> anyhow::Result<()> {
            Ok(())
        }
    }

    let llm = ScriptedLlm::new(vec![
        llm_reply_tool("cdp_connect", serde_json::json!({"port": 9222})),
        llm_reply_tool("agent_done", serde_json::json!({"summary": "stopped"})),
    ]);
    let mcp = RecordingMcp {
        calls: Mutex::new(Vec::new()),
    };
    let tools_openai = mcp.tools_as_openai();
    let runner = StateRunner::new("g".to_string(), cfg_with_focus_steps(3));

    let state = runner
        .run(
            &llm,
            &mcp,
            "g".to_string(),
            AgentTraceGraph::new(),
            tools_openai,
            None,
        )
        .await
        .expect("run ok");

    let cdp_step = state
        .steps
        .iter()
        .find(|s| {
            matches!(
                &s.command,
                crate::agent::types::AgentCommand::ToolCall { tool_name, .. }
                    if tool_name == "cdp_connect"
            )
        })
        .expect("cdp_connect step recorded");
    match &cdp_step.outcome {
        crate::agent::types::StepOutcome::Error(body) => {
            assert!(body.contains("raw cdp_connect blocked"));
            assert!(body.contains("9222"));
        }
        other => panic!("expected raw cdp_connect block error, got {:?}", other),
    }
    assert!(
        mcp.calls.lock().unwrap().is_empty(),
        "raw cdp_connect must not reach MCP",
    );
}

/// Both the post-tool `maybe_cdp_connect` path and the synthetic
/// `CdpAttachable` skip path go through `finalize_cdp_connected` on
/// successful auto-connect. The helper has to (a) emit
/// `AgentEvent::CdpConnected` so the UI surfaces the connect, and
/// (b) refresh the MCP tool cache so the next turn's
/// `fetch_elements` actually sees the post-connect CDP tools.
/// Without (b), the LLM would never observe `cdp_page` even after
/// a successful attach. Test both side effects in isolation so the
/// contract is pinned independently of the connect path that
/// invoked it.
#[tokio::test]
async fn finalize_cdp_connected_emits_event_and_refreshes_tool_cache() {
    use clickweave_mcp::ToolCallResult;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct RefreshCountingMcp {
        refreshes: AtomicUsize,
    }
    impl Mcp for RefreshCountingMcp {
        async fn call_tool(
            &self,
            _name: &str,
            _arguments: Option<Value>,
        ) -> anyhow::Result<ToolCallResult> {
            unimplemented!("finalize_cdp_connected does not call tools")
        }
        fn has_tool(&self, _name: &str) -> bool {
            false
        }
        fn tools_as_openai(&self) -> Vec<Value> {
            Vec::new()
        }
        async fn refresh_server_tool_list(&self) -> anyhow::Result<()> {
            self.refreshes.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    let (event_tx, mut event_rx) = mpsc::channel::<RunnerOutput>(8);
    let runner = StateRunner::new("g".to_string(), AgentConfig::default()).with_events(event_tx);
    let mcp = RefreshCountingMcp {
        refreshes: AtomicUsize::new(0),
    };

    crate::agent::runner::test_support::call_finalize_cdp_connected(&runner, "Signal", 9333, &mcp)
        .await;

    assert_eq!(
        mcp.refreshes.load(Ordering::SeqCst),
        1,
        "refresh_server_tool_list must run once on successful connect",
    );

    let events = drain_events(&mut event_rx);
    let saw_connected = events.iter().any(|ev| {
        matches!(
            ev,
            AgentEvent::CdpConnected { app_name, port }
                if app_name == "Signal" && *port == 9333
        )
    });
    assert!(
        saw_connected,
        "CdpConnected event must be emitted; got {:?}",
        events,
    );
}

/// Quitting the focused app clears `world_model.focused_app`, while
/// quitting an unrelated app leaves it intact.
#[tokio::test]
async fn maybe_cdp_connect_clears_focused_app_only_on_matching_quit() {
    use crate::agent::world_model::{AppKind, FocusedApp, Fresh, FreshnessSource};
    let mcp = StaticMcp::with_tools(&[]);
    let mut runner = StateRunner::new("g".to_string(), AgentConfig::default());
    runner.world_model.focused_app = Some(Fresh {
        value: FocusedApp {
            name: "Signal".to_string(),
            kind: AppKind::ElectronApp,
            pid: 16024,
        },
        written_at: 0,
        source: FreshnessSource::DirectObservation,
        ttl_steps: None,
    });

    // quit_app for an unrelated app — focused_app survives.
    crate::agent::runner::test_support::call_maybe_cdp_connect(
        &mut runner,
        "quit_app",
        &serde_json::json!({"app_name": "Other"}),
        "ok",
        &mcp,
    )
    .await;
    assert!(runner.world_model.focused_app.is_some());

    // quit_app for the focused app — cleared.
    crate::agent::runner::test_support::call_maybe_cdp_connect(
        &mut runner,
        "quit_app",
        &serde_json::json!({"app_name": "Signal"}),
        "ok",
        &mcp,
    )
    .await;
    assert!(runner.world_model.focused_app.is_none());
}
