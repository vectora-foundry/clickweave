//! Rubric-10 gate for Phase 3b cutover (D-PR2 / Task 3b.0).
//!
//! This test covers the user-visible Tauri seam of the agent run:
//! the engine produces `AgentEvent`s, the Tauri forwarder persists
//! every event to `events.jsonl` and fans it out to a matching
//! `agent://*` topic. Because the actual `run_agent` command
//! constructs a real `LlmClient` and spawns an MCP subprocess, the
//! scripted smoke test drives the backend-of-Tauri surface directly:
//!
//! - calls `clickweave_engine::agent::run_agent_workflow` with the
//!   shared `ScriptedLlm` + `StaticMcp` stubs (mirrors what
//!   `run_agent` would do after MCP bring-up),
//! - drains the engine event channel through a channel-pump loop
//!   that invokes the exact same `forward_agent_event` helper and
//!   `RunStorage::append_agent_event` call the production spawn
//!   uses,
//! - captures `agent://*` emits via `tauri::test::mock_app()` +
//!   per-topic `listen_any` handlers,
//! - asserts emit count matches `AgentEvent` line count in
//!   `events.jsonl` (filtered to exclude `StepRecord` boundary
//!   writes, which live in the same file per Task 3a.6.5), and
//! - asserts the legacy `AgentState` wire-shape
//!   (`state.steps.len()` matches the scripted tool-call count and
//!   `state.terminal_reason` is `Completed`).
//!
//! Any future event-forwarding regression — a missing match arm on
//! a new `AgentEvent` variant, a dropped persistence call, a
//! divergent emit topic — fails this test.

use super::*;
use clickweave_engine::agent::test_stubs::{ScriptedLlm, StaticMcp, llm_reply_tool};
use clickweave_engine::agent::{AgentConfig, run_agent_workflow};
use std::sync::{Arc, Mutex};
use tauri::Listener;

/// Every `agent://*` topic `forward_agent_event` can emit. Listed
/// explicitly so the test panics loud if a new `AgentEvent` variant
/// is added without a matching topic — keep in sync with
/// `forward_agent_event`.
const AGENT_TOPICS: &[&str] = &[
    "agent://step",
    "agent://node_added",
    "agent://edge_added",
    "agent://error",
    "agent://warning",
    "agent://cdp_connected",
    "agent://step_failed",
    "agent://sub_action",
    "agent://completion_disagreement",
    "agent://consecutive_destructive_cap_hit",
    "agent://task_state_changed",
    "agent://world_model_changed",
    "agent://boundary_record_written",
    "agent://episodes_retrieved",
    "agent://episode_written",
    "agent://episode_promoted",
];

fn agent_event_line_count(events_path: &std::path::Path) -> usize {
    std::fs::read_to_string(events_path)
        .ok()
        .map(|raw| {
            raw.lines()
                .filter(|line| !line.is_empty())
                .filter(|line| {
                    serde_json::from_str::<serde_json::Value>(line)
                        .ok()
                        .and_then(|value| value.get("type").cloned())
                        .is_some()
                })
                .count()
        })
        .unwrap_or(0)
}

async fn wait_for_captured_count(captured: &Arc<Mutex<Vec<(String, String)>>>, expected: usize) {
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        loop {
            if captured.lock().unwrap().len() >= expected {
                break;
            }
            tokio::task::yield_now().await;
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("captured Tauri events in time");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn runner_output_forwarder_skips_drain_barrier_and_persists_events() {
    let app = tauri::test::mock_app();
    let handle = app.handle().clone();
    let run_id = uuid::Uuid::new_v4().to_string();

    let tmp = tempfile::tempdir().expect("tempdir");
    let project_name = "runner-output-forwarder";
    let mut storage_inner = clickweave_core::storage::RunStorage::new(tmp.path(), project_name);
    let exec_dir = storage_inner.begin_execution().expect("begin_execution");
    let events_path = tmp
        .path()
        .join(".clickweave")
        .join("runs")
        .join(project_name)
        .join(&exec_dir)
        .join("events.jsonl");
    let storage = Arc::new(Mutex::new(storage_inner));

    let (tx, mut rx) = tokio::sync::mpsc::channel::<RunnerOutput>(8);
    let forwarded: Arc<Mutex<Vec<AgentEvent>>> = Arc::new(Mutex::new(Vec::new()));
    let forwarded_for_task = Arc::clone(&forwarded);
    let storage_for_task = Arc::clone(&storage);
    let forwarder_task = tokio::spawn(async move {
        while let Some(output) = rx.recv().await {
            match output {
                RunnerOutput::Event(event) => {
                    let _ = storage_for_task.lock().unwrap().append_agent_event(&event);
                    forward_agent_event(&handle, &run_id, &event);
                    forwarded_for_task.lock().unwrap().push(event);
                }
                RunnerOutput::DrainBarrier { ack } => {
                    let _ = ack.send(());
                }
                RunnerOutput::SkillProposalNeeded { .. } => {}
            }
        }
    });

    let (ack_tx, ack_rx) = tokio::sync::oneshot::channel();
    tx.send(RunnerOutput::DrainBarrier { ack: ack_tx })
        .await
        .expect("send drain barrier");
    tokio::time::timeout(std::time::Duration::from_secs(1), ack_rx)
        .await
        .expect("drain barrier ack in time")
        .expect("drain barrier ack sender alive");
    assert_eq!(
        agent_event_line_count(&events_path),
        0,
        "DrainBarrier must not append an AgentEvent line",
    );

    tx.send(RunnerOutput::Event(AgentEvent::Warning {
        message: "synthetic warning".to_string(),
    }))
    .await
    .expect("send warning event");
    let (ack_tx, ack_rx) = tokio::sync::oneshot::channel();
    tx.send(RunnerOutput::DrainBarrier { ack: ack_tx })
        .await
        .expect("send second drain barrier");
    tokio::time::timeout(std::time::Duration::from_secs(1), ack_rx)
        .await
        .expect("second drain barrier ack in time")
        .expect("second drain barrier ack sender alive");

    drop(tx);
    forwarder_task.await.expect("forwarder joined");

    assert_eq!(
        agent_event_line_count(&events_path),
        1,
        "RunnerOutput::Event must append exactly one AgentEvent line",
    );
    assert!(
        matches!(
            forwarded.lock().unwrap().as_slice(),
            [AgentEvent::Warning { .. }]
        ),
        "only the durable event should be forwarded",
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn terminal_emit_waits_for_prior_runner_output_drain() {
    let app = tauri::test::mock_app();
    let handle = app.handle().clone();
    let run_id = uuid::Uuid::new_v4().to_string();

    let captured: Arc<Mutex<Vec<(String, String)>>> = Arc::new(Mutex::new(Vec::new()));
    for topic in ["agent://warning", "agent://complete"] {
        let captured = Arc::clone(&captured);
        handle.listen_any(topic, move |evt| {
            captured
                .lock()
                .unwrap()
                .push((topic.to_string(), evt.payload().to_string()));
        });
    }

    let (tx, mut rx) = tokio::sync::mpsc::channel::<RunnerOutput>(8);
    let forwarder_handle = handle.clone();
    let forwarder_run_id = run_id.clone();
    let forwarder_task = tokio::spawn(async move {
        while let Some(output) = rx.recv().await {
            match output {
                RunnerOutput::Event(event) => {
                    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                    forward_agent_event(&forwarder_handle, &forwarder_run_id, &event);
                }
                RunnerOutput::DrainBarrier { ack } => {
                    let _ = ack.send(());
                }
                RunnerOutput::SkillProposalNeeded { .. } => {}
            }
        }
    });

    tx.send(RunnerOutput::Event(AgentEvent::Warning {
        message: "queued before terminal".to_string(),
    }))
    .await
    .expect("send prior event");
    emit_after_agent_event_drain(
        &tx,
        &handle,
        "agent://complete",
        serde_json::json!({ "run_id": run_id, "summary": "done" }),
    )
    .await;
    drop(tx);
    forwarder_task.await.expect("forwarder joined");

    wait_for_captured_count(&captured, 2).await;
    let topics: Vec<String> = captured
        .lock()
        .unwrap()
        .iter()
        .map(|(topic, _)| topic.clone())
        .collect();
    assert_eq!(
        topics,
        vec![
            "agent://warning".to_string(),
            "agent://complete".to_string()
        ],
        "terminal emit must not outrun already queued per-step events",
    );
}

/// Rubric-10 gate (D-PR2): every `AgentEvent` the engine emits
/// must (1) reach `events.jsonl` and (2) route to exactly one
/// `agent://<topic>` via `forward_agent_event`. The scripted
/// scenario runs two tool calls and terminates on `agent_done`,
/// which produces a known-non-zero event stream (at minimum
/// `StepCompleted`; typically also `NodeAdded` / `EdgeAdded` /
/// `GoalComplete`). The test does not pin an exact event count —
/// it asserts emit and persistence counts are equal and both
/// non-empty, which catches any future missing-match-arm
/// regression.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn run_agent_emits_full_event_stream_and_persists_records() {
    // Guardrail: any future deadlock (runner hang, forwarder pump
    // never draining, Tauri listener never firing) must produce a
    // loud timeout rather than wedging CI. 60s is generous for a
    // fully stubbed scenario — the engine-side happy-path
    // equivalent finishes in ~50 ms.
    tokio::time::timeout(std::time::Duration::from_secs(60), run_smoke_test_body())
        .await
        .expect("smoke test must finish within 60s (deadlock / hang regression)");
}

async fn run_smoke_test_body() {
    // ── Arrange: mock Tauri AppHandle + per-topic capture ──────
    let app = tauri::test::mock_app();
    let handle = app.handle().clone();

    // `listen_any` subscribes on a specific topic; collecting to
    // a shared Vec gives us a post-run view of every forwarded
    // event. The GoalComplete + CompletionDisagreementResolved
    // variants intentionally do not show up here — those are
    // emitted by the run-agent task itself, not by this
    // forwarder.
    let captured: Arc<Mutex<Vec<(String, String)>>> = Arc::new(Mutex::new(Vec::new()));
    for topic in AGENT_TOPICS {
        let topic = topic.to_string();
        let captured = Arc::clone(&captured);
        handle.listen_any(topic.clone(), move |evt| {
            captured
                .lock()
                .unwrap()
                .push((topic.clone(), evt.payload().to_string()));
        });
    }

    // ── Arrange: scripted LLM + MCP stubs ──────────────────────
    // Two tool calls then agent_done. `cdp_find_elements` returns
    // an empty matches set, mirroring the stable fixture in the
    // engine-side end-to-end happy-path test.
    let llm = ScriptedLlm::new(vec![
        llm_reply_tool(
            "cdp_find_elements",
            serde_json::json!({"query": "", "max_results": 300}),
        ),
        llm_reply_tool("cdp_click", serde_json::json!({"uid": "1_0"})),
        llm_reply_tool(
            "agent_done",
            serde_json::json!({"summary": "rubric-10 smoke test"}),
        ),
    ]);
    let mcp = StaticMcp::with_tools(&["cdp_find_elements", "cdp_click"])
        .with_reply(
            "cdp_find_elements",
            r#"{"page_url":"about:blank","source":"cdp","matches":[]}"#,
        )
        .with_reply("cdp_click", "clicked");

    // ── Arrange: real RunStorage rooted at a tempdir ───────────
    let tmp = tempfile::tempdir().expect("tempdir");
    let project_name = "rubric-10-smoke";
    let mut storage_inner = clickweave_core::storage::RunStorage::new(tmp.path(), project_name);
    let exec_dir = storage_inner.begin_execution().expect("begin_execution");
    let events_path = tmp
        .path()
        .join(".clickweave")
        .join("runs")
        .join(project_name)
        .join(&exec_dir)
        .join("events.jsonl");
    let storage = Arc::new(Mutex::new(storage_inner));

    // ── Arrange: engine event channel + Tauri-forwarder pump ───
    let (event_tx, mut event_rx) = tokio::sync::mpsc::channel::<RunnerOutput>(64);
    let (approval_tx, _approval_rx) =
        tokio::sync::mpsc::channel::<(ApprovalRequest, tokio::sync::oneshot::Sender<bool>)>(1);
    let channels = AgentChannels {
        event_tx,
        approval_tx,
    };

    let run_id = uuid::Uuid::new_v4().to_string();
    let run_uuid: uuid::Uuid = run_id.parse().unwrap();

    // Forwarder pump: mirrors the production agent.rs body —
    // persist to `events.jsonl`, then call
    // `forward_agent_event`. Count forwarded events here so the
    // assertion does not depend on listener-dispatch latency.
    let forwarder_handle = handle.clone();
    let forwarder_run_id = run_id.clone();
    let forwarder_storage = Arc::clone(&storage);
    let forwarded: Arc<Mutex<Vec<AgentEvent>>> = Arc::new(Mutex::new(Vec::new()));
    let forwarded_for_task = Arc::clone(&forwarded);
    let forwarder_task = tokio::spawn(async move {
        while let Some(output) = event_rx.recv().await {
            let Some(event) = output.into_event() else {
                continue;
            };
            let _ = forwarder_storage.lock().unwrap().append_agent_event(&event);
            forward_agent_event(&forwarder_handle, &forwarder_run_id, &event);
            forwarded_for_task.lock().unwrap().push(event);
        }
    });

    // ── Act: drive the engine ──────────────────────────────────
    let (state, _writer_tx) = run_agent_workflow(
        &llm,
        AgentConfig::default(),
        "rubric-10 gate: forwarder + persistence contract".to_string(),
        &mcp,
        Some(channels),
        None,
        // Permission policy: `allow_all` so scripted destructive-ish
        // tool calls (cdp_click) don't block waiting on an approval
        // oneshot that nothing in this test answers. The production
        // agent.rs threads the operator's policy from the UI; this
        // smoke test only cares about event forwarding, so the
        // simplest shape that bypasses the approval gate is enough.
        Some(PermissionPolicy {
            allow_all: true,
            ..PermissionPolicy::default()
        }),
        run_uuid,
        None,
        None,
        Some(Arc::clone(&storage)),
        None,
        None,
    )
    .await
    .expect("run_agent_workflow ok");

    // Wait for the forwarder pump to drain (`event_tx` was dropped
    // when the workflow returned, so the recv loop exits cleanly).
    forwarder_task.await.expect("forwarder joined");

    // Give the Tauri listener task a scheduling window so the
    // per-topic capture vector observes every emit.
    tokio::task::yield_now().await;

    // ── Assert: legacy AgentState wire-shape ───────────────────
    assert_eq!(
        state.steps.len(),
        2,
        "scripted tool-call count (2) must match state.steps.len(); got {:?}",
        state.steps,
    );
    assert!(
        matches!(
            state.terminal_reason,
            Some(TerminalReason::Completed { ref summary })
                if summary == "rubric-10 smoke test"
        ),
        "terminal_reason must be Completed with the agent_done summary, got {:?}",
        state.terminal_reason,
    );
    assert!(
        state.completed,
        "state.completed must be true after agent_done terminal",
    );

    // ── Assert: forwarder touched every engine event ───────────
    let forwarded_events = forwarded.lock().unwrap();
    let forwarded_count = forwarded_events.len();
    assert!(
        forwarded_count > 0,
        "the forwarder must receive at least one AgentEvent from the engine",
    );

    // ── Assert: events.jsonl holds every forwarded event ───────
    // `events.jsonl` also contains StepRecord boundary writes
    // (Task 3a.6.5) and `AgentEvent::BoundaryRecordWritten`
    // AgentEvents (Task 3.4). Both shapes carry `boundary_kind`,
    // but only `AgentEvent` lines carry `serde(tag = "type")` —
    // filter on `type` presence so the count comparison is
    // apples-to-apples against the forwarded-event stream.
    let trace_raw = std::fs::read_to_string(&events_path)
        .unwrap_or_else(|e| panic!("read events.jsonl at {:?}: {}", events_path, e));
    let trace_json: Vec<serde_json::Value> = trace_raw
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| serde_json::from_str(l).expect("events.jsonl line is valid JSON"))
        .collect();
    let agent_event_lines: Vec<&serde_json::Value> = trace_json
        .iter()
        .filter(|v| v.get("type").is_some())
        .collect();
    assert_eq!(
        agent_event_lines.len(),
        forwarded_count,
        "events.jsonl AgentEvent line count ({}) must equal forwarded-event \
         count ({}); trace_raw={}",
        agent_event_lines.len(),
        forwarded_count,
        trace_raw,
    );

    // ── Assert: every forwarded event reached `agent://*` ──────
    // `GoalComplete` and `CompletionDisagreementResolved` are the
    // two variants `forward_agent_event` deliberately swallows
    // (terminal emission / Tauri-only origin), so subtract those
    // from the expected capture count. Every other forwarded
    // variant must produce exactly one `agent://<topic>` payload.
    let forwarder_silenced = forwarded_events
        .iter()
        .filter(|e| {
            matches!(
                e,
                AgentEvent::GoalComplete { .. } | AgentEvent::CompletionDisagreementResolved { .. }
            )
        })
        .count();
    let expected_emits = forwarded_count - forwarder_silenced;
    let captured_events = captured.lock().unwrap();
    assert_eq!(
        captured_events.len(),
        expected_emits,
        "every forwarded AgentEvent (minus GoalComplete / \
         CompletionDisagreementResolved) must produce exactly one \
         `agent://<topic>` emission — forwarded={}, silenced={}, \
         captured={:?}",
        forwarded_count,
        forwarder_silenced,
        captured_events,
    );

    // ── Assert: the run emitted a concrete `agent://step` ──────
    // A successful scripted scenario must pass through at least
    // one `StepCompleted` — that's the canonical user-visible
    // event the UI renders per step.
    assert!(
        captured_events
            .iter()
            .any(|(topic, _)| topic == "agent://step"),
        "at least one `agent://step` emission expected; captured={:?}",
        captured_events,
    );

    // Sanity: every captured event payload carries the run_id we
    // seeded. This pins the `event_run_id.clone()` pass-through
    // behaviour in `forward_agent_event` — a regression there
    // would silently strip the id from frontend-visible payloads.
    for (topic, payload) in captured_events.iter() {
        let parsed: serde_json::Value = serde_json::from_str(payload)
            .unwrap_or_else(|e| panic!("payload on {} is valid JSON: {}", topic, e));
        assert_eq!(
            parsed.get("run_id").and_then(|v| v.as_str()),
            Some(run_id.as_str()),
            "every `agent://*` payload must carry run_id={}; topic={}, payload={}",
            run_id,
            topic,
            payload,
        );
    }
}

/// F2 acceptance test: pin the exact top-level JSON keys for the
/// three Spec 2 D33 episodic events. The locked contract lives at
/// `docs/design/2026-04-24_agent-episodic-memory.md:699-701`. A
/// future drift on either the engine event variant fields or the
/// `forward_agent_event` payload shape must fail this test loud.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn forward_agent_event_emits_locked_episodic_payload_shapes() {
    use clickweave_engine::agent::ScopeBreakdown;
    use clickweave_engine::agent::episodic::{EpisodeScope, RetrievalTrigger};
    use std::collections::BTreeSet;
    use tauri::Listener;

    let app = tauri::test::mock_app();
    let handle = app.handle().clone();

    let captured: Arc<Mutex<Vec<(String, String)>>> = Arc::new(Mutex::new(Vec::new()));
    for topic in [
        "agent://episodes_retrieved",
        "agent://episode_written",
        "agent://episode_promoted",
    ] {
        let captured = Arc::clone(&captured);
        handle.listen_any(topic, move |evt| {
            captured
                .lock()
                .unwrap()
                .push((topic.to_string(), evt.payload().to_string()));
        });
    }

    let run_id = uuid::Uuid::new_v4();
    let run_id_str = run_id.to_string();

    let retrieved = AgentEvent::EpisodesRetrieved {
        run_id,
        trigger: RetrievalTrigger::RunStart,
        count: 2,
        episode_ids: vec!["ep_a".into(), "ep_b".into()],
        scope_breakdown: ScopeBreakdown {
            workflow: 1,
            global: 1,
        },
    };
    let written = AgentEvent::EpisodeWritten {
        run_id,
        outcome: "inserted".into(),
        episode_id: "ep_c".into(),
        scope: EpisodeScope::WorkflowLocal,
        occurrence_count: 1,
    };
    let promoted = AgentEvent::EpisodePromoted {
        run_id,
        promoted_episode_ids: vec!["ep_d".into()],
        skipped_count: 3,
    };

    forward_agent_event(&handle, &run_id_str, &retrieved);
    forward_agent_event(&handle, &run_id_str, &written);
    forward_agent_event(&handle, &run_id_str, &promoted);

    // Yield so listener tasks pick up the emits.
    for _ in 0..16 {
        tokio::task::yield_now().await;
    }

    let captured = captured.lock().unwrap();
    let by_topic = |t: &str| -> serde_json::Value {
        let raw = captured
            .iter()
            .find(|(topic, _)| topic == t)
            .unwrap_or_else(|| panic!("no emission on {} — captured={:?}", t, captured))
            .1
            .clone();
        serde_json::from_str(&raw).expect("payload is valid JSON")
    };
    let key_set = |v: &serde_json::Value| -> BTreeSet<String> {
        v.as_object()
            .expect("payload is an object")
            .keys()
            .cloned()
            .collect()
    };
    let expect_keys = |actual: BTreeSet<String>, want: &[&str], topic: &str| {
        let want_set: BTreeSet<String> = want.iter().map(|s| (*s).to_string()).collect();
        assert_eq!(
            actual, want_set,
            "{} payload must carry exactly the locked Spec 2 D33 keys",
            topic,
        );
    };

    let r = by_topic("agent://episodes_retrieved");
    // `event_run_id` is the harness-added forwarder echo of the
    // engine-side `run_id`; the spec contract is on the engine
    // payload's keys (which are the *other* fields). Both must be
    // present in the emit per the existing forwarder pattern.
    expect_keys(
        key_set(&r),
        &[
            "run_id",
            "event_run_id",
            "trigger",
            "count",
            "episode_ids",
            "scope_breakdown",
        ],
        "episodes_retrieved",
    );
    let breakdown = r.get("scope_breakdown").expect("scope_breakdown present");
    let breakdown_keys: BTreeSet<String> = breakdown
        .as_object()
        .expect("scope_breakdown is an object")
        .keys()
        .cloned()
        .collect();
    expect_keys(
        breakdown_keys,
        &["workflow", "global"],
        "episodes_retrieved.scope_breakdown",
    );

    let w = by_topic("agent://episode_written");
    expect_keys(
        key_set(&w),
        &[
            "run_id",
            "event_run_id",
            "outcome",
            "episode_id",
            "scope",
            "occurrence_count",
        ],
        "episode_written",
    );

    let p = by_topic("agent://episode_promoted");
    expect_keys(
        key_set(&p),
        &[
            "run_id",
            "event_run_id",
            "promoted_episode_ids",
            "skipped_count",
        ],
        "episode_promoted",
    );
    // `promoted_episode_ids` must carry the actual IDs, not be
    // collapsed to a count.
    let ids = p
        .get("promoted_episode_ids")
        .and_then(|v| v.as_array())
        .expect("promoted_episode_ids is an array");
    assert_eq!(
        ids.len(),
        1,
        "exactly one promoted ID expected from synthetic event",
    );
    assert_eq!(ids[0].as_str(), Some("ep_d"));
}
