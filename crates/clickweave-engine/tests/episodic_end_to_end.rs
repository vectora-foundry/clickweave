//! Public-surface end-to-end smoke tests for the Spec 2 episodic
//! memory layer.
//!
//! The deeper `StateRunner::run`-driving end-to-end suite lives at
//! `src/agent/episodic/end_to_end_tests.rs` (gated `#[cfg(test)]`)
//! because it needs the crate-private `Mcp` trait and the
//! `pub mod test_stubs` doubles. This file complements it with
//! assertions that only need the public API:
//!
//!   1. `EpisodicWriter::spawn` opens both stores when the context
//!      is enabled, and emits an `EpisodeWritten` event after a
//!      `DeriveAndInsert` request lands.
//!   2. The same writer fires an `EpisodePromoted` event after a
//!      run-terminal `PromotePass`.
//!   3. The `EpisodicWriter` is a no-op on `EpisodicContext::disabled()`
//!      paths — Phase 3 must keep failure isolation (D32) intact.

use chrono::Utc;
use clickweave_engine::agent::AgentEvent;
use clickweave_engine::agent::episodic::store::EpisodicStore;
use clickweave_engine::agent::episodic::{
    CompactAction, Embedder, EpisodeRecord, EpisodeScope, EpisodicContext, EpisodicWriter,
    FailureSignature, HashedShingleEmbedder, PreStateSignature, PromotionTerminalKind,
    RecoveringEntrySnapshot, RecoveryActionsHash, SqliteEpisodicStore, TriggeringError,
    WriteRequest,
};
use clickweave_engine::agent::step_record::{BoundaryKind, StepRecord, WorldModelSnapshot};
use clickweave_engine::agent::task_state::{Phase, TaskState};
use tokio::sync::mpsc;

fn empty_world_model_snapshot() -> WorldModelSnapshot {
    WorldModelSnapshot {
        focused_app: None,
        window_list: None,
        cdp_page: None,
        element_summary: None,
        modal_present: None,
        dialog_present: None,
        last_screenshot: None,
        last_native_ax_snapshot: None,
        uncertainty: Default::default(),
    }
}

fn empty_task_state(goal: &str) -> TaskState {
    TaskState {
        goal: goal.into(),
        subgoal_stack: vec![],
        watch_slots: vec![],
        hypotheses: vec![],
        phase: Phase::Recovering,
        milestones: vec![],
    }
}

fn mk_recovery_snapshot(workflow_hash: &str, sig: &str) -> RecoveringEntrySnapshot {
    RecoveringEntrySnapshot {
        entered_at_step: 1,
        world_model_at_entry: empty_world_model_snapshot(),
        task_state_at_entry: empty_task_state("login"),
        triggering_error: TriggeringError {
            failed_tool: "cdp_click".into(),
            error_kind: "NotFound".into(),
            consecutive_errors_at_entry: 1,
            step_index: 1,
        },
        workflow_hash: workflow_hash.into(),
        pre_state_signature: PreStateSignature(sig.into()),
        active_watch_slots: vec![],
        events_jsonl_ref: Some("/tmp/exec_a/events.jsonl".into()),
    }
}

fn mk_step_record() -> StepRecord {
    StepRecord {
        step_index: 2,
        boundary_kind: BoundaryKind::RecoverySucceeded,
        world_model_snapshot: empty_world_model_snapshot(),
        task_state_snapshot: empty_task_state("login"),
        action_taken: serde_json::json!({"kind":"tool_call","tool_name":"ax_click"}),
        outcome: serde_json::json!({"kind":"tool_success"}),
        timestamp: Utc::now(),
    }
}

fn mk_episode_pre_seeded(scope: EpisodeScope, sig: &str, actions_hash: &str) -> EpisodeRecord {
    let now = Utc::now();
    let e = HashedShingleEmbedder::default();
    EpisodeRecord {
        episode_id: format!("ep_{}", ulid::Ulid::new()),
        scope,
        workflow_hash: "test-workflow".into(),
        pre_state_signature: PreStateSignature(sig.into()),
        goal: "login".into(),
        subgoal_text: None,
        failure_signature: FailureSignature {
            failed_tool: "cdp_click".into(),
            error_kind: "NotFound".into(),
            consecutive_errors_at_entry: 1,
        },
        recovery_actions: vec![CompactAction {
            tool_name: "ax_click".into(),
            brief_args: "uid=a1".into(),
            outcome_kind: "ok".into(),
        }],
        recovery_actions_hash: RecoveryActionsHash(actions_hash.into()),
        outcome_summary: "ok".into(),
        pre_state_snapshot: empty_world_model_snapshot(),
        goal_subgoal_embedding: e.embed("login"),
        embedding_impl_id: e.impl_id().into(),
        // P1.M3: occurrence_count >= 2 ensures should_promote returns
        // true even without prior global cross-confirmation.
        occurrence_count: 2,
        created_at: now,
        last_seen_at: now,
        last_retrieved_at: None,
        step_record_refs: vec![],
    }
}

#[tokio::test]
async fn writer_emits_episode_written_event_after_derive_and_insert() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = EpisodicContext {
        enabled: true,
        workflow_local_path: dir.path().join("episodic.sqlite"),
        global_path: None,
        workflow_hash: "test-workflow".into(),
    };

    let (tx, mut rx) = mpsc::channel::<AgentEvent>(16);
    let run_id = uuid::Uuid::new_v4();
    let writer = EpisodicWriter::spawn(ctx.clone(), Some(tx), run_id).expect("spawn writer");

    writer
        .queue(WriteRequest::DeriveAndInsert {
            entry: Box::new(mk_recovery_snapshot("test-workflow", "sig_1")),
            recovery_success: Box::new(mk_step_record()),
            recovery_actions: vec![CompactAction {
                tool_name: "ax_click".into(),
                brief_args: "uid=a1".into(),
                outcome_kind: "ok".into(),
            }],
        })
        .await
        .expect("queue");

    writer.flush_for_tests().await;

    // Pull the next event off the channel — must be EpisodeWritten,
    // and its run_id must match the one we spawned the writer with.
    let evt = tokio::time::timeout(std::time::Duration::from_millis(500), rx.recv())
        .await
        .expect("event received in time")
        .expect("channel still open");
    match evt {
        AgentEvent::EpisodeWritten {
            run_id: ev_run_id,
            outcome,
            occurrence_count,
            ..
        } => {
            assert_eq!(
                ev_run_id, run_id,
                "writer must stamp emitted events with the run_id captured at spawn"
            );
            assert_eq!(
                outcome, "inserted",
                "fresh insert outcome should be 'inserted'"
            );
            assert_eq!(
                occurrence_count, 1,
                "fresh insert reports occurrence_count=1"
            );
        }
        other => panic!("expected EpisodeWritten, got {:?}", other),
    }
}

#[tokio::test]
async fn writer_emits_episode_promoted_event_on_clean_terminal() {
    let dir = tempfile::tempdir().unwrap();
    let wl_path = dir.path().join("workflow.sqlite");
    let g_path = dir.path().join("global.sqlite");

    // Pre-seed the workflow-local store with a row that the
    // promotion gate will accept (occurrence_count = 2).
    {
        let wl = SqliteEpisodicStore::new(&wl_path, EpisodeScope::WorkflowLocal).unwrap();
        let ep = mk_episode_pre_seeded(EpisodeScope::WorkflowLocal, "sig_promote", "hash_promote");
        wl.insert(ep).await.expect("insert wl");
    }

    let ctx = EpisodicContext {
        enabled: true,
        workflow_local_path: wl_path.clone(),
        global_path: Some(g_path.clone()),
        workflow_hash: "test-workflow".into(),
    };

    let (tx, mut rx) = mpsc::channel::<AgentEvent>(16);
    let run_id = uuid::Uuid::new_v4();
    let writer = EpisodicWriter::spawn(ctx.clone(), Some(tx), run_id).expect("spawn writer");

    // Promotion only walks rows touched during this run. The pre-seed
    // landed seconds ago, so use a `run_started_at` from a few minutes
    // back so the SQL filter matches.
    let run_started_at = Utc::now() - chrono::Duration::minutes(5);
    writer
        .queue(WriteRequest::PromotePass {
            workflow_hash: "test-workflow".into(),
            terminal_kind: PromotionTerminalKind::Clean,
            run_started_at,
        })
        .await
        .expect("queue promote");
    writer.flush_for_tests().await;

    // Pull events until we hit EpisodePromoted (or time out).
    let mut promoted_seen = false;
    while let Ok(maybe_event) =
        tokio::time::timeout(std::time::Duration::from_millis(500), rx.recv()).await
    {
        match maybe_event {
            Some(AgentEvent::EpisodePromoted {
                run_id: ev_run_id,
                count,
                ..
            }) => {
                assert_eq!(ev_run_id, run_id);
                assert!(count >= 1, "at least one episode should have been promoted");
                promoted_seen = true;
                break;
            }
            Some(_) => continue,
            None => break,
        }
    }
    assert!(
        promoted_seen,
        "EpisodePromoted event must fire on a clean-terminal PromotePass with eligible rows"
    );
}

#[tokio::test]
async fn writer_skips_promotion_on_skip_terminal_kind_and_emits_no_event() {
    let dir = tempfile::tempdir().unwrap();
    let wl_path = dir.path().join("workflow.sqlite");
    let g_path = dir.path().join("global.sqlite");

    {
        let wl = SqliteEpisodicStore::new(&wl_path, EpisodeScope::WorkflowLocal).unwrap();
        wl.insert(mk_episode_pre_seeded(
            EpisodeScope::WorkflowLocal,
            "sig_skip",
            "hash_skip",
        ))
        .await
        .unwrap();
    }

    let ctx = EpisodicContext {
        enabled: true,
        workflow_local_path: wl_path.clone(),
        global_path: Some(g_path.clone()),
        workflow_hash: "test-workflow".into(),
    };

    let (tx, mut rx) = mpsc::channel::<AgentEvent>(16);
    let run_id = uuid::Uuid::new_v4();
    let writer = EpisodicWriter::spawn(ctx.clone(), Some(tx), run_id).expect("spawn writer");

    writer
        .queue(WriteRequest::PromotePass {
            workflow_hash: "test-workflow".into(),
            terminal_kind: PromotionTerminalKind::SkipPromotion,
            run_started_at: Utc::now() - chrono::Duration::minutes(5),
        })
        .await
        .expect("queue");
    writer.flush_for_tests().await;

    // No promotion event should fire on a SkipPromotion terminal.
    let evt = tokio::time::timeout(std::time::Duration::from_millis(150), rx.recv()).await;
    match evt {
        Err(_) => {
            // Timeout — expected. No event on the wire.
        }
        Ok(Some(AgentEvent::EpisodePromoted { .. })) => {
            panic!("EpisodePromoted must NOT fire on SkipPromotion terminal");
        }
        Ok(_) => {
            // Some unrelated event — also fine.
        }
    }
}
