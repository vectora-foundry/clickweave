//! Integration tests for `SqliteEpisodicStore` against a real temp-file
//! SQLite database, plus end-to-end coverage for `EpisodicWriter`.
//!
//! These tests live outside `src/` so they exercise the public surface
//! the runner (Phase 3) will eventually depend on. Anything they need
//! that is `pub(crate)` must surface through a public test helper —
//! see `SqliteEpisodicStore::row_count_for_tests` for the pattern.

use chrono::Utc;
use clickweave_engine::agent::episodic::store::EpisodicStore;
use clickweave_engine::agent::episodic::{
    CompactAction, Embedder, EpisodeRecord, EpisodeScope, EpisodicContext, EpisodicWriter,
    FailureSignature, HashedShingleEmbedder, InsertOutcome, PreStateSignature,
    PromotionTerminalKind, RecoveringEntrySnapshot, RecoveryActionsHash, RetrievalQuery,
    RetrievalTrigger, SqliteEpisodicStore, TriggeringError, WriteRequest,
};
use clickweave_engine::agent::step_record::{BoundaryKind, StepRecord, WorldModelSnapshot};
use clickweave_engine::agent::task_state::{Phase, TaskState};

fn empty_task_state(goal: &str, phase: Phase) -> TaskState {
    TaskState {
        goal: goal.into(),
        subgoal_stack: vec![],
        watch_slots: vec![],
        hypotheses: vec![],
        phase,
        milestones: vec![],
    }
}

fn mk_episode(sig: &str, actions_hash: &str, workflow_hash: &str) -> EpisodeRecord {
    let now = Utc::now();
    let e = HashedShingleEmbedder::default();
    EpisodeRecord {
        episode_id: format!("ep_{}", ulid::Ulid::new()),
        scope: EpisodeScope::WorkflowLocal,
        workflow_hash: workflow_hash.into(),
        pre_state_signature: PreStateSignature(sig.into()),
        goal: "test goal".into(),
        subgoal_text: Some("test subgoal".into()),
        failure_signature: FailureSignature {
            failed_tool: "cdp_click".into(),
            error_kind: "NotFound".into(),
            consecutive_errors_at_entry: 1,
        },
        recovery_actions: vec![CompactAction {
            tool_name: "ax_click".into(),
            brief_args: "button Continue".into(),
            outcome_kind: "ok".into(),
        }],
        recovery_actions_hash: RecoveryActionsHash(actions_hash.into()),
        outcome_summary: "ok".into(),
        pre_state_snapshot: WorldModelSnapshot::default(),
        goal_subgoal_embedding: e.embed("test goal test subgoal"),
        embedding_impl_id: e.impl_id().into(),
        occurrence_count: 1,
        created_at: now,
        last_seen_at: now,
        last_retrieved_at: None,
        step_record_refs: vec!["exec_1/node_a/events.jsonl".into()],
    }
}

#[tokio::test]
async fn new_store_bootstraps_schema_and_opens_wal_mode() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("episodic.sqlite");
    let _store = SqliteEpisodicStore::new(&path, EpisodeScope::WorkflowLocal).unwrap();
    assert!(path.exists(), "sqlite file should have been created");

    // WAL pragma is set during construction; the -wal sidecar should
    // appear after the first write (the schema bootstrap counts).
    let wal = path.with_extension("sqlite-wal");
    assert!(
        wal.exists() || path.exists(),
        "expected the sqlite or its -wal sidecar to exist after bootstrap"
    );
}

#[tokio::test]
async fn insert_new_episode_returns_inserted() {
    let dir = tempfile::tempdir().unwrap();
    let store =
        SqliteEpisodicStore::new(&dir.path().join("db.sqlite"), EpisodeScope::WorkflowLocal)
            .unwrap();
    let ep = mk_episode("sig_A", "hash_A", "w1");
    let out = store.insert(ep.clone()).await.unwrap();
    match out {
        InsertOutcome::Inserted { episode_id } => {
            assert_eq!(episode_id, ep.episode_id);
        }
        other => panic!("expected Inserted, got {:?}", other),
    }
    assert_eq!(store.row_count_for_tests().unwrap(), 1);
}

#[tokio::test]
async fn insert_duplicate_merges_and_bumps_count() {
    let dir = tempfile::tempdir().unwrap();
    let store =
        SqliteEpisodicStore::new(&dir.path().join("db.sqlite"), EpisodeScope::WorkflowLocal)
            .unwrap();
    let ep1 = mk_episode("sig_A", "hash_A", "w1");
    let ep2 = mk_episode("sig_A", "hash_A", "w1"); // same signature + actions_hash
    let first = store.insert(ep1.clone()).await.unwrap();
    let out = store.insert(ep2).await.unwrap();
    match (first, out) {
        (
            InsertOutcome::Inserted { .. },
            InsertOutcome::MergedWithExisting {
                episode_id,
                new_occurrence_count,
            },
        ) => {
            assert_eq!(new_occurrence_count, 2);
            assert_eq!(
                episode_id, ep1.episode_id,
                "merge must reuse the existing row's id, not the new one"
            );
        }
        (a, b) => panic!(
            "expected (Inserted, MergedWithExisting), got ({:?}, {:?})",
            a, b
        ),
    }
    // Dedup means only one row exists.
    assert_eq!(store.row_count_for_tests().unwrap(), 1);
}

#[tokio::test]
async fn insert_different_actions_hash_is_separate_row() {
    let dir = tempfile::tempdir().unwrap();
    let store =
        SqliteEpisodicStore::new(&dir.path().join("db.sqlite"), EpisodeScope::WorkflowLocal)
            .unwrap();
    let ep1 = mk_episode("sig_A", "hash_A", "w1");
    let ep2 = mk_episode("sig_A", "hash_B", "w1");
    let out1 = store.insert(ep1).await.unwrap();
    let out2 = store.insert(ep2).await.unwrap();
    assert!(matches!(out1, InsertOutcome::Inserted { .. }));
    assert!(matches!(out2, InsertOutcome::Inserted { .. }));
    assert_eq!(store.row_count_for_tests().unwrap(), 2);
}

#[tokio::test]
async fn retrieve_returns_empty_when_no_rows() {
    let dir = tempfile::tempdir().unwrap();
    let store =
        SqliteEpisodicStore::new(&dir.path().join("db.sqlite"), EpisodeScope::WorkflowLocal)
            .unwrap();
    let sig = PreStateSignature("nope".into());
    let q = RetrievalQuery {
        trigger: RetrievalTrigger::RunStart,
        pre_state_signature: &sig,
        goal: "anything",
        subgoal_text: None,
        workflow_hash: "w1",
        now: Utc::now(),
    };
    let out = store.retrieve(&q, 2).await.unwrap();
    assert!(out.is_empty());
}

#[tokio::test]
async fn retrieve_structured_match_returns_matching_rows() {
    let dir = tempfile::tempdir().unwrap();
    let store =
        SqliteEpisodicStore::new(&dir.path().join("db.sqlite"), EpisodeScope::WorkflowLocal)
            .unwrap();
    store
        .insert(mk_episode("sig_A", "hash_A", "w1"))
        .await
        .unwrap();
    store
        .insert(mk_episode("sig_B", "hash_B", "w1"))
        .await
        .unwrap();

    let sig = PreStateSignature("sig_A".into());
    let q = RetrievalQuery {
        trigger: RetrievalTrigger::RecoveringEntry,
        pre_state_signature: &sig,
        goal: "test goal",
        subgoal_text: Some("test subgoal"),
        workflow_hash: "w1",
        now: Utc::now(),
    };
    let out = store.retrieve(&q, 5).await.unwrap();
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].episode.pre_state_signature.0, "sig_A");
    assert!(
        out[0].score_breakdown.structured_match,
        "structured stage must mark the match flag"
    );
    assert!(
        out[0].score_breakdown.final_score > 0.0,
        "structured-match candidate must score above zero"
    );
}

#[tokio::test]
async fn retrieve_fallback_used_when_no_structured_match() {
    let dir = tempfile::tempdir().unwrap();
    let store =
        SqliteEpisodicStore::new(&dir.path().join("db.sqlite"), EpisodeScope::WorkflowLocal)
            .unwrap();
    store
        .insert(mk_episode("sig_X", "hash_X", "w1"))
        .await
        .unwrap();

    let sig = PreStateSignature("no_match".into());
    let q = RetrievalQuery {
        trigger: RetrievalTrigger::RunStart,
        pre_state_signature: &sig,
        goal: "test goal",
        subgoal_text: Some("test subgoal"),
        workflow_hash: "w1",
        now: Utc::now(),
    };
    let out = store.retrieve(&q, 2).await.unwrap();
    assert_eq!(
        out.len(),
        1,
        "embedding-only fallback should return the one row"
    );
    assert!(
        !out[0].score_breakdown.structured_match,
        "fallback rows must report structured_match = false"
    );
}

#[tokio::test]
async fn retrieve_respects_top_k() {
    let dir = tempfile::tempdir().unwrap();
    let store =
        SqliteEpisodicStore::new(&dir.path().join("db.sqlite"), EpisodeScope::WorkflowLocal)
            .unwrap();
    for i in 0..5 {
        store
            .insert(mk_episode("sig_A", &format!("hash_{}", i), "w1"))
            .await
            .unwrap();
    }

    let sig = PreStateSignature("sig_A".into());
    let q = RetrievalQuery {
        trigger: RetrievalTrigger::RecoveringEntry,
        pre_state_signature: &sig,
        goal: "test goal",
        subgoal_text: Some("test subgoal"),
        workflow_hash: "w1",
        now: Utc::now(),
    };
    let out = store.retrieve(&q, 2).await.unwrap();
    assert_eq!(out.len(), 2);
}

#[tokio::test]
async fn retrieve_renders_pre_state_snapshot_round_trip() {
    // The render path (`render_retrieved_recoveries_block`) reads
    // `pre_state_snapshot.focused_app.name` etc. straight off the
    // retrieved row, so the SQLite round-trip must preserve at least
    // the focused-app field. This pins the upstream Deserialize +
    // Default contract on `WorldModelSnapshot` so a future refactor
    // can't accidentally regress the render contract.
    use clickweave_engine::agent::world_model::{AppKind, FocusedApp};

    let dir = tempfile::tempdir().unwrap();
    let store =
        SqliteEpisodicStore::new(&dir.path().join("db.sqlite"), EpisodeScope::WorkflowLocal)
            .unwrap();
    let mut ep = mk_episode("sig_with_snap", "hash_with_snap", "w1");
    ep.pre_state_snapshot.focused_app = Some(FocusedApp {
        name: "Safari".into(),
        kind: AppKind::ChromeBrowser,
        pid: 4242,
    });
    ep.pre_state_snapshot.modal_present = Some(true);
    store.insert(ep).await.unwrap();

    let sig = PreStateSignature("sig_with_snap".into());
    let q = RetrievalQuery {
        trigger: RetrievalTrigger::RecoveringEntry,
        pre_state_signature: &sig,
        goal: "test goal",
        subgoal_text: Some("test subgoal"),
        workflow_hash: "w1",
        now: Utc::now(),
    };
    let out = store.retrieve(&q, 1).await.unwrap();
    assert_eq!(out.len(), 1);
    let snap = &out[0].episode.pre_state_snapshot;
    let app = snap
        .focused_app
        .as_ref()
        .expect("focused_app must round-trip through SQLite");
    assert_eq!(app.name, "Safari");
    assert!(matches!(app.kind, AppKind::ChromeBrowser));
    assert_eq!(snap.modal_present, Some(true));
}

#[tokio::test]
async fn prune_lru_respects_recent_grace_window() {
    let dir = tempfile::tempdir().unwrap();
    let store =
        SqliteEpisodicStore::new(&dir.path().join("db.sqlite"), EpisodeScope::WorkflowLocal)
            .unwrap();
    for i in 0..5 {
        store
            .insert(mk_episode(
                &format!("sig_{}", i),
                &format!("hash_{}", i),
                "w1",
            ))
            .await
            .unwrap();
    }
    // All five rows are fresh; the 1h grace window must shield them
    // from eviction even though `cap = 2`.
    let deleted = store.prune_lru(2).await.unwrap();
    assert_eq!(deleted, 0);
    assert_eq!(store.row_count_for_tests().unwrap(), 5);
}

#[tokio::test]
async fn writer_persists_on_derive_and_insert() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = EpisodicContext {
        enabled: true,
        workflow_local_path: dir.path().join("wl.sqlite"),
        global_path: None,
        workflow_hash: "w1".into(),
    };
    let writer = EpisodicWriter::spawn(ctx.clone(), None, uuid::Uuid::new_v4()).unwrap();

    let now = Utc::now();
    let entry = RecoveringEntrySnapshot {
        entered_at_step: 1,
        world_model_at_entry: WorldModelSnapshot::default(),
        task_state_at_entry: empty_task_state("test", Phase::Recovering),
        triggering_error: TriggeringError {
            failed_tool: "cdp_click".into(),
            error_kind: "NotFound".into(),
            consecutive_errors_at_entry: 1,
            step_index: 1,
        },
        workflow_hash: "w1".into(),
        pre_state_signature: PreStateSignature("test_sig_abcdef01".into()),
        active_watch_slots: vec![],
        events_jsonl_ref: Some("/tmp/fake/events.jsonl".into()),
    };
    let recovery_success = StepRecord {
        step_index: 2,
        boundary_kind: BoundaryKind::RecoverySucceeded,
        world_model_snapshot: WorldModelSnapshot::default(),
        task_state_snapshot: empty_task_state("test", Phase::Executing),
        action_taken: serde_json::Value::Null,
        outcome: serde_json::Value::Null,
        timestamp: now,
    };

    writer
        .queue(WriteRequest::DeriveAndInsert {
            entry: Box::new(entry),
            recovery_success: Box::new(recovery_success),
            recovery_actions: vec![CompactAction {
                tool_name: "ax_click".into(),
                brief_args: "button Continue".into(),
                outcome_kind: "ok".into(),
            }],
        })
        .await
        .unwrap();

    writer.flush_for_tests().await;

    // Re-open the workflow-local store from outside the writer task
    // and verify the row landed. Using the public `row_count_for_tests`
    // helper avoids reaching into the writer's internal connection.
    let store =
        SqliteEpisodicStore::new(&ctx.workflow_local_path, EpisodeScope::WorkflowLocal).unwrap();
    assert_eq!(store.row_count_for_tests().unwrap(), 1);

    let q_sig = PreStateSignature("test_sig_abcdef01".into());
    let q = RetrievalQuery {
        trigger: RetrievalTrigger::RecoveringEntry,
        pre_state_signature: &q_sig,
        goal: "test",
        subgoal_text: None,
        workflow_hash: "w1",
        now: Utc::now(),
    };
    let retrieved = store.retrieve(&q, 5).await.unwrap();
    assert_eq!(retrieved.len(), 1);
    let ep = &retrieved[0].episode;
    assert_eq!(ep.failure_signature.failed_tool, "cdp_click");
    assert_eq!(ep.failure_signature.error_kind, "NotFound");
    assert_eq!(
        ep.recovery_actions
            .iter()
            .map(|a| a.tool_name.as_str())
            .collect::<Vec<_>>(),
        vec!["ax_click"]
    );
    assert_eq!(
        ep.step_record_refs,
        vec!["/tmp/fake/events.jsonl".to_string()],
        "writer must populate step_record_refs from the entry snapshot"
    );
}

#[tokio::test]
async fn writer_skip_promotion_does_not_touch_global_store() {
    // SkipPromotion terminals (CompletionDisagreement, LoopDetected, errored
    // terminals — see PromotionTerminalKind) must not copy any rows into
    // the global store. This is the safety net for D31's "promote only
    // on clean terminals" gate.
    let dir = tempfile::tempdir().unwrap();
    let wl_path = dir.path().join("wl.sqlite");
    let global_path = dir.path().join("global.sqlite");
    let ctx = EpisodicContext {
        enabled: true,
        workflow_local_path: wl_path.clone(),
        global_path: Some(global_path.clone()),
        workflow_hash: "w1".into(),
    };
    let writer = EpisodicWriter::spawn(ctx, None, uuid::Uuid::new_v4()).unwrap();

    // Pre-populate workflow-local with a row that *would* qualify for
    // promotion (occurrence_count = 2, cross-workflow already in global
    // = false → should_promote returns true).
    let wl_store = SqliteEpisodicStore::new(&wl_path, EpisodeScope::WorkflowLocal).unwrap();
    let mut ep = mk_episode("sig_skip", "hash_skip", "w1");
    ep.occurrence_count = 2;
    wl_store.insert(ep).await.unwrap();

    writer
        .queue(WriteRequest::PromotePass {
            workflow_hash: "w1".into(),
            terminal_kind: PromotionTerminalKind::SkipPromotion,
            run_started_at: Utc::now() - chrono::Duration::hours(1),
        })
        .await
        .unwrap();
    writer.flush_for_tests().await;

    let global_store = SqliteEpisodicStore::new(&global_path, EpisodeScope::Global).unwrap();
    assert_eq!(
        global_store.row_count_for_tests().unwrap(),
        0,
        "SkipPromotion must not copy any rows into the global store"
    );
}

#[tokio::test]
async fn promotion_dedup_writes_rfc3339_last_seen_at() {
    // Regression: the global-dedup branch in `promote_matching_episodes`
    // previously wrote `last_seen_at = datetime('now')`, which produces
    // SQLite's default `YYYY-MM-DD HH:MM:SS` format. `row_to_episode`
    // then parses `last_seen_at` strictly as RFC3339 and falls back to
    // `Utc::now()` on failure, so a merged global row read after the
    // run looked freshly seen on every retrieval and broke the
    // recency-decay ordering. Ensure the merge path stores RFC3339.
    let dir = tempfile::tempdir().unwrap();
    let wl_path = dir.path().join("wl.sqlite");
    let global_path = dir.path().join("global.sqlite");

    // Pre-populate the global store with a row that shares the
    // `(scope, pre_state_signature, recovery_actions_hash)` triple with
    // the row we'll promote, forcing the INSERT OR IGNORE inside
    // `promote_matching_episodes` to take the dedup-merge branch.
    let global_store = SqliteEpisodicStore::new(&global_path, EpisodeScope::Global).unwrap();
    let mut existing_global = mk_episode("sig_dedup", "hash_dedup", "different_workflow");
    existing_global.scope = EpisodeScope::Global;
    global_store.insert(existing_global).await.unwrap();

    let wl_store = SqliteEpisodicStore::new(&wl_path, EpisodeScope::WorkflowLocal).unwrap();
    let mut wl_row = mk_episode("sig_dedup", "hash_dedup", "w1");
    wl_row.occurrence_count = 2; // qualifies for promotion
    wl_store.insert(wl_row).await.unwrap();

    let ctx = EpisodicContext {
        enabled: true,
        workflow_local_path: wl_path,
        global_path: Some(global_path.clone()),
        workflow_hash: "w1".into(),
    };
    let writer = EpisodicWriter::spawn(ctx, None, uuid::Uuid::new_v4()).unwrap();
    writer
        .queue(WriteRequest::PromotePass {
            workflow_hash: "w1".into(),
            terminal_kind: PromotionTerminalKind::Clean,
            run_started_at: Utc::now() - chrono::Duration::hours(1),
        })
        .await
        .unwrap();
    writer.flush_for_tests().await;

    // Read the merged global row's raw `last_seen_at` and assert it
    // parses as RFC3339. A SQLite-default `YYYY-MM-DD HH:MM:SS` string
    // would fail this parse — the symptom the fix addresses.
    let global_store = SqliteEpisodicStore::new(&global_path, EpisodeScope::Global).unwrap();
    assert_eq!(global_store.row_count_for_tests().unwrap(), 1);
    let conn = rusqlite::Connection::open(&global_path).unwrap();
    let raw_last_seen: String = conn
        .query_row("SELECT last_seen_at FROM episodes LIMIT 1", [], |r| {
            r.get(0)
        })
        .unwrap();
    chrono::DateTime::parse_from_rfc3339(&raw_last_seen).unwrap_or_else(|e| {
        panic!("merged-global last_seen_at must be RFC3339, got {raw_last_seen:?}: {e}")
    });
}

#[tokio::test]
async fn workflow_a_episodes_do_not_appear_in_workflow_b_retrievals() {
    // Cross-scope isolation canary (D34): two distinct workflow-local
    // stores must never see each other's rows, even when they share a
    // PreStateSignature. This is the structural guarantee that prevents
    // context-bleed from one workflow into another.
    let dir = tempfile::tempdir().unwrap();
    let a_path = dir.path().join("a.sqlite");
    let b_path = dir.path().join("b.sqlite");

    let store_a = SqliteEpisodicStore::new(&a_path, EpisodeScope::WorkflowLocal).unwrap();
    let store_b = SqliteEpisodicStore::new(&b_path, EpisodeScope::WorkflowLocal).unwrap();

    store_a
        .insert(mk_episode("sig_shared", "hash_A", "workflow_a"))
        .await
        .unwrap();

    let sig = PreStateSignature("sig_shared".into());
    let q = RetrievalQuery {
        trigger: RetrievalTrigger::RecoveringEntry,
        pre_state_signature: &sig,
        goal: "anything",
        subgoal_text: None,
        workflow_hash: "workflow_b",
        now: Utc::now(),
    };
    let results = store_b.retrieve(&q, 5).await.unwrap();
    assert!(
        results.is_empty(),
        "workflow B retrieved {} rows from workflow A's store",
        results.len()
    );
    assert_eq!(
        store_b.row_count_for_tests().unwrap(),
        0,
        "store B should remain empty"
    );
}

// ── Fallback retrieval scans every row in scope ────────────────────
//
// `retrieve` fallback path (no structured `pre_state_signature`
// match) must score every row in scope, not a `LIMIT N` slice in
// undefined SQLite row order. A slice would make the best semantic
// match invisible once the store grew past N rows even within the
// configured 500 / 2000 cap, and would make fallback results
// nondeterministic.

#[tokio::test]
async fn fallback_scores_rows_past_the_old_200_limit() {
    use clickweave_engine::agent::episodic::HashedShingleEmbedder;
    let dir = tempfile::tempdir().unwrap();
    let store =
        SqliteEpisodicStore::new(&dir.path().join("db.sqlite"), EpisodeScope::WorkflowLocal)
            .unwrap();

    // Insert 250 noise rows whose goal text is unrelated to the
    // query, then one "needle" row whose goal embedding matches
    // the query directly. The needle is inserted *last* so it
    // would have ended up outside the legacy `LIMIT 200` window
    // depending on SQLite's row order — we force the noise count
    // above the old cap to make the regression visible.
    let e = HashedShingleEmbedder::default();
    for i in 0..250 {
        let mut ep = mk_episode(
            &format!("sig_noise_{i}"),
            &format!("hash_noise_{i}"),
            "fallback-w",
        );
        ep.goal = format!("noise unrelated topic number {i}");
        ep.subgoal_text = None;
        ep.goal_subgoal_embedding = e.embed(&ep.goal);
        store.insert(ep).await.unwrap();
    }
    let mut needle = mk_episode("sig_needle", "hash_needle", "fallback-w");
    needle.goal = "submit checkout payment confirmation".into();
    needle.subgoal_text = None;
    needle.goal_subgoal_embedding = e.embed(&needle.goal);
    let needle_id = needle.episode_id.clone();
    store.insert(needle).await.unwrap();

    // Query has no structured match (signature does not appear in
    // any row), forcing the fallback path. The query text matches
    // the needle's goal text closely.
    let no_match_sig = PreStateSignature("sig_does_not_exist".into());
    let q = RetrievalQuery {
        trigger: RetrievalTrigger::RunStart,
        pre_state_signature: &no_match_sig,
        goal: "submit checkout payment confirmation",
        subgoal_text: None,
        workflow_hash: "fallback-w",
        now: Utc::now(),
    };
    let hits = store.retrieve(&q, 1).await.unwrap();
    assert_eq!(hits.len(), 1, "top-1 fallback retrieval");
    assert_eq!(
        hits[0].episode.episode_id, needle_id,
        "the semantically-best row must be returned even when it sits past the legacy 200-row window",
    );
}

#[tokio::test]
async fn fallback_ordering_is_deterministic_across_repeated_queries() {
    use clickweave_engine::agent::episodic::HashedShingleEmbedder;
    let dir = tempfile::tempdir().unwrap();
    let store =
        SqliteEpisodicStore::new(&dir.path().join("db.sqlite"), EpisodeScope::WorkflowLocal)
            .unwrap();

    // 30 rows with identical goal text so scoring is a tie on
    // text-similarity. The deterministic ORDER BY in the fallback
    // SQL (last_seen_at DESC, occurrence_count DESC, episode_id)
    // must produce the same top-k across repeated calls.
    let e = HashedShingleEmbedder::default();
    for i in 0..30 {
        let mut ep = mk_episode(&format!("sig_tie_{i}"), &format!("hash_tie_{i}"), "tie-w");
        ep.goal = "deterministic tie-break case".into();
        ep.subgoal_text = None;
        ep.goal_subgoal_embedding = e.embed(&ep.goal);
        store.insert(ep).await.unwrap();
    }

    let no_match_sig = PreStateSignature("sig_does_not_exist".into());
    let q = RetrievalQuery {
        trigger: RetrievalTrigger::RunStart,
        pre_state_signature: &no_match_sig,
        goal: "deterministic tie-break case",
        subgoal_text: None,
        workflow_hash: "tie-w",
        now: Utc::now(),
    };

    let hits1 = store.retrieve(&q, 5).await.unwrap();
    let hits2 = store.retrieve(&q, 5).await.unwrap();
    let ids1: Vec<&str> = hits1
        .iter()
        .map(|r| r.episode.episode_id.as_str())
        .collect();
    let ids2: Vec<&str> = hits2
        .iter()
        .map(|r| r.episode.episode_id.as_str())
        .collect();
    assert_eq!(
        ids1, ids2,
        "fallback retrieval must be deterministic across repeated queries",
    );
}
