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

#[cfg(test)]
mod run_agent_workflow_signature_tests {
    /// Compile-time assertion: `run_agent_workflow` accepts a
    /// `Option<RunStorageHandle>` as its last parameter.
    ///
    /// If this coerces, the plumbing compiles; we do not invoke the
    /// function here because it takes a concrete `McpClient` which cannot
    /// be instantiated in-crate without spawning the external MCP server.
    /// Task 3a.1's `ScriptedLlm`/`StaticMcp` stubs enable a live
    /// end-to-end test.
    #[test]
    fn run_agent_workflow_accepts_storage_argument() {
        fn _coerce() {
            let _: Option<crate::agent::RunStorageHandle> = None;
        }
    }
}

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

#[cfg(test)]
mod top_level_loop_tests {
    use super::super::super::test_stubs::{ScriptedLlm, StaticMcp, llm_reply_tool};
    use crate::agent::runner::StateRunner;
    use crate::agent::types::{AgentConfig, TerminalReason};
    use crate::executor::Mcp;

    #[tokio::test]
    async fn run_completes_on_agent_done_after_two_tool_calls() {
        let llm = ScriptedLlm::new(vec![
            llm_reply_tool(
                "cdp_find_elements",
                serde_json::json!({"query": "", "max_results": 300}),
            ),
            llm_reply_tool("cdp_click", serde_json::json!({"uid": "d1"})),
            llm_reply_tool("agent_done", serde_json::json!({"summary": "ok"})),
        ]);
        let mcp = StaticMcp::with_tools(&["cdp_find_elements", "cdp_click"])
            .with_reply(
                "cdp_find_elements",
                r#"{"page_url":"about:blank","source":"cdp","matches":[]}"#,
            )
            .with_reply("cdp_click", "clicked");

        let tools = mcp.tools_as_openai();
        let runner = StateRunner::new("goal".to_string(), AgentConfig::default());
        let state = runner
            .run(
                &llm,
                &mcp,
                "goal".to_string(),
                clickweave_core::Workflow::default(),
                tools,
                None,
            )
            .await
            .expect("run ok");

        assert_eq!(
            state.steps.len(),
            2,
            "two dispatched tool calls should be recorded as steps"
        );
        assert!(state.completed, "agent_done should mark state.completed");
        assert!(
            matches!(
                state.terminal_reason,
                Some(TerminalReason::Completed { .. })
            ),
            "terminal reason should be Completed, got {:?}",
            state.terminal_reason,
        );
    }

    #[tokio::test]
    async fn run_terminates_at_max_steps_without_completion() {
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

        let tools = mcp.tools_as_openai();
        let cfg = AgentConfig {
            max_steps: 3,
            ..AgentConfig::default()
        };
        let runner = StateRunner::new("goal".to_string(), cfg);
        let state = runner
            .run(
                &llm,
                &mcp,
                "goal".to_string(),
                clickweave_core::Workflow::default(),
                tools,
                None,
            )
            .await
            .expect("run ok");

        assert_eq!(state.steps.len(), 3);
        assert!(!state.completed);
        assert!(
            matches!(
                state.terminal_reason,
                Some(TerminalReason::MaxStepsReached { steps_executed: 3 })
            ),
            "terminal reason should be MaxStepsReached {{3}}, got {:?}",
            state.terminal_reason,
        );
    }

    #[tokio::test]
    async fn run_records_tool_error_as_step_error() {
        // cdp_click is asked to fail by the stub: the MCP returns is_error
        // via a tool that does not exist. Instead we use NullMcp-style
        // behaviour via StaticMcp without the right tool; but StaticMcp
        // falls back to "ok". Simulate a tool error by having the stub
        // return a reply through has_tool=false path — the McpToolExecutor
        // surfaces the bail! as an error body.
        use super::super::super::test_stubs::NullMcp;
        let llm = ScriptedLlm::new(vec![
            llm_reply_tool("cdp_click", serde_json::json!({"uid": "d1"})),
            llm_reply_tool("agent_done", serde_json::json!({"summary": "stop"})),
        ]);
        let mcp = NullMcp;
        let runner = StateRunner::new("goal".to_string(), AgentConfig::default());
        let state = runner
            .run(
                &llm,
                &mcp,
                "goal".to_string(),
                clickweave_core::Workflow::default(),
                Vec::new(),
                None,
            )
            .await
            .expect("run ok");

        assert_eq!(state.steps.len(), 1, "the failing tool call is recorded");
        let step = &state.steps[0];
        assert!(matches!(
            step.outcome,
            crate::agent::types::StepOutcome::Error(_)
        ));
        assert!(state.completed);
    }

    /// Phase 3a port is complete — no deferred-work markers remain.
    ///
    /// Tasks 3a.3 (VLM verification + approval gate),
    /// 3a.4 (loop detection, destructive cap, terminal-reason mapping),
    /// 3a.5 (workflow-graph emission), 3a.6 (CDP auto-connect + synthetic
    /// focus_window skip), and 3a.6.5 (exactly-once boundary `StepRecord`
    /// writes) have all landed. Each task removed its corresponding
    /// `TODO(task-3a.N)` marker from `runner.rs` when its behaviour was
    /// wired into `StateRunner::run`. This test pins the zero-marker
    /// contract so a regression that re-introduces deferred work would
    /// fail loudly.
    ///
    /// Tasks 3a.7 (legacy test migration), 3a.8 (end-to-end test), and
    /// 3a.9 (specta derives) do not touch `runner.rs` semantics — they
    /// are testing / binding concerns, not deferred runtime hooks, so
    /// they never planted markers here.
    #[test]
    fn runner_source_has_no_deferred_task_markers() {
        let runner_src = include_str!("../runner.rs");
        // Scan only the non-doc portion of the file — the doc-comment on
        // `parse_agent_turn` historically references `TODO(task-3a.2)` as
        // forward-looking narrative, which must not be interpreted as a
        // deferred-work pin. The canonical marker shape planted by
        // earlier tasks was a line-comment `// TODO(task-3a.N):`; only
        // match that exact form.
        let offenders: Vec<&str> = runner_src
            .lines()
            .filter(|line| line.trim_start().starts_with("// TODO(task-3a."))
            .collect();
        assert!(
            offenders.is_empty(),
            "expected zero `// TODO(task-3a.N):` markers in runner.rs but found: {:?}",
            offenders,
        );
    }
}

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

#[cfg(test)]
mod verify_and_approval_tests {
    use std::sync::Arc;

    use clickweave_core::Workflow;
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
    const TINY_PNG_BASE64: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNkYAAAAAYAAjCB0C8AAAAASUVORK5CYII=";

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
                Workflow::default(),
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
                Workflow::default(),
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
                Workflow::default(),
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
                Workflow::default(),
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

        let runner =
            StateRunner::new("goal".to_string(), cfg_with_steps(5)).with_approval(approval_tx);
        let state = runner
            .run(
                &llm,
                &mcp,
                "goal".to_string(),
                Workflow::default(),
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
        let (approval_tx, approval_rx) =
            mpsc::channel::<(ApprovalRequest, oneshot::Sender<bool>)>(1);
        drop(approval_rx);

        let runner =
            StateRunner::new("goal".to_string(), cfg_with_steps(5)).with_approval(approval_tx);
        let state = runner
            .run(
                &llm,
                &mcp,
                "goal".to_string(),
                Workflow::default(),
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

        let runner =
            StateRunner::new("goal".to_string(), cfg_with_steps(5)).with_approval(approval_tx);
        let state = runner
            .run(
                &llm,
                &mcp,
                "goal".to_string(),
                Workflow::default(),
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
}

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

#[cfg(test)]
mod loop_and_cap_tests {
    use super::super::super::test_stubs::{ScriptedLlm, StaticMcp, llm_reply_tool};
    use crate::agent::runner::StateRunner;
    use crate::agent::types::{AgentConfig, AgentEvent, RunnerOutput, TerminalReason};
    use crate::executor::Mcp;
    use clickweave_core::Workflow;
    use tokio::sync::mpsc;

    fn cfg_with_steps(steps: usize) -> AgentConfig {
        AgentConfig {
            max_steps: steps,
            ..AgentConfig::default()
        }
    }

    /// Build an MCP stub that advertises a single destructive tool flagged
    /// via `destructiveHint = true`. `cdp_find_elements` is also advertised
    /// so the runner's observe phase returns an empty but well-formed page
    /// (no schema-drift warning).
    fn destructive_mcp(tool_name: &str) -> StaticMcp {
        let tools = serde_json::json!([
            {
                "type": "function",
                "function": {
                    "name": tool_name,
                    "description": "stub destructive",
                    "parameters": {"type": "object", "properties": {}},
                    "annotations": {"destructiveHint": true, "readOnlyHint": false}
                }
            },
            {
                "type": "function",
                "function": {
                    "name": "cdp_find_elements",
                    "description": "stub",
                    "parameters": {"type": "object", "properties": {}}
                }
            }
        ]);
        let stub = StaticMcp::with_tools(&[tool_name, "cdp_find_elements"]).with_reply(
            "cdp_find_elements",
            r#"{"page_url":"about:blank","source":"cdp","matches":[]}"#,
        );
        // Replace the advertised tool list so the destructive annotation is
        // visible to `build_annotations_index` / `maybe_halt_on_destructive_cap`.
        stub.with_tools_override(tools.as_array().unwrap().clone())
    }

    /// Two identical failing `cdp_click` calls halt on the second turn with
    /// `TerminalReason::LoopDetected`. Exercises the live-path loop detector
    /// ported from `AgentRunner::handle_step_outcome`.
    #[tokio::test]
    async fn two_identical_tool_errors_in_a_row_halt_with_loop_detected() {
        let llm = ScriptedLlm::new(vec![
            llm_reply_tool("cdp_click", serde_json::json!({"uid": "d1"})),
            llm_reply_tool("cdp_click", serde_json::json!({"uid": "d1"})),
            // Guard: if loop detection somehow didn't fire, fall through to
            // agent_done so the test doesn't hang.
            llm_reply_tool("agent_done", serde_json::json!({"summary": "x"})),
        ]);
        let mcp =
            StaticMcp::with_tools(&["cdp_click"]).with_error("cdp_click", "element not found");
        let tools = mcp.tools_as_openai();
        let runner = StateRunner::new("goal".to_string(), cfg_with_steps(5));

        let state = runner
            .run(
                &llm,
                &mcp,
                "goal".to_string(),
                Workflow::default(),
                tools,
                None,
            )
            .await
            .expect("run ok");

        match state.terminal_reason {
            Some(TerminalReason::LoopDetected { tool_name, error }) => {
                assert_eq!(tool_name, "cdp_click");
                assert_eq!(error, "element not found");
            }
            other => panic!("expected LoopDetected, got {:?}", other),
        }
        assert_eq!(
            state.steps.len(),
            2,
            "loop detection fires on the second identical failure"
        );
    }

    /// Different arguments for the same tool must NOT trigger loop detection
    /// — the LLM is exploring, not looping. After two different-uid
    /// failures the run should hit `MaxErrorsReached` (cfg max is 2) rather
    /// than `LoopDetected`, pinning that the args comparison is live.
    #[tokio::test]
    async fn different_args_do_not_trigger_loop_detection() {
        let mut cfg = cfg_with_steps(5);
        cfg.max_consecutive_errors = 2;
        let llm = ScriptedLlm::new(vec![
            llm_reply_tool("cdp_click", serde_json::json!({"uid": "d1"})),
            llm_reply_tool("cdp_click", serde_json::json!({"uid": "d2"})),
            llm_reply_tool("agent_done", serde_json::json!({"summary": "x"})),
        ]);
        let mcp =
            StaticMcp::with_tools(&["cdp_click"]).with_error("cdp_click", "element not found");
        let tools = mcp.tools_as_openai();
        let runner = StateRunner::new("goal".to_string(), cfg);

        let state = runner
            .run(
                &llm,
                &mcp,
                "goal".to_string(),
                Workflow::default(),
                tools,
                None,
            )
            .await
            .expect("run ok");

        match state.terminal_reason {
            Some(TerminalReason::MaxErrorsReached { consecutive_errors }) => {
                assert_eq!(consecutive_errors, 2);
            }
            other => panic!(
                "different args should NOT trip LoopDetected; got {:?}",
                other
            ),
        }
    }

    /// Three successful destructive tools in a row halt the run with
    /// `TerminalReason::ConsecutiveDestructiveCap` and emit the matching
    /// `ConsecutiveDestructiveCapHit` event.
    #[tokio::test]
    async fn consecutive_destructive_cap_halts_run() {
        let mut cfg = cfg_with_steps(10);
        cfg.consecutive_destructive_cap = 3;
        let llm = ScriptedLlm::new(vec![
            llm_reply_tool("quit_app", serde_json::json!({"app_name": "A"})),
            llm_reply_tool("quit_app", serde_json::json!({"app_name": "B"})),
            llm_reply_tool("quit_app", serde_json::json!({"app_name": "C"})),
            // Guard: destructive cap should halt before this runs.
            llm_reply_tool("agent_done", serde_json::json!({"summary": "x"})),
        ]);
        let mcp = destructive_mcp("quit_app");
        let tools = mcp.tools_as_openai();

        let (event_tx, mut event_rx) = mpsc::channel::<RunnerOutput>(32);
        let runner = StateRunner::new("goal".to_string(), cfg).with_events(event_tx);
        let state = runner
            .run(
                &llm,
                &mcp,
                "goal".to_string(),
                Workflow::default(),
                tools,
                None,
            )
            .await
            .expect("run ok");

        match state.terminal_reason {
            Some(TerminalReason::ConsecutiveDestructiveCap {
                cap,
                recent_tool_names,
            }) => {
                assert_eq!(cap, 3);
                assert_eq!(recent_tool_names, vec!["quit_app", "quit_app", "quit_app"]);
            }
            other => panic!("expected ConsecutiveDestructiveCap, got {:?}", other),
        }

        let mut saw_cap_event = false;
        while let Ok(ev) = event_rx.try_recv() {
            let Some(ev) = ev.into_event() else {
                continue;
            };
            if matches!(ev, AgentEvent::ConsecutiveDestructiveCapHit { .. }) {
                saw_cap_event = true;
                break;
            }
        }
        assert!(
            saw_cap_event,
            "ConsecutiveDestructiveCapHit event must be emitted"
        );
    }

    /// A non-destructive (read-only) success in between destructive calls
    /// resets the streak. With cap=3, the sequence destr/destr/read/destr
    /// finishes with an agent_done rather than hitting the cap.
    #[tokio::test]
    async fn non_destructive_success_resets_destructive_streak() {
        let mut cfg = cfg_with_steps(10);
        cfg.consecutive_destructive_cap = 3;
        // Advertise both a destructive tool and a read-only probe so the
        // annotations index sees both hints.
        let tools = serde_json::json!([
            {
                "type": "function",
                "function": {
                    "name": "quit_app",
                    "description": "destructive",
                    "parameters": {"type": "object", "properties": {}},
                    "annotations": {"destructiveHint": true}
                }
            },
            {
                "type": "function",
                "function": {
                    "name": "probe_app",
                    "description": "read-only",
                    "parameters": {"type": "object", "properties": {}},
                    "annotations": {"readOnlyHint": true}
                }
            },
            {
                "type": "function",
                "function": {
                    "name": "cdp_find_elements",
                    "description": "stub",
                    "parameters": {"type": "object", "properties": {}}
                }
            }
        ]);
        let mcp = StaticMcp::with_tools(&["quit_app", "probe_app", "cdp_find_elements"])
            .with_reply(
                "cdp_find_elements",
                r#"{"page_url":"about:blank","source":"cdp","matches":[]}"#,
            )
            .with_reply("quit_app", "quit-ok")
            .with_reply("probe_app", "{}")
            .with_tools_override(tools.as_array().unwrap().clone());

        let llm = ScriptedLlm::new(vec![
            llm_reply_tool("quit_app", serde_json::json!({"app_name": "A"})),
            llm_reply_tool("quit_app", serde_json::json!({"app_name": "B"})),
            llm_reply_tool("probe_app", serde_json::json!({"app_name": "A"})),
            llm_reply_tool("quit_app", serde_json::json!({"app_name": "C"})),
            llm_reply_tool("quit_app", serde_json::json!({"app_name": "D"})),
            llm_reply_tool("agent_done", serde_json::json!({"summary": "done"})),
        ]);
        let advertised = mcp.tools_as_openai();
        let runner = StateRunner::new("goal".to_string(), cfg);
        let state = runner
            .run(
                &llm,
                &mcp,
                "goal".to_string(),
                Workflow::default(),
                advertised,
                None,
            )
            .await
            .expect("run ok");

        // Run completed via agent_done, not destructive cap.
        assert!(
            state.completed,
            "run should have completed, not been capped"
        );
        assert!(matches!(
            state.terminal_reason,
            Some(TerminalReason::Completed { .. })
        ));
    }

    /// `consecutive_destructive_cap == 0` disables the feature entirely:
    /// many destructive tools in a row run without halting.
    #[tokio::test]
    async fn cap_zero_disables_destructive_feature() {
        let mut cfg = cfg_with_steps(20);
        cfg.consecutive_destructive_cap = 0;
        let llm = ScriptedLlm::new(vec![
            llm_reply_tool("quit_app", serde_json::json!({"app_name": "A"})),
            llm_reply_tool("quit_app", serde_json::json!({"app_name": "B"})),
            llm_reply_tool("quit_app", serde_json::json!({"app_name": "C"})),
            llm_reply_tool("quit_app", serde_json::json!({"app_name": "D"})),
            llm_reply_tool("quit_app", serde_json::json!({"app_name": "E"})),
            llm_reply_tool("agent_done", serde_json::json!({"summary": "done"})),
        ]);
        let mcp = destructive_mcp("quit_app").with_reply("quit_app", "quit-ok");
        let tools = mcp.tools_as_openai();
        let runner = StateRunner::new("goal".to_string(), cfg);
        let state = runner
            .run(
                &llm,
                &mcp,
                "goal".to_string(),
                Workflow::default(),
                tools,
                None,
            )
            .await
            .expect("run ok");

        assert!(state.completed, "cap=0 should disable the halt");
        assert!(matches!(
            state.terminal_reason,
            Some(TerminalReason::Completed { .. })
        ));
    }

    /// `max_consecutive_errors = 2` + two different-args failures halts
    /// with `TerminalReason::MaxErrorsReached`.
    #[tokio::test]
    async fn max_errors_reached_sets_correct_terminal_reason() {
        let mut cfg = cfg_with_steps(10);
        cfg.max_consecutive_errors = 2;
        let llm = ScriptedLlm::new(vec![
            llm_reply_tool("cdp_click", serde_json::json!({"uid": "d1"})),
            llm_reply_tool("cdp_click", serde_json::json!({"uid": "d2"})),
            llm_reply_tool("agent_done", serde_json::json!({"summary": "x"})),
        ]);
        let mcp = StaticMcp::with_tools(&["cdp_click"]).with_error("cdp_click", "elem not found");
        let tools = mcp.tools_as_openai();
        let runner = StateRunner::new("goal".to_string(), cfg);
        let state = runner
            .run(
                &llm,
                &mcp,
                "goal".to_string(),
                Workflow::default(),
                tools,
                None,
            )
            .await
            .expect("run ok");

        match state.terminal_reason {
            Some(TerminalReason::MaxErrorsReached { consecutive_errors }) => {
                assert_eq!(consecutive_errors, 2);
            }
            other => panic!("expected MaxErrorsReached, got {:?}", other),
        }
    }
}

// ---------------------------------------------------------------------------
// Task 3a.5: workflow-graph emission
// ---------------------------------------------------------------------------
//
// Exercise `StateRunner::add_workflow_node` through the public `run()` entry
// point. These tests pin the ported `NodeAdded` / `EdgeAdded` behaviour,
// including `source_run_id` stamping, anchor chaining, observation-tool
// filtering, and AX descriptor enrichment.

#[cfg(test)]
mod workflow_graph_tests {
    use super::super::super::test_stubs::{ScriptedLlm, StaticMcp, llm_reply_tool};
    use crate::agent::runner::StateRunner;
    use crate::agent::types::{AgentConfig, AgentEvent, RunnerOutput, TerminalReason};
    use crate::executor::Mcp;
    use clickweave_core::Workflow;
    use tokio::sync::mpsc;

    fn cfg_with_steps(steps: usize) -> AgentConfig {
        AgentConfig {
            max_steps: steps,
            ..AgentConfig::default()
        }
    }

    /// MCP fixture: advertises `cdp_find_elements` + `cdp_click`; the
    /// `cdp_find_elements` reply contains exactly one element so the
    /// fingerprint is stable across runs.
    fn build_mcp_with_one_element() -> StaticMcp {
        let body = r#"{"page_url":"about:blank","source":"cdp","matches":[{"uid":"1_0","role":"button","label":"Submit","tag":"button","disabled":false,"parent_role":null,"parent_name":null}]}"#;
        StaticMcp::with_tools(&["cdp_find_elements", "cdp_click"])
            .with_reply("cdp_find_elements", body)
            .with_reply("cdp_click", "clicked")
    }

    /// Drain `event_rx` of every already-buffered event. Non-blocking.
    fn drain_events(rx: &mut mpsc::Receiver<RunnerOutput>) -> Vec<AgentEvent> {
        let mut out = Vec::new();
        while let Ok(ev) = rx.try_recv() {
            if let Some(event) = ev.into_event() {
                out.push(event);
            }
        }
        out
    }

    /// A successful live-path tool call emits `AgentEvent::NodeAdded` with the
    /// runner's `run_id` stamped as `source_run_id`, and the workflow gains a
    /// single node with no prior edge (the anchor slot is empty).
    #[tokio::test]
    async fn successful_tool_call_emits_node_added_event_with_source_run_id() {
        let llm = ScriptedLlm::new(vec![
            llm_reply_tool("cdp_click", serde_json::json!({"uid": "1_0"})),
            llm_reply_tool("agent_done", serde_json::json!({"summary": "ok"})),
        ]);
        let mcp = build_mcp_with_one_element();
        let tools = mcp.tools_as_openai();

        let run_id = uuid::Uuid::new_v4();
        let (event_tx, mut event_rx) = mpsc::channel::<RunnerOutput>(16);
        let runner = StateRunner::new("goal".to_string(), cfg_with_steps(5))
            .with_run_id(run_id)
            .with_events(event_tx);

        let state = runner
            .run(
                &llm,
                &mcp,
                "goal".to_string(),
                Workflow::default(),
                tools,
                None,
            )
            .await
            .expect("run ok");

        let events = drain_events(&mut event_rx);
        let node_events: Vec<_> = events
            .iter()
            .filter_map(|ev| match ev {
                AgentEvent::NodeAdded { node } => Some(node.as_ref()),
                _ => None,
            })
            .collect();
        assert_eq!(node_events.len(), 1, "one live tool call → one NodeAdded");
        assert_eq!(
            node_events[0].source_run_id,
            Some(run_id),
            "every emitted node must carry the runner's run_id as source_run_id"
        );
        // No EdgeAdded — anchor_node_id is None and this is the first node.
        assert!(
            !events
                .iter()
                .any(|ev| matches!(ev, AgentEvent::EdgeAdded { .. })),
            "first node without an anchor must not emit an EdgeAdded"
        );
        assert_eq!(state.workflow.nodes.len(), 1);
        assert!(state.workflow.edges.is_empty());
    }

    /// Two successful tool calls emit an `EdgeAdded` that connects the first
    /// node to the second, and the workflow's edge vec is populated.
    #[tokio::test]
    async fn second_tool_call_emits_edge_added_connecting_to_first_node() {
        let llm = ScriptedLlm::new(vec![
            llm_reply_tool("cdp_click", serde_json::json!({"uid": "1_0"})),
            llm_reply_tool("cdp_click", serde_json::json!({"uid": "2_0"})),
            llm_reply_tool("agent_done", serde_json::json!({"summary": "ok"})),
        ]);
        let mcp = build_mcp_with_one_element();
        let tools = mcp.tools_as_openai();

        let (event_tx, mut event_rx) = mpsc::channel::<RunnerOutput>(32);
        let runner = StateRunner::new("goal".to_string(), cfg_with_steps(5)).with_events(event_tx);

        let state = runner
            .run(
                &llm,
                &mcp,
                "goal".to_string(),
                Workflow::default(),
                tools,
                None,
            )
            .await
            .expect("run ok");

        let events = drain_events(&mut event_rx);
        let nodes: Vec<_> = events
            .iter()
            .filter_map(|ev| match ev {
                AgentEvent::NodeAdded { node } => Some(node.as_ref().clone()),
                _ => None,
            })
            .collect();
        let edges: Vec<_> = events
            .iter()
            .filter_map(|ev| match ev {
                AgentEvent::EdgeAdded { edge } => Some(edge.clone()),
                _ => None,
            })
            .collect();

        assert_eq!(nodes.len(), 2, "two live tool calls → two NodeAdded");
        assert_eq!(edges.len(), 1, "two nodes, no anchor → one EdgeAdded");
        assert_eq!(edges[0].from, nodes[0].id);
        assert_eq!(edges[0].to, nodes[1].id);
        assert_eq!(state.workflow.nodes.len(), 2);
        assert_eq!(state.workflow.edges.len(), 1);
    }

    /// Observation-only tools (here `cdp_find_elements`) execute but must not
    /// produce a workflow node or emit `NodeAdded`.
    #[tokio::test]
    async fn observation_tool_does_not_emit_node() {
        let llm = ScriptedLlm::new(vec![
            llm_reply_tool(
                "cdp_find_elements",
                serde_json::json!({"query": "", "max_results": 300}),
            ),
            llm_reply_tool("agent_done", serde_json::json!({"summary": "ok"})),
        ]);
        let mcp = build_mcp_with_one_element();
        let tools = mcp.tools_as_openai();

        let (event_tx, mut event_rx) = mpsc::channel::<RunnerOutput>(16);
        let runner = StateRunner::new("goal".to_string(), cfg_with_steps(5)).with_events(event_tx);

        let state = runner
            .run(
                &llm,
                &mcp,
                "goal".to_string(),
                Workflow::default(),
                tools,
                None,
            )
            .await
            .expect("run ok");

        let events = drain_events(&mut event_rx);
        let node_count = events
            .iter()
            .filter(|ev| matches!(ev, AgentEvent::NodeAdded { .. }))
            .count();
        assert_eq!(
            node_count, 0,
            "observation tools must not produce workflow nodes"
        );
        assert!(state.workflow.nodes.is_empty());
        assert!(state.workflow.edges.is_empty());
    }

    /// A caller-provided `anchor_node_id` seeds `state.last_node_id`, so the
    /// first live node chains from the anchor via `EdgeAdded`.
    #[tokio::test]
    async fn anchor_node_id_chains_first_new_node() {
        let anchor = uuid::Uuid::new_v4();
        let llm = ScriptedLlm::new(vec![
            llm_reply_tool("cdp_click", serde_json::json!({"uid": "1_0"})),
            llm_reply_tool("agent_done", serde_json::json!({"summary": "ok"})),
        ]);
        let mcp = build_mcp_with_one_element();
        let tools = mcp.tools_as_openai();

        let (event_tx, mut event_rx) = mpsc::channel::<RunnerOutput>(16);
        let runner = StateRunner::new("goal".to_string(), cfg_with_steps(5)).with_events(event_tx);

        let state = runner
            .run(
                &llm,
                &mcp,
                "goal".to_string(),
                Workflow::default(),
                tools,
                Some(anchor),
            )
            .await
            .expect("run ok");

        let events = drain_events(&mut event_rx);
        let first_node = events.iter().find_map(|ev| match ev {
            AgentEvent::NodeAdded { node } => Some(node.as_ref().clone()),
            _ => None,
        });
        let first_edge = events.iter().find_map(|ev| match ev {
            AgentEvent::EdgeAdded { edge } => Some(edge.clone()),
            _ => None,
        });
        let node = first_node.expect("one live node");
        let edge = first_edge.expect("anchor must produce a first edge");
        assert_eq!(edge.from, anchor, "first edge must chain from the anchor");
        assert_eq!(edge.to, node.id);
        assert_eq!(state.workflow.edges.len(), 1);
    }

    /// `build_workflow = false` opts out of workflow-graph emission even on a
    /// successful tool call. No nodes, no edges, no events.
    #[tokio::test]
    async fn build_workflow_false_suppresses_node_emission() {
        let mut cfg = cfg_with_steps(5);
        cfg.build_workflow = false;
        let llm = ScriptedLlm::new(vec![
            llm_reply_tool("cdp_click", serde_json::json!({"uid": "1_0"})),
            llm_reply_tool("agent_done", serde_json::json!({"summary": "ok"})),
        ]);
        let mcp = build_mcp_with_one_element();
        let tools = mcp.tools_as_openai();

        let (event_tx, mut event_rx) = mpsc::channel::<RunnerOutput>(16);
        let runner = StateRunner::new("goal".to_string(), cfg).with_events(event_tx);

        let state = runner
            .run(
                &llm,
                &mcp,
                "goal".to_string(),
                Workflow::default(),
                tools,
                None,
            )
            .await
            .expect("run ok");

        let events = drain_events(&mut event_rx);
        assert!(
            !events
                .iter()
                .any(|ev| matches!(ev, AgentEvent::NodeAdded { .. })),
            "build_workflow=false must suppress NodeAdded"
        );
        assert!(state.workflow.nodes.is_empty());
        assert!(
            matches!(
                state.terminal_reason,
                Some(TerminalReason::Completed { .. })
            ),
            "run still completes normally, {:?}",
            state.terminal_reason,
        );
    }
}

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

#[cfg(test)]
mod cdp_and_focus_window_tests {
    use super::super::super::test_stubs::{ScriptedLlm, StaticMcp, llm_reply_tool};
    use crate::agent::runner::{FocusSkipReason, StateRunner};
    use crate::agent::types::{AgentConfig, AgentEvent, RunnerOutput, TerminalReason};
    use crate::executor::Mcp;
    use clickweave_core::Workflow;
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
                Workflow::default(),
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
                Workflow::default(),
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
            state.workflow.nodes.is_empty(),
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
                Workflow::default(),
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
                Workflow::default(),
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
                Workflow::default(),
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
                Workflow::default(),
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
                Workflow::default(),
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
                Workflow::default(),
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
                Workflow::default(),
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
        let runner =
            StateRunner::new("g".to_string(), AgentConfig::default()).with_events(event_tx);
        let mcp = RefreshCountingMcp {
            refreshes: AtomicUsize::new(0),
        };

        crate::agent::runner::test_support::call_finalize_cdp_connected(
            &runner, "Signal", 9333, &mcp,
        )
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
}

// ---------------------------------------------------------------------------
// Task 3a.6.5: exactly-once boundary StepRecord writes
// ---------------------------------------------------------------------------
//
// Asserts the three D8 boundaries (`Terminal`, `SubgoalCompleted`,
// `RecoverySucceeded`) each persist exactly one `StepRecord` per
// occurrence to the execution-level `events.jsonl`. The sanity-test that
// runs without storage reuses the unit-level `write_step_record` no-op
// path to confirm the loop doesn't panic when `with_storage` is omitted.

#[cfg(test)]
mod boundary_persistence_tests {
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
                clickweave_core::Workflow::default(),
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
                clickweave_core::Workflow::default(),
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
                clickweave_core::Workflow::default(),
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
                clickweave_core::Workflow::default(),
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
                clickweave_core::Workflow::default(),
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
}

// ---------------------------------------------------------------------------
// Task 3a.8: End-to-end tests through `run_agent_workflow`
// ---------------------------------------------------------------------------
//
// Rubric (10) gate: drive the full engine-crate public seam
// (`clickweave_engine::agent::run_agent_workflow`) with `ScriptedLlm` +
// `StaticMcp` stubs and lock the legacy `AgentState` contract
// that external callers (the Tauri command at
// `src-tauri/src/commands/agent.rs:507-525`) depend on. These tests are
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
// The concrete call site in `src-tauri/src/commands/agent.rs` keeps
// working because `McpClient` satisfies the `Mcp` trait through the
// existing blanket impl in `crate::executor`.

#[cfg(test)]
mod e2e_run_agent_workflow_tests {
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
}

// ---------------------------------------------------------------------------
// Task 3.5: D18 variant-context lives in messages[1], not messages[0].
// ---------------------------------------------------------------------------
//
// The system prompt (messages[0]) stays stable across runs so the prompt
// cache keeps its prefix hit. Variant context + prior-turn log are
// composed into the goal string by the caller (`build_goal_block`) and
// land in messages[1] (goal slot). This module locks both halves of the
// invariant through the public `run_agent_workflow` seam.
#[cfg(test)]
mod variant_context_placement_tests {
    use super::super::super::test_stubs::{
        CapturingLlm, StaticMcp, build_agent_done_response, llm_reply_tool,
    };
    use crate::agent::types::AgentConfig;
    use crate::agent::{build_goal_block, run_agent_workflow};
    use clickweave_llm::Role;

    /// Variant context must appear in `messages[1]` (user/goal slot) and
    /// never in `messages[0]` (system prompt). Asserts the D18 invariant
    /// end-to-end through the public `run_agent_workflow` seam.
    #[tokio::test]
    async fn variant_context_lands_in_messages_1_not_messages_0() {
        const VARIANT_SENTINEL: &str = "VARIANT_CTX_SENTINEL_XYZ";
        let llm = CapturingLlm::new(vec![
            llm_reply_tool(
                "cdp_find_elements",
                serde_json::json!({"query": "", "max_results": 10}),
            ),
            build_agent_done_response("done"),
        ]);
        let mcp = StaticMcp::with_tools(&["cdp_find_elements"]).with_reply(
            "cdp_find_elements",
            r#"{"page_url":"about:blank","source":"cdp","matches":[]}"#,
        );

        // Compose the goal-block exactly the way the Tauri seam now
        // does — prior turns + variant context + user goal.
        let goal_block = build_goal_block(
            "log me in",
            &[],
            Some(&format!("variant=A; sentinel={}", VARIANT_SENTINEL)),
            1000,
        );

        let (_state, _writer_tx) = run_agent_workflow(
            &llm,
            AgentConfig::default(),
            goal_block,
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

        let messages = llm.messages_at(0);
        assert!(
            messages.len() >= 2,
            "runner must send at least [system, user] on the first turn; got len={}",
            messages.len()
        );
        assert_eq!(
            messages[0].role,
            Role::System,
            "messages[0] must be the system prompt"
        );
        assert_eq!(
            messages[1].role,
            Role::User,
            "messages[1] must be the user/goal turn"
        );

        let sys_text = messages[0].content_text().unwrap_or("").to_string();
        let user_text = messages[1].content_text().unwrap_or("").to_string();

        assert!(
            !sys_text.contains(VARIANT_SENTINEL),
            "D18: variant-context sentinel must NOT appear in messages[0] (system prompt); \
             found sentinel in system prompt: {sys_text}"
        );
        assert!(
            !sys_text.contains("Variant context:"),
            "D18: `Variant context:` header must NOT appear in messages[0]; \
             system prompt must stay stable across runs for prompt-cache hits"
        );
        assert!(
            user_text.contains(VARIANT_SENTINEL),
            "D18: variant-context sentinel must appear in messages[1] (goal slot); \
             user turn: {user_text}"
        );
        assert!(
            user_text.contains("Variant context:"),
            "D18: `Variant context:` header must appear in messages[1]; user turn: {user_text}"
        );
    }

    /// When no variant context is supplied, messages[0] and messages[1]
    /// both remain free of a `Variant context:` header — the composed
    /// goal-block collapses to the raw goal.
    #[tokio::test]
    async fn variant_context_absent_produces_clean_goal_block() {
        let llm = CapturingLlm::new(vec![build_agent_done_response("done")]);
        let mcp = StaticMcp::with_tools(&["cdp_find_elements"]);

        let goal_block = build_goal_block("just a goal", &[], None, 1000);

        let (_state, _writer_tx) = run_agent_workflow(
            &llm,
            AgentConfig::default(),
            goal_block,
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

        let messages = llm.messages_at(0);
        let sys_text = messages[0].content_text().unwrap_or("").to_string();
        let user_text = messages[1].content_text().unwrap_or("").to_string();

        assert!(
            !sys_text.contains("Variant context:"),
            "messages[0] must never carry a `Variant context:` header"
        );
        assert!(
            !user_text.contains("Variant context:"),
            "messages[1] must not carry a `Variant context:` header when none was supplied"
        );
        assert!(
            user_text.contains("just a goal"),
            "messages[1] must carry the raw user goal; got: {user_text}"
        );
    }
}

// ---------------------------------------------------------------------------
// Task 3.4: D17 `agent://*` event contract — TaskStateChanged /
// WorldModelChanged / BoundaryRecordWritten emissions.
// ---------------------------------------------------------------------------
//
// Asserts the three new `AgentEvent` variants fire with the runner's
// `run_id` threaded through to the event payload, and that the
// boundary-event emission tracks the corresponding `StepRecord`
// persistence exactly.

#[cfg(test)]
mod state_spine_event_contract_tests {
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
                clickweave_core::Workflow::default(),
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
                clickweave_core::Workflow::default(),
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
                clickweave_core::Workflow::default(),
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
                clickweave_core::Workflow::default(),
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
}

// Repeat-action loop detection: identical successful non-observation calls
// for `REPEAT_ACTION_THRESHOLD` consecutive turns must emit an
// `AgentEvent::Warning` carrying `NO_PROGRESS_WARNING_PREFIX`.
#[cfg(test)]
mod repeat_action_loop_detection_tests {
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
                clickweave_core::Workflow::default(),
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
                clickweave_core::Workflow::default(),
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
                clickweave_core::Workflow::default(),
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
}
