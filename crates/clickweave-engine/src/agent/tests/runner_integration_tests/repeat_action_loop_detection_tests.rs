use super::super::super::test_stubs::{ScriptedLlm, StaticMcp, llm_reply_tool};
use crate::agent::runner::{NO_PROGRESS_WARNING_PREFIX, StateRunner};
use crate::agent::types::{AgentConfig, AgentEvent, RunnerOutput};
use crate::executor::Mcp;
use tokio::sync::mpsc;

fn drain_events(rx: &mut mpsc::Receiver<RunnerOutput>) -> Vec<AgentEvent> {
    let mut out = Vec::new();
    while let Ok(ev) = rx.try_recv() {
        if let Some(event) = ev.into_event() {
            out.push(event);
        }
    }
    out
}

fn cfg(steps: usize) -> AgentConfig {
    AgentConfig {
        max_steps: steps,
        ..AgentConfig::default()
    }
}

fn count_no_progress(events: &[AgentEvent]) -> usize {
    events
            .iter()
            .filter(|ev| {
                matches!(ev, AgentEvent::Warning { message } if message.starts_with(NO_PROGRESS_WARNING_PREFIX))
            })
            .count()
}

async fn run_scenario(
    scripted: Vec<clickweave_llm::ChatResponse>,
    mcp: StaticMcp,
    max_steps: usize,
) -> Vec<AgentEvent> {
    let llm = ScriptedLlm::new(scripted);
    let (event_tx, mut event_rx) = mpsc::channel::<RunnerOutput>(64);
    let runner = StateRunner::new("goal".to_string(), cfg(max_steps)).with_events(event_tx);
    let tools = mcp.tools_as_openai();
    let _ = runner
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
    drain_events(&mut event_rx)
}

#[tokio::test]
async fn three_identical_action_calls_emit_no_progress_warning() {
    let same = || llm_reply_tool("cdp_click", serde_json::json!({"uid": "1_0"}));
    let mcp = StaticMcp::with_tools(&["cdp_click"]).with_reply("cdp_click", "clicked");
    let events = run_scenario(
        vec![
            same(),
            same(),
            same(),
            same(),
            llm_reply_tool("agent_done", serde_json::json!({"summary": "ok"})),
        ],
        mcp,
        8,
    )
    .await;

    assert!(
        count_no_progress(&events) >= 2,
        "third and fourth identical successful action must each emit a \
             no-progress warning; events={:?}",
        events,
    );
}

#[tokio::test]
async fn divergent_action_resets_repeat_counter() {
    let mcp = StaticMcp::with_tools(&["cdp_click"]).with_reply("cdp_click", "clicked");
    let events = run_scenario(
        vec![
            llm_reply_tool("cdp_click", serde_json::json!({"uid": "1_0"})),
            llm_reply_tool("cdp_click", serde_json::json!({"uid": "1_0"})),
            llm_reply_tool("cdp_click", serde_json::json!({"uid": "2_0"})),
            llm_reply_tool("cdp_click", serde_json::json!({"uid": "1_0"})),
            llm_reply_tool("cdp_click", serde_json::json!({"uid": "1_0"})),
            llm_reply_tool("agent_done", serde_json::json!({"summary": "ok"})),
        ],
        mcp,
        10,
    )
    .await;

    assert_eq!(
        count_no_progress(&events),
        0,
        "divergent intermediate call must reset the repeat counter; events={:?}",
        events,
    );
}

#[tokio::test]
async fn alternating_action_cycle_emits_no_progress_warning() {
    let fill = || {
        llm_reply_tool(
            "cdp_fill",
            serde_json::json!({"uid": "d-search", "value": "synthetic-channel"}),
        )
    };
    let cancel = || llm_reply_tool("cdp_click", serde_json::json!({"uid": "d-cancel"}));
    let mcp = StaticMcp::with_tools(&["cdp_fill", "cdp_click"])
        .with_reply("cdp_fill", "filled synthetic field")
        .with_reply("cdp_click", "clicked synthetic cancel");
    let events = run_scenario(
        vec![
            fill(),
            cancel(),
            fill(),
            cancel(),
            llm_reply_tool("agent_done", serde_json::json!({"summary": "ok"})),
        ],
        mcp,
        8,
    )
    .await;

    assert!(
        count_no_progress(&events) >= 1,
        "repeated two-action cycle must emit a no-progress warning; events={:?}",
        events,
    );
}

#[tokio::test]
async fn three_action_cycle_emits_no_progress_warning() {
    let fill = || {
        llm_reply_tool(
            "cdp_fill",
            serde_json::json!({"uid": "d-search", "value": "synthetic-channel"}),
        )
    };
    let filter = || llm_reply_tool("cdp_click", serde_json::json!({"uid": "d-filter"}));
    let cancel = || llm_reply_tool("cdp_click", serde_json::json!({"uid": "d-cancel"}));
    let mcp = StaticMcp::with_tools(&["cdp_fill", "cdp_click"])
        .with_reply("cdp_fill", "filled synthetic field")
        .with_reply("cdp_click", "clicked synthetic control");
    let events = run_scenario(
        vec![
            fill(),
            filter(),
            cancel(),
            fill(),
            filter(),
            cancel(),
            llm_reply_tool("agent_done", serde_json::json!({"summary": "ok"})),
        ],
        mcp,
        10,
    )
    .await;

    assert!(
        count_no_progress(&events) >= 1,
        "repeated three-action cycle must emit a no-progress warning; events={:?}",
        events,
    );
}

/// Identical successful live dispatches must trip the no-progress
/// detector once they cross the repeat threshold.
#[tokio::test]
async fn live_repeated_dispatch_emits_no_progress_warning() {
    let same = || llm_reply_tool("cdp_click", serde_json::json!({"uid": "1_0"}));
    let mcp = StaticMcp::with_tools(&["cdp_find_elements", "cdp_click"])
            .with_reply(
                "cdp_find_elements",
                r#"{"page_url":"about:blank","source":"cdp","matches":[{"uid":"1_0","role":"button","label":"Submit","tag":"button","disabled":false,"parent_role":null,"parent_name":null}]}"#,
            )
            .with_reply("cdp_click", "clicked");

    let cfg = AgentConfig {
        max_steps: 8,
        ..AgentConfig::default()
    };
    let llm = ScriptedLlm::new(vec![
        same(),
        same(),
        same(),
        llm_reply_tool("agent_done", serde_json::json!({"summary": "ok"})),
    ]);
    let (event_tx, mut event_rx) = mpsc::channel::<RunnerOutput>(64);
    let runner = StateRunner::new("goal".to_string(), cfg).with_events(event_tx);
    let tools = mcp.tools_as_openai();
    let _ = runner
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

    let events = drain_events(&mut event_rx);
    assert!(
        count_no_progress(&events) >= 1,
        "repeated live dispatches must contribute to the repeat-action streak; events={:?}",
        events,
    );
}

/// Regression: a non-dispatched action between identical successful
/// dispatches must break the streak. Without this, two cdp_click(A)
/// calls + a denied cdp_fill + another cdp_click(A) would still trip
/// the threshold even though the click was not actually emitted three
/// turns in a row.
#[tokio::test]
async fn denied_intervening_action_resets_repeat_counter() {
    use crate::agent::permissions::{PermissionAction, PermissionPolicy, PermissionRule};

    let click = || llm_reply_tool("cdp_click", serde_json::json!({"uid": "1_0"}));
    let fill = || llm_reply_tool("cdp_fill", serde_json::json!({"uid": "1_0", "value": "x"}));
    let mcp = StaticMcp::with_tools(&["cdp_click", "cdp_fill"])
        .with_reply("cdp_click", "clicked")
        .with_reply("cdp_fill", "filled");

    // Deny `cdp_fill` so the middle turn takes the policy-deny early-exit
    // path that records an error step + `continue`s without invoking
    // `run_turn`.
    let policy = PermissionPolicy {
        rules: vec![PermissionRule {
            tool_pattern: "cdp_fill".to_string(),
            args_pattern: None,
            action: PermissionAction::Deny,
        }],
        ..PermissionPolicy::default()
    };

    let cfg_with_room = AgentConfig {
        max_steps: 8,
        // Allow several errors in a row so the deny doesn't terminate the run.
        max_consecutive_errors: 5,
        ..AgentConfig::default()
    };
    let llm = ScriptedLlm::new(vec![
        click(),
        click(),
        fill(),
        click(),
        llm_reply_tool("agent_done", serde_json::json!({"summary": "ok"})),
    ]);
    let (event_tx, mut event_rx) = mpsc::channel::<RunnerOutput>(64);
    let runner = StateRunner::new("goal".to_string(), cfg_with_room)
        .with_events(event_tx)
        .with_permissions(policy);
    let tools = mcp.tools_as_openai();
    let _ = runner
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

    let events = drain_events(&mut event_rx);
    assert_eq!(
        count_no_progress(&events),
        0,
        "denied intermediate dispatch must reset the streak; events={:?}",
        events,
    );
}

#[tokio::test]
async fn repeated_observation_tool_does_not_emit_warning() {
    let obs = || {
        llm_reply_tool(
            "cdp_find_elements",
            serde_json::json!({"query": "", "max_results": 300}),
        )
    };
    let mcp = StaticMcp::with_tools(&["cdp_find_elements"]).with_reply(
        "cdp_find_elements",
        r#"{"page_url":"about:blank","source":"cdp","matches":[]}"#,
    );
    let events = run_scenario(
        vec![
            obs(),
            obs(),
            obs(),
            obs(),
            llm_reply_tool("agent_done", serde_json::json!({"summary": "ok"})),
        ],
        mcp,
        8,
    )
    .await;

    assert_eq!(
        count_no_progress(&events),
        0,
        "observation-only tools must be exempt; events={:?}",
        events,
    );
}

#[tokio::test]
async fn repeated_send_search_after_text_input_emits_no_progress_warning() {
    let mcp = StaticMcp::with_tools(&["cdp_fill", "cdp_find_elements", "cdp_press_key"])
            .with_reply(
                "cdp_fill",
                "Filled uid=d1 'Message' (textbox) with 'hello' (strategy=rich_editor_keyboard, observed_text=true)",
            )
            .with_reply(
                "cdp_find_elements",
                r#"{"page_url":"about:blank","source":"cdp","matches":[],"inventory":[]}"#,
            );

    let events = run_scenario(
        vec![
            llm_reply_tool(
                "cdp_fill",
                serde_json::json!({"uid": "d1", "value": "hello"}),
            ),
            llm_reply_tool(
                "cdp_find_elements",
                serde_json::json!({"query": "Send", "role": "button"}),
            ),
            llm_reply_tool("cdp_find_elements", serde_json::json!({"query": "send"})),
            llm_reply_tool(
                "cdp_find_elements",
                serde_json::json!({"query": "send button", "role": "button"}),
            ),
            llm_reply_tool("agent_done", serde_json::json!({"summary": "ok"})),
        ],
        mcp,
        8,
    )
    .await;

    assert!(
        count_no_progress(&events) >= 1,
        "repeated send searches after composing text must emit a no-progress warning; events={:?}",
        events,
    );
}
