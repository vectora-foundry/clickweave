use super::super::super::test_stubs::{ScriptedLlm, StaticMcp, llm_reply_tool};
use crate::agent::runner::StateRunner;
use crate::agent::step_record::BoundaryKind;
use crate::agent::types::AgentConfig;
use crate::executor::Mcp;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// Attach a fresh `RunStorage` to a `StateRunner` and return the
/// path to the execution-level `events.jsonl` the runner will
/// append boundary records to.
fn setup_runner_with_storage(
    runner: StateRunner,
    workflow_name: &str,
) -> (StateRunner, tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().expect("tempdir");
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
    let runner = runner.with_storage(storage);
    (runner, tmp, events_path)
}

/// Read the boundary records from the execution-level `events.jsonl`.
/// Returns the parsed `StepRecord`s (not every line is a StepRecord —
/// the file can carry other agent events — so the parse is best-effort
/// and skips lines that don't deserialize).
fn read_boundary_records(events_path: &std::path::Path) -> Vec<serde_json::Value> {
    let contents = std::fs::read_to_string(events_path).unwrap_or_default();
    contents
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .filter(|v| v.get("boundary_kind").is_some())
        .collect()
}

/// Count records with a given `boundary_kind` tag.
fn count_of(records: &[serde_json::Value], kind: &str) -> usize {
    records
        .iter()
        .filter(|r| r.get("boundary_kind").and_then(|k| k.as_str()) == Some(kind))
        .count()
}

#[tokio::test]
async fn terminal_boundary_record_written_once_on_agent_done() {
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

    let runner = StateRunner::new("goal".to_string(), AgentConfig::default());
    let (runner, _tmp, events_path) = setup_runner_with_storage(runner, "term-test");
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

    let records = read_boundary_records(&events_path);
    assert_eq!(
        count_of(&records, "terminal"),
        1,
        "exactly one Terminal record expected on agent_done; got records={:?}",
        records,
    );
}

#[tokio::test]
async fn terminal_boundary_record_written_once_on_max_steps() {
    // Pathological: LLM loops on `cdp_find_elements` forever.
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

    let cfg = AgentConfig {
        max_steps: 3,
        ..AgentConfig::default()
    };
    let runner = StateRunner::new("goal".to_string(), cfg);
    let (runner, _tmp, events_path) = setup_runner_with_storage(runner, "maxsteps-test");
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

    let records = read_boundary_records(&events_path);
    assert_eq!(
        count_of(&records, "terminal"),
        1,
        "exactly one Terminal record expected on max_steps; got records={:?}",
        records,
    );
}

#[tokio::test]
async fn subgoal_completed_writes_one_record_per_completion() {
    // Drive the boundary write through `run_turn` directly — the
    // scripted LLM path does not yet parse pseudo-tool mutation names
    // out of tool_calls (that's Task 3a.2's TODO), so building the
    // turns inline is the shortest path to asserting mutation-driven
    // persistence.
    use super::ScriptedExecutor;
    use crate::agent::runner::{AgentAction, AgentTurn};
    use crate::agent::task_state::TaskStateMutation;

    let runner = StateRunner::new_for_test("goal".to_string());
    let (mut runner, _tmp, events_path) = setup_runner_with_storage(runner, "subgoal-test");

    let exec = ScriptedExecutor::new(vec![Ok("ok".to_string()), Ok("ok".to_string())]);

    // Turn 1: push subgoal A + tool call.
    let t1 = AgentTurn {
        mutations: vec![TaskStateMutation::PushSubgoal {
            text: "A".to_string(),
        }],
        action: AgentAction::ToolCall {
            tool_name: "cdp_click".to_string(),
            arguments: serde_json::json!({}),
            tool_call_id: "tc-1".to_string(),
        },
    };
    // Helper: mirror what `run()` does at the 5a boundary site —
    // write one SubgoalCompleted record per milestone appended by
    // the turn. Calling `build_step_record` + `write_step_record` on
    // a `&mut` runner exercises the same persistence path.
    fn persist_subgoal_records(runner: &StateRunner, count: usize) {
        for _ in 0..count {
            let record = runner.build_step_record(
                BoundaryKind::SubgoalCompleted,
                serde_json::json!({"kind": "complete_subgoal"}),
                serde_json::json!({"kind": "subgoal_completed"}),
            );
            runner.write_step_record(&record);
        }
    }

    let (_, _, m1) = runner.run_turn(&t1, &exec).await;
    assert_eq!(m1, 0, "push_subgoal does not append a milestone");
    persist_subgoal_records(&runner, m1);

    // Turn 2: complete A + push B + tool call.
    let t2 = AgentTurn {
        mutations: vec![
            TaskStateMutation::CompleteSubgoal {
                summary: "did A".to_string(),
            },
            TaskStateMutation::PushSubgoal {
                text: "B".to_string(),
            },
        ],
        action: AgentAction::ToolCall {
            tool_name: "cdp_click".to_string(),
            arguments: serde_json::json!({}),
            tool_call_id: "tc-2".to_string(),
        },
    };
    let (_, _, m2) = runner.run_turn(&t2, &exec).await;
    assert_eq!(m2, 1, "CompleteSubgoal appends exactly one milestone");
    persist_subgoal_records(&runner, m2);

    // Turn 3: complete B + agent_done.
    let t3 = AgentTurn {
        mutations: vec![TaskStateMutation::CompleteSubgoal {
            summary: "did B".to_string(),
        }],
        action: AgentAction::AgentDone {
            summary: "done".to_string(),
        },
    };
    let (_, _, m3) = runner.run_turn(&t3, &exec).await;
    assert_eq!(m3, 1, "CompleteSubgoal appends exactly one milestone");
    persist_subgoal_records(&runner, m3);

    let records = read_boundary_records(&events_path);
    assert_eq!(
        count_of(&records, "subgoal_completed"),
        2,
        "two CompleteSubgoal mutations should persist two records; got {:?}",
        records,
    );
}

#[tokio::test]
async fn recovery_succeeded_writes_one_record_on_error_to_success_transition() {
    // Step 1 fails (no such tool). Step 2 succeeds. consecutive_errors
    // goes 0 -> 1 -> 0 across the two turns — the recovery-succeeded
    // boundary must write exactly one record on the transition.
    let llm = ScriptedLlm::new(vec![
        // First: an unknown tool the StaticMcp rejects with an error.
        llm_reply_tool("cdp_click", serde_json::json!({"uid": "d1"})),
        // Second: a known-good observation that the stub replies to.
        llm_reply_tool(
            "cdp_find_elements",
            serde_json::json!({"query": "", "max_results": 300}),
        ),
        llm_reply_tool("agent_done", serde_json::json!({"summary": "ok"})),
    ]);
    // StaticMcp::with_error marks the tool as erroring. `cdp_click` is
    // advertised so the parser dispatches; the registered error body
    // flips the executor into `Err(...)`.
    let mcp = StaticMcp::with_tools(&["cdp_click", "cdp_find_elements"])
        .with_error("cdp_click", "not dispatchable")
        .with_reply(
            "cdp_find_elements",
            r#"{"page_url":"about:blank","source":"cdp","matches":[]}"#,
        );

    let runner = StateRunner::new("goal".to_string(), AgentConfig::default());
    let (runner, _tmp, events_path) = setup_runner_with_storage(runner, "recovery-test");
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

    let records = read_boundary_records(&events_path);
    assert_eq!(
        count_of(&records, "recovery_succeeded"),
        1,
        "error-then-success should persist one RecoverySucceeded record; got {:?}",
        records,
    );
    // Terminal still fires once.
    assert_eq!(count_of(&records, "terminal"), 1);
}

#[tokio::test]
async fn no_boundary_records_written_when_no_storage_attached() {
    // Sanity: run end-to-end without `with_storage`; the write_* calls
    // in the loop must be silent no-ops rather than panicking.
    let llm = ScriptedLlm::new(vec![llm_reply_tool(
        "agent_done",
        serde_json::json!({"summary": "ok"}),
    )]);
    let mcp = StaticMcp::with_tools(&[]);
    let runner = StateRunner::new("goal".to_string(), AgentConfig::default());
    let tools = mcp.tools_as_openai();
    let result = runner
        .run(
            &llm,
            &mcp,
            "goal".to_string(),
            crate::agent::trace_graph::AgentTraceGraph::new(),
            tools,
            None,
        )
        .await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn terminal_boundary_record_carries_world_model_and_task_state_snapshots() {
    let llm = ScriptedLlm::new(vec![llm_reply_tool(
        "agent_done",
        serde_json::json!({"summary": "ok"}),
    )]);
    let mcp = StaticMcp::with_tools(&[]);
    let runner = StateRunner::new("literal-goal".to_string(), AgentConfig::default());
    let (runner, _tmp, events_path) = setup_runner_with_storage(runner, "snapshot-test");
    let tools = mcp.tools_as_openai();
    let _ = runner
        .run(
            &llm,
            &mcp,
            "literal-goal".to_string(),
            crate::agent::trace_graph::AgentTraceGraph::new(),
            tools,
            None,
        )
        .await
        .expect("run ok");

    let records = read_boundary_records(&events_path);
    let terminal = records
        .iter()
        .find(|r| r.get("boundary_kind").and_then(|k| k.as_str()) == Some("terminal"))
        .expect("one terminal record");
    // Every `StepRecord` field must appear on disk — asserted via the
    // JSON projection of `StepRecord` (the type is Serialize-only, so
    // checking field presence is the on-disk contract pin).
    for field in [
        "step_index",
        "boundary_kind",
        "world_model_snapshot",
        "task_state_snapshot",
        "action_taken",
        "outcome",
        "timestamp",
    ] {
        assert!(
            terminal.get(field).is_some(),
            "terminal StepRecord missing `{}` field: {:?}",
            field,
            terminal,
        );
    }
    // Spot-check the task state snapshot carries the original goal so
    // the record is genuinely tied to this run.
    let goal = terminal
        .pointer("/task_state_snapshot/goal")
        .and_then(|v| v.as_str())
        .expect("task_state_snapshot.goal");
    assert_eq!(goal, "literal-goal");
}
