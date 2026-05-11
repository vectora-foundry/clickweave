//! Integration tests for the state-spine `StateRunner`.
//!
//! These tests drive the runner turn-by-turn with a deterministic
//! `AgentTurn` stream and a stubbed `ToolExecutor`, verifying that the
//! state-spine control flow (observe → apply_mutations → dispatch →
//! continuity → invalidation → phase inference) behaves correctly across
//! the canonical scenarios.
//!
//! Phase 2c intentionally stops short of wiring `StateRunner` to a live
//! `ChatBackend` + `McpClient` — the plan calls that out as Phase 3
//! cutover work (the legacy runner → `runner.rs` swap). The harness
//! below is sufficient to exercise the state-spine invariants the design
//! doc requires.

use std::sync::Mutex;

use async_trait::async_trait;

use crate::agent::runner::{AgentAction, AgentTurn, StateRunner, ToolExecutor, TurnOutcome};
use crate::agent::task_state::TaskStateMutation;

/// Deterministic tool executor: pulls the next result off a FIFO queue and
/// returns it. `Ok(body)` for a successful tool body; `Err(msg)` to
/// simulate a tool failure.
struct ScriptedExecutor {
    results: Mutex<Vec<Result<String, String>>>,
}

impl ScriptedExecutor {
    fn new(results: Vec<Result<String, String>>) -> Self {
        Self {
            results: Mutex::new(results),
        }
    }
}

#[async_trait]
impl ToolExecutor for ScriptedExecutor {
    async fn call_tool(
        &self,
        _tool_name: &str,
        _arguments: &serde_json::Value,
    ) -> Result<String, String> {
        let mut q = self.results.lock().unwrap();
        if q.is_empty() {
            Err("scripted_executor: no more results".to_string())
        } else {
            q.remove(0)
        }
    }
}

fn agent_done(summary: &str) -> AgentTurn {
    AgentTurn {
        mutations: vec![],
        action: AgentAction::AgentDone {
            summary: summary.to_string(),
        },
    }
}

fn agent_replan(reason: &str) -> AgentTurn {
    AgentTurn {
        mutations: vec![],
        action: AgentAction::AgentReplan {
            reason: reason.to_string(),
        },
    }
}

fn tool_call(tool: &str, args: serde_json::Value, call_id: &str) -> AgentTurn {
    AgentTurn {
        mutations: vec![],
        action: AgentAction::ToolCall {
            tool_name: tool.to_string(),
            arguments: args,
            tool_call_id: call_id.to_string(),
        },
    }
}

#[tokio::test]
async fn single_step_agent_done_completes_run() {
    let mut r = StateRunner::new_for_test("log in".to_string());
    let exec = ScriptedExecutor::new(vec![]);
    let (outcome, warnings, _milestones) = r.run_turn(&agent_done("completed login"), &exec).await;
    assert!(warnings.is_empty());
    assert!(matches!(outcome, TurnOutcome::Done { .. }));
    // `step_index` counts recorded AgentSteps (advanced by
    // `advance_recorded_step_index`, called only by sites that push
    // onto `state.steps`). `agent_done` is terminal and pushes no
    // step, so the counter stays at 0.
    assert_eq!(r.step_index, 0);
}

#[tokio::test]
async fn multi_step_push_complete_subgoal_tracks_milestones() {
    let mut r = StateRunner::new_for_test("goal".to_string());
    let exec = ScriptedExecutor::new(vec![Ok("ok".to_string())]);

    // Turn 1: push a subgoal + fire a tool call.
    let turn = AgentTurn {
        mutations: vec![TaskStateMutation::PushSubgoal {
            text: "open login form".to_string(),
        }],
        action: AgentAction::ToolCall {
            tool_name: "cdp_click".to_string(),
            arguments: serde_json::json!({"uid":"d1"}),
            tool_call_id: "tc-1".to_string(),
        },
    };
    let (o1, _, _) = r.run_turn(&turn, &exec).await;
    assert!(matches!(o1, TurnOutcome::ToolSuccess { .. }));
    assert_eq!(r.task_state.subgoal_stack.len(), 1);

    // Turn 2: complete subgoal + agent_done.
    let turn2 = AgentTurn {
        mutations: vec![TaskStateMutation::CompleteSubgoal {
            summary: "form opened".to_string(),
        }],
        action: AgentAction::AgentDone {
            summary: "logged in".to_string(),
        },
    };
    let (o2, _, milestones) = r.run_turn(&turn2, &exec).await;
    assert_eq!(
        milestones, 1,
        "CompleteSubgoal should appended one milestone"
    );
    assert!(matches!(o2, TurnOutcome::Done { .. }));
    assert!(r.task_state.subgoal_stack.is_empty());
    assert_eq!(r.task_state.milestones.len(), 1);
    assert_eq!(r.task_state.milestones[0].summary, "form opened");
}

#[tokio::test]
async fn tool_failure_increments_consecutive_errors_and_queues_invalidation() {
    let mut r = StateRunner::new_for_test("goal".to_string());
    let exec = ScriptedExecutor::new(vec![Err("not_dispatchable".to_string())]);

    let (outcome, _, _) = r
        .run_turn(
            &tool_call("cdp_click", serde_json::json!({"uid":"d1"}), "tc-1"),
            &exec,
        )
        .await;
    assert!(matches!(outcome, TurnOutcome::ToolError { .. }));
    assert_eq!(r.consecutive_errors, 1);
    // ToolFailed is queued for the next observe() to consume.
    assert_eq!(r.pending_events.len(), 1);
}

#[tokio::test]
async fn consecutive_errors_transition_phase_to_recovering() {
    let mut r = StateRunner::new_for_test("goal".to_string());
    let exec = ScriptedExecutor::new(vec![Err("first".to_string()), Err("second".to_string())]);

    let _ = r
        .run_turn(
            &tool_call("cdp_click", serde_json::json!({}), "tc-1"),
            &exec,
        )
        .await;
    let _ = r
        .run_turn(
            &tool_call("cdp_click", serde_json::json!({}), "tc-2"),
            &exec,
        )
        .await;

    // After two errors, phase should have shifted out of Exploring.
    assert_eq!(r.consecutive_errors, 2);
    assert_ne!(r.task_state.phase, crate::agent::phase::Phase::Exploring);
}

#[tokio::test]
async fn successful_tool_resets_consecutive_errors() {
    let mut r = StateRunner::new_for_test("goal".to_string());
    let exec = ScriptedExecutor::new(vec![Err("boom".to_string()), Ok("ok".to_string())]);
    let _ = r
        .run_turn(
            &tool_call("cdp_click", serde_json::json!({}), "tc-1"),
            &exec,
        )
        .await;
    assert_eq!(r.consecutive_errors, 1);
    let _ = r
        .run_turn(
            &tool_call("cdp_click", serde_json::json!({}), "tc-2"),
            &exec,
        )
        .await;
    assert_eq!(r.consecutive_errors, 0);
}

#[tokio::test]
async fn take_ax_snapshot_success_populates_continuity() {
    let mut r = StateRunner::new_for_test("goal".to_string());
    let body = "uid=a1g1 button \"OK\"\n  uid=a2g1 textbox \"Email\"";
    let exec = ScriptedExecutor::new(vec![Ok(body.to_string())]);
    let _ = r
        .run_turn(
            &tool_call("take_ax_snapshot", serde_json::json!({}), "tc-ax"),
            &exec,
        )
        .await;
    let snap = r
        .world_model
        .last_native_ax_snapshot
        .as_ref()
        .expect("ax snapshot populated");
    assert_eq!(snap.value.element_count, 2);
    assert!(snap.value.ax_tree_text.contains("uid=a1g1"));
}

#[tokio::test]
async fn agent_replan_records_last_replan_step() {
    let mut r = StateRunner::new_for_test("goal".to_string());
    let exec = ScriptedExecutor::new(vec![]);
    let (_, _, _) = r.run_turn(&agent_replan("form is gone"), &exec).await;
    assert_eq!(r.last_replan_step, Some(0));
}

#[tokio::test]
async fn terminal_boundary_record_captures_final_state() {
    use crate::agent::step_record::BoundaryKind;

    let mut r = StateRunner::new_for_test("goal".to_string());
    let exec = ScriptedExecutor::new(vec![]);
    let (_, _, _) = r
        .run_turn(&agent_done("done".to_string().as_str()), &exec)
        .await;

    let record = r.build_step_record(
        BoundaryKind::Terminal,
        serde_json::to_value(&AgentAction::AgentDone {
            summary: "done".to_string(),
        })
        .unwrap(),
        serde_json::json!({"kind":"completed"}),
    );
    let json = serde_json::to_string(&record).unwrap();
    assert!(json.contains("\"boundary_kind\":\"terminal\""));
    // `step_index` reflects recorded AgentSteps. `agent_done` is
    // terminal with no step push, so the boundary record describes
    // the run as terminating before any step was recorded.
    assert_eq!(record.step_index, 0);
}

#[tokio::test]
async fn subgoal_completed_boundary_written_once_via_storage() {
    use crate::agent::step_record::BoundaryKind;
    use std::sync::Arc;

    let tmp = tempfile::tempdir().unwrap();
    let mut storage = clickweave_core::storage::RunStorage::new(tmp.path(), "int-test");
    let exec_dir = storage.begin_execution().expect("begin exec");
    let storage = Arc::new(Mutex::new(storage));

    let exec = ScriptedExecutor::new(vec![Ok("ok".to_string())]);
    let mut r = StateRunner::new_for_test("goal".to_string()).with_storage(storage.clone());

    // Turn 1: push subgoal, fire tool call.
    let t1 = AgentTurn {
        mutations: vec![TaskStateMutation::PushSubgoal {
            text: "step A".to_string(),
        }],
        action: AgentAction::ToolCall {
            tool_name: "cdp_click".to_string(),
            arguments: serde_json::json!({}),
            tool_call_id: "tc-1".to_string(),
        },
    };
    let _ = r.run_turn(&t1, &exec).await;

    // Turn 2: complete subgoal — write the boundary record.
    let t2 = AgentTurn {
        mutations: vec![TaskStateMutation::CompleteSubgoal {
            summary: "did A".to_string(),
        }],
        action: AgentAction::AgentDone {
            summary: "done".to_string(),
        },
    };
    let _ = r.run_turn(&t2, &exec).await;

    let subgoal_record = r.build_step_record(
        BoundaryKind::SubgoalCompleted,
        serde_json::json!({"kind":"complete_subgoal","summary":"did A"}),
        serde_json::json!({"kind":"success"}),
    );
    r.write_step_record(&subgoal_record);
    let terminal_record = r.build_step_record(
        BoundaryKind::Terminal,
        serde_json::json!({"kind":"agent_done","summary":"done"}),
        serde_json::json!({"kind":"completed"}),
    );
    r.write_step_record(&terminal_record);

    let path = tmp
        .path()
        .join(".clickweave")
        .join("runs")
        .join("int-test")
        .join(&exec_dir)
        .join("events.jsonl");
    let contents = std::fs::read_to_string(&path).unwrap();
    let subgoal_count = contents
        .lines()
        .filter(|l| l.contains("\"boundary_kind\":\"subgoal_completed\""))
        .count();
    assert_eq!(subgoal_count, 1);
    let terminal_count = contents
        .lines()
        .filter(|l| l.contains("\"boundary_kind\":\"terminal\""))
        .count();
    assert_eq!(terminal_count, 1);
}

// ---------------------------------------------------------------------------
// Task 3a.0.6: `RunStorage` parameter plumbing
// ---------------------------------------------------------------------------
//
// Asserts the new `storage` parameter on `run_agent_workflow` compiles and
// flows through the public seam. The legacy `AgentRunner` does not yet
// consume the handle — that wiring lands in Task 3a.6.5. This test pins
// the signature so subsequent tasks cannot silently drop the argument.

mod run_agent_workflow_signature_tests;

// ---------------------------------------------------------------------------
// Task 3a.1: `StateRunner::run` top-level loop skeleton
// ---------------------------------------------------------------------------
//
// Exercises the minimum observe → LLM → parse → apply → dispatch → compact
// loop through stubbed `ChatBackend` (`ScriptedLlm`) + stubbed `Mcp`
// (`StaticMcp`). Deferred behaviour (VLM, approval, loop
// detection, consecutive-destructive cap, workflow-graph emission, CDP
// auto-connect, synthetic focus_window skip, recovery, boundary writes)
// is asserted absent — each later task flips its behaviour on and must
// delete its corresponding `TODO(task-3a.N)` marker from `runner.rs`.

mod top_level_loop_tests;

// ---------------------------------------------------------------------------
// Task 3a.3: VLM completion verification + approval gate
// ---------------------------------------------------------------------------
//
// Exercise `StateRunner::verify_completion` and the live-call approval gate
// through the public `run()` entry point with `ScriptedLlm` + `StaticMcp`
// + `YesVlm`/`NoVlm`. Covers the five sub-branches the plan calls out:
//   1. YES verdict → normal completion
//   2. NO verdict  → CompletionDisagreement terminal + event
//   3. Approval Rejected → Replan step, no tool dispatch
//   4. Approval Unavailable → ApprovalUnavailable terminal
//   5. Artifact persistence (both YES + NO verdicts)
//
// All tests use stubs from `agent/test_stubs.rs` — no network calls, no
// sleeps, no real backends.

mod verify_and_approval_tests;

// ---------------------------------------------------------------------------
// Task 3a.4: loop detection, consecutive-destructive cap, terminal reasons
// ---------------------------------------------------------------------------
//
// Exercise the three new halting paths ported onto `StateRunner::run`:
//
//   1. Loop detection: two identical (tool, args, error) failures in a row
//      halt with `TerminalReason::LoopDetected`.
//   2. Consecutive-destructive cap: N successful destructive tools in a row
//      halt with `TerminalReason::ConsecutiveDestructiveCap` and emit the
//      `ConsecutiveDestructiveCapHit` event.
//   3. Recovery abort: `consecutive_errors >= max_consecutive_errors` halts
//      with `TerminalReason::MaxErrorsReached`.
//
// Each test drives `StateRunner::run` through `ScriptedLlm` + `StaticMcp` so
// the harness exercises the same code path the live runner uses.

mod loop_and_cap_tests;

// ---------------------------------------------------------------------------
// trace-graph accumulation
// ---------------------------------------------------------------------------
//
// Exercise `StateRunner::add_workflow_node` through the public `run()` entry
// point. These tests verify `state.trace_graph` accumulation directly,
// including `source_run_id` stamping, anchor chaining, observation-tool
// filtering, and AX descriptor enrichment.

mod workflow_graph_tests;

// ---------------------------------------------------------------------------
// Task 3a.6: CDP auto-connect + synthetic focus_window skip
// ---------------------------------------------------------------------------
//
// Pin the ported `maybe_cdp_connect` / `should_skip_focus_window` /
// `is_synthetic_focus_skip` behaviour. Heavy end-to-end CDP auto-connect
// flows (quit → relaunch → connect → warmup) would run real timers and
// platform probes, so these tests stay tight around the state-mutation
// contracts the legacy runner pinned: cdp_state bookkeeping from the
// post-tool hook, synthetic focus_window sentinels through the dispatch
// path, and the kind-hint + CDP-live guard table.

mod cdp_and_focus_window_tests;

// ---------------------------------------------------------------------------
// Task 3a.6.5: exactly-once boundary StepRecord writes
// ---------------------------------------------------------------------------
//
// Asserts the three D8 boundaries (`Terminal`, `SubgoalCompleted`,
// `RecoverySucceeded`) each persist exactly one `StepRecord` per
// occurrence to the execution-level `events.jsonl`. The sanity-test that
// runs without storage reuses the unit-level `write_step_record` no-op
// path to confirm the loop doesn't panic when `with_storage` is omitted.

mod boundary_persistence_tests;

// ---------------------------------------------------------------------------
// Task 3a.8: End-to-end tests through `run_agent_workflow`
// ---------------------------------------------------------------------------
//
// Rubric (10) gate: drive the full engine-crate public seam
// (`clickweave_engine::agent::run_agent_workflow`) with `ScriptedLlm` +
// `StaticMcp` stubs and lock the legacy `AgentState` contract
// that external callers (the Tauri command at
// `src-tauri/src/commands/agent/commands.rs`) depend on. These tests are
// distinct from `top_level_loop_tests` above: those exercise
// `StateRunner::run` directly; these go through the builder chain
// `run_agent_workflow` assembles so the wrapper's behaviour is pinned too.
//
// The Phase 3b Tauri-level smoke test (Task 3b.0) covers the Tauri command
// + event forwarder layer; scope of this task stops at the engine crate.
//
// Signature note: `run_agent_workflow` was generified in Task 3a.8 from
// `mcp: &McpClient` to `mcp: &M where M: Mcp + ?Sized` so this test can
// feed it a `StaticMcp` stub without constructing a real MCP subprocess.
// The concrete call site in `src-tauri/src/commands/agent/commands.rs` keeps
// working because `McpClient` satisfies the `Mcp` trait through the
// existing blanket impl in `crate::executor`.

mod e2e_run_agent_workflow_tests;

// ---------------------------------------------------------------------------
// Task 3.5: D18 variant-context lives in messages[1], not messages[0].
// ---------------------------------------------------------------------------
//
// The system prompt (messages[0]) stays stable across runs so the prompt
// cache keeps its prefix hit. Variant context + prior-turn log are
// composed into the goal string by the caller (`build_goal_block`) and
// land in messages[1] (goal slot). This module locks both halves of the
// invariant through the public `run_agent_workflow` seam.
mod variant_context_placement_tests;

// ---------------------------------------------------------------------------
// Task 3.4: D17 `agent://*` event contract — TaskStateChanged /
// WorldModelChanged / BoundaryRecordWritten emissions.
// ---------------------------------------------------------------------------
//
// Asserts the three new `AgentEvent` variants fire with the runner's
// `run_id` threaded through to the event payload, and that the
// boundary-event emission tracks the corresponding `StepRecord`
// persistence exactly.

mod state_spine_event_contract_tests;

// Repeat-action loop detection: identical successful non-observation calls
// for `REPEAT_ACTION_THRESHOLD` consecutive turns must emit an
// `AgentEvent::Warning` carrying `NO_PROGRESS_WARNING_PREFIX`.
mod repeat_action_loop_detection_tests;
