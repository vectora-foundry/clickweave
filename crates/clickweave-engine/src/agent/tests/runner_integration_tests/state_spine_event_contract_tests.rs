use super::super::super::test_stubs::{ScriptedLlm, StaticMcp, llm_reply_tool};
use crate::agent::runner::StateRunner;
use crate::agent::step_record::BoundaryKind;
use crate::agent::task_state::Phase;
use crate::agent::types::{AgentConfig, AgentEvent, RunnerOutput};
use crate::executor::Mcp;
use clickweave_llm::{CallType, ChatResponse, Choice, FunctionCall, Message, ToolCall};
use serde_json::Value;
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

fn tc(id: &str, name: &str, arguments: Value) -> ToolCall {
    ToolCall {
        id: id.to_string(),
        call_type: CallType::Function,
        function: FunctionCall {
            name: name.to_string(),
            arguments,
        },
    }
}

fn llm_reply_tools(id: &str, calls: Vec<ToolCall>) -> ChatResponse {
    ChatResponse {
        id: id.to_string(),
        choices: vec![Choice {
            index: 0,
            message: Message::assistant_tool_calls(calls),
            finish_reason: Some("tool_calls".to_string()),
        }],
        usage: None,
    }
}

/// A scripted run through `StateRunner::run` emits
/// `AgentEvent::WorldModelChanged` with the runner's `run_id`
/// stamped on every step (D17). `WorldModelChanged` must fire
/// once per step after `observe` — the scripted scenario runs
/// two tool calls and terminates on `agent_done`, so at least
/// one `WorldModelChanged` event must be observed.
#[tokio::test]
async fn world_model_changed_event_fires_per_step_with_run_id() {
    let llm = ScriptedLlm::new(vec![
        llm_reply_tool(
            "cdp_find_elements",
            serde_json::json!({"query": "", "max_results": 300}),
        ),
        llm_reply_tool("cdp_click", serde_json::json!({"uid": "1_0"})),
        llm_reply_tool("agent_done", serde_json::json!({"summary": "ok"})),
    ]);
    let mcp = StaticMcp::with_tools(&["cdp_find_elements", "cdp_click"])
        .with_reply(
            "cdp_find_elements",
            r#"{"page_url":"about:blank","source":"cdp","matches":[]}"#,
        )
        .with_reply("cdp_click", "clicked");

    let run_id = uuid::Uuid::new_v4();
    let (event_tx, mut event_rx) = mpsc::channel::<RunnerOutput>(32);
    let runner = StateRunner::new("goal".to_string(), AgentConfig::default())
        .with_run_id(run_id)
        .with_events(event_tx);

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
    let world_events: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::WorldModelChanged { run_id: rid, diff } => {
                Some((*rid, diff.changed_fields.clone()))
            }
            _ => None,
        })
        .collect();
    assert!(
        !world_events.is_empty(),
        "at least one WorldModelChanged event must fire across a multi-step run; events={:?}",
        events,
    );
    for (rid, _fields) in &world_events {
        assert_eq!(
            *rid, run_id,
            "every WorldModelChanged event must carry the runner's run_id",
        );
    }
}

/// Direct-observation writes the top-level `run` loop performs after
/// `fetch_cdp_page_summary` (populating `world_model.cdp_page`) must
/// surface in `WorldModelChanged.changed_fields`. Without the pre-mirror
/// signature capture these writes happen before `run_turn` snapshots
/// pre/post signatures, so the diff would silently report no change
/// on the very turn that changed the rendered state block.
#[tokio::test]
async fn world_model_changed_reports_cdp_page_on_first_observation() {
    let llm = ScriptedLlm::new(vec![
        llm_reply_tool(
            "cdp_find_elements",
            serde_json::json!({"query": "", "max_results": 300}),
        ),
        llm_reply_tool("agent_done", serde_json::json!({"summary": "ok"})),
    ]);
    // Page URL + inventory → first-turn mirror flips
    // `world_model.cdp_page` from None to Some. CDP element candidates
    // are no longer mirrored into `world_model.elements`; they stay in
    // explicit `cdp_find_elements` tool results.
    let summary_body = r#"{"page_url":"https://example.com","source":"dom_summary","inventory":[{"role":"button","count":1,"sample_labels":["Submit"]}]}"#;
    let find_body = r#"{"page_url":"https://example.com","source":"cdp","matches":[{"uid":"1_0","role":"button","label":"Submit","tag":"button","disabled":false,"parent_role":null,"parent_name":null}]}"#;
    let mcp = StaticMcp::with_tools(&["cdp_summarize_page", "cdp_find_elements"])
        .with_reply("cdp_summarize_page", summary_body)
        .with_reply("cdp_find_elements", find_body);

    let run_id = uuid::Uuid::new_v4();
    let (event_tx, mut event_rx) = mpsc::channel::<RunnerOutput>(32);
    let runner = StateRunner::new("goal".to_string(), AgentConfig::default())
        .with_run_id(run_id)
        .with_events(event_tx);

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
    let changed_fields_sets: Vec<Vec<String>> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::WorldModelChanged { diff, .. } => Some(diff.changed_fields.clone()),
            _ => None,
        })
        .collect();
    assert!(
        !changed_fields_sets
            .iter()
            .any(|cf| cf.iter().any(|f| f == "elements")),
        "automatic CDP summary must not report `elements` changes; got {:?}",
        changed_fields_sets,
    );
    assert!(
        changed_fields_sets
            .iter()
            .any(|cf| cf.iter().any(|f| f == "cdp_page")),
        "some WorldModelChanged event must report `cdp_page` in changed_fields \
             after the per-turn mirror block sets world_model.cdp_page; got {:?}",
        changed_fields_sets,
    );
}

/// A run that terminates on `agent_done` emits exactly one
/// `AgentEvent::BoundaryRecordWritten { Terminal, .. }` with the
/// runner's `run_id` (D17). The Terminal boundary write is the
/// canonical terminal gate, so a missing event here means the
/// Tauri-visible event seam dropped the last boundary signal.
#[tokio::test]
async fn boundary_record_written_fires_for_terminal_with_run_id() {
    let llm = ScriptedLlm::new(vec![
        llm_reply_tool(
            "cdp_find_elements",
            serde_json::json!({"query": "", "max_results": 300}),
        ),
        llm_reply_tool("agent_done", serde_json::json!({"summary": "ok"})),
    ]);
    let mcp = StaticMcp::with_tools(&["cdp_find_elements"]).with_reply(
        "cdp_find_elements",
        r#"{"page_url":"about:blank","source":"cdp","matches":[]}"#,
    );

    let run_id = uuid::Uuid::new_v4();
    let (event_tx, mut event_rx) = mpsc::channel::<RunnerOutput>(32);
    let runner = StateRunner::new("goal".to_string(), AgentConfig::default())
        .with_run_id(run_id)
        .with_events(event_tx);

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
    let terminal_events: Vec<_> = events
        .iter()
        .filter(|e| {
            matches!(
                e,
                AgentEvent::BoundaryRecordWritten {
                    boundary_kind: BoundaryKind::Terminal,
                    ..
                }
            )
        })
        .collect();
    assert_eq!(
        terminal_events.len(),
        1,
        "exactly one Terminal BoundaryRecordWritten event expected; events={:?}",
        events,
    );
    match terminal_events[0] {
        AgentEvent::BoundaryRecordWritten {
            run_id: rid,
            boundary_kind: BoundaryKind::Terminal,
            milestone_text,
            ..
        } => {
            assert_eq!(
                *rid, run_id,
                "BoundaryRecordWritten must carry the runner's run_id",
            );
            assert_eq!(
                milestone_text, &None,
                "Terminal boundary events must not carry milestone text",
            );
        }
        other => panic!("unreachable — filtered above; got {:?}", other),
    }
}

#[tokio::test]
async fn boundary_record_written_carries_milestone_text_for_completed_subgoals() {
    let llm = ScriptedLlm::new(vec![
        llm_reply_tools(
            "scripted-push-subgoals",
            vec![
                tc("m1", "push_subgoal", serde_json::json!({"text": "A"})),
                tc("m2", "push_subgoal", serde_json::json!({"text": "B"})),
                tc(
                    "a1",
                    "cdp_find_elements",
                    serde_json::json!({"query": "", "max_results": 300}),
                ),
            ],
        ),
        llm_reply_tools(
            "scripted-complete-subgoals",
            vec![
                tc(
                    "m3",
                    "complete_subgoal",
                    serde_json::json!({"summary": "did B"}),
                ),
                tc(
                    "m4",
                    "complete_subgoal",
                    serde_json::json!({"summary": "did A"}),
                ),
                tc("a2", "agent_done", serde_json::json!({"summary": "ok"})),
            ],
        ),
    ]);
    let mcp = StaticMcp::with_tools(&["cdp_find_elements"]).with_reply(
        "cdp_find_elements",
        r#"{"page_url":"about:blank","source":"cdp","matches":[]}"#,
    );

    let run_id = uuid::Uuid::new_v4();
    let (event_tx, mut event_rx) = mpsc::channel::<RunnerOutput>(32);
    let runner = StateRunner::new("goal".to_string(), AgentConfig::default())
        .with_run_id(run_id)
        .with_events(event_tx);

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
    let completed_texts: Vec<Option<String>> = events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::BoundaryRecordWritten {
                boundary_kind: BoundaryKind::SubgoalCompleted,
                milestone_text,
                ..
            } => Some(milestone_text.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        completed_texts,
        vec![Some("B".to_string()), Some("A".to_string())],
        "each CompleteSubgoal boundary must carry the matching milestone text; events={:?}",
        events,
    );
}

/// Driving `StateRunner::run_turn` directly with a turn that pushes the
/// first subgoal and dispatches an action emits `TaskStateChanged` twice:
/// once for the applied mutation, then once after `observe` re-infers the
/// phase as `Executing`. A later turn with no mutations and no phase shift
/// must not emit another one.
#[tokio::test]
async fn task_state_changed_reemits_when_observe_shifts_phase() {
    use crate::agent::runner::{AgentAction, AgentTurn, ToolExecutor};
    use crate::agent::task_state::TaskStateMutation;
    use async_trait::async_trait;

    struct FixedOk;

    #[async_trait]
    impl ToolExecutor for FixedOk {
        async fn call_tool(
            &self,
            _tool_name: &str,
            _arguments: &serde_json::Value,
        ) -> Result<String, String> {
            Ok("ok".to_string())
        }
    }

    let run_id = uuid::Uuid::new_v4();
    let (event_tx, mut event_rx) = mpsc::channel::<RunnerOutput>(32);
    let mut runner = StateRunner::new("goal".to_string(), AgentConfig::default())
        .with_run_id(run_id)
        .with_events(event_tx);

    // Turn 1: mutation-only push. Expect TaskStateChanged.
    let turn_push = AgentTurn {
        mutations: vec![TaskStateMutation::PushSubgoal {
            text: "A".to_string(),
        }],
        action: AgentAction::ToolCall {
            tool_name: "cdp_click".to_string(),
            arguments: serde_json::json!({}),
            tool_call_id: "tc-1".to_string(),
        },
    };
    let _ = runner.run_turn(&turn_push, &FixedOk).await;

    // Turn 2: no mutations. Must NOT emit TaskStateChanged.
    let turn_bare = AgentTurn {
        mutations: vec![],
        action: AgentAction::ToolCall {
            tool_name: "cdp_click".to_string(),
            arguments: serde_json::json!({}),
            tool_call_id: "tc-2".to_string(),
        },
    };
    let _ = runner.run_turn(&turn_bare, &FixedOk).await;

    let events = drain_events(&mut event_rx);
    let task_state_events: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::TaskStateChanged {
                run_id: rid,
                task_state,
            } => Some((*rid, task_state.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(
        task_state_events.len(),
        2,
        "exactly two TaskStateChanged events expected: mutation snapshot plus \
             post-observe phase snapshot; events={:?}",
        events,
    );
    assert!(
        task_state_events.iter().all(|(rid, _)| *rid == run_id),
        "TaskStateChanged must carry the runner's run_id",
    );
    assert_eq!(
        task_state_events[0].1.subgoal_stack.len(),
        1,
        "task_state payload must reflect the post-mutation stack depth",
    );
    assert_eq!(
        task_state_events[0].1.phase,
        Phase::Exploring,
        "the first TaskStateChanged event is the immediate post-mutation snapshot",
    );
    assert_eq!(
        task_state_events[1].1.phase,
        Phase::Executing,
        "the second TaskStateChanged event must carry the dispatch-time phase",
    );
}
