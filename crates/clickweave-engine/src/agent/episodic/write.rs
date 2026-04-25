//! Async writer task and promotion logic (D30, D31, D38).
//!
//! Phase 2 wires up the consumer half of the runner-to-store channel:
//! - `EpisodicWriter::spawn` opens the workflow-local (and optional
//!   global) `SqliteEpisodicStore`s and starts a tokio task that drains
//!   `WriteRequest`s.
//! - `DeriveAndInsert` derives an `EpisodeRecord` from the
//!   `RecoveringEntrySnapshot` captured at `Recovering` entry plus the
//!   `RecoverySucceeded` `StepRecord` from the matching exit, then
//!   inserts via the store's dedup-aware `insert`.
//! - `PromotePass` is run-terminal — on a clean terminal (D31 gate) it
//!   walks workflow-local rows touched during this run, applies
//!   `should_promote`, and copies qualifying rows into the global store.
//!
//! Failure isolation (D32): every step uses `unwrap_or_default()` or
//! `.ok()` swallowing — the writer task never panics, never propagates
//! errors back to the runner. Channel back-pressure surfaces as
//! `EpisodicError::Backpressure` to the queuer; the runner drops the
//! request and continues the agent loop unaffected.

#![allow(dead_code)]

use std::sync::Arc;

use chrono::Utc;
use tokio::sync::mpsc;
use ulid::Ulid;

use crate::agent::episodic::embedder::{Embedder, HashedShingleEmbedder};
use crate::agent::episodic::promotion::should_promote;
use crate::agent::episodic::store::{EpisodicStore, SqliteEpisodicStore, join_err, lock_conn};
use crate::agent::episodic::types::{
    CompactAction, EpisodeRecord, EpisodeScope, EpisodicContext, EpisodicError, FailureSignature,
    InsertOutcome, PromotionTerminalKind, RecoveringEntrySnapshot, RecoveryActionsHash,
    WriteRequest,
};

/// Bounded channel capacity. `64` is enough headroom that bursty
/// `Recovering` entries during a flaky run still fit, but small enough
/// that runaway producers surface as `Backpressure` quickly instead of
/// silently growing memory.
const CHANNEL_CAP: usize = 64;

pub struct EpisodicWriter {
    tx: mpsc::Sender<WriteRequest>,
    /// Held so the consumer task is dropped (and aborts) when the
    /// writer is dropped. The `JoinHandle` is otherwise unused; the
    /// task is fire-and-forget.
    #[allow(dead_code)]
    join: tokio::task::JoinHandle<()>,
    /// Notified after every drained request, so test helpers can wait
    /// for the consumer to catch up without polling channel internals.
    flush: Arc<tokio::sync::Notify>,
}

impl EpisodicWriter {
    /// Spawn the consumer task. `event_tx`, when `Some`, will receive
    /// `EpisodeWritten` / `EpisodePromoted` emissions once Phase 3
    /// wires the corresponding `AgentEvent` variants through the
    /// runner. Phase 2 keeps the parameter on the signature for
    /// forward compatibility with that wiring; the per-request
    /// emission paths inside the consumer are no-ops today (see the
    /// inline TODOs).
    /// `run_id` is the runner's active-run UUID; it is captured at
    /// spawn time so emitted events pass the frontend's stale-run
    /// filter even after the runner moves on.
    pub fn spawn(
        ctx: EpisodicContext,
        event_tx: Option<mpsc::Sender<crate::agent::types::AgentEvent>>,
        run_id: uuid::Uuid,
    ) -> Result<Self, EpisodicError> {
        let (tx, mut rx) = mpsc::channel::<WriteRequest>(CHANNEL_CAP);
        let flush = Arc::new(tokio::sync::Notify::new());

        let wl = Arc::new(SqliteEpisodicStore::new(
            &ctx.workflow_local_path,
            EpisodeScope::WorkflowLocal,
        )?);
        let global: Option<Arc<SqliteEpisodicStore>> = match &ctx.global_path {
            Some(p) => Some(Arc::new(SqliteEpisodicStore::new(p, EpisodeScope::Global)?)),
            None => None,
        };

        let flush_signal = flush.clone();
        let event_tx_task = event_tx.clone();
        let join = tokio::spawn(async move {
            while let Some(req) = rx.recv().await {
                match req {
                    WriteRequest::DeriveAndInsert {
                        entry,
                        recovery_success,
                        recovery_actions,
                    } => {
                        match handle_derive_and_insert(
                            &wl,
                            *entry,
                            *recovery_success,
                            recovery_actions,
                        )
                        .await
                        {
                            Ok(outcome) => {
                                if let Some(tx) = &event_tx_task {
                                    let event = event_from_insert_outcome(run_id, outcome);
                                    let _ = tx.send(event).await;
                                }
                            }
                            Err(e) => {
                                tracing::warn!(error = %e, "episodic: derive_and_insert failed")
                            }
                        }
                    }
                    WriteRequest::PromotePass {
                        workflow_hash,
                        terminal_kind,
                        run_started_at,
                    } => {
                        if matches!(terminal_kind, PromotionTerminalKind::SkipPromotion) {
                            flush_signal.notify_waiters();
                            continue;
                        }
                        if let Some(g) = &global {
                            match promote_matching_episodes(&wl, g, &workflow_hash, run_started_at)
                                .await
                            {
                                Ok((promoted, skipped)) => {
                                    if let Some(tx) = &event_tx_task {
                                        let event =
                                            crate::agent::types::AgentEvent::EpisodePromoted {
                                                run_id,
                                                count: promoted.len(),
                                                skipped,
                                            };
                                        let _ = tx.send(event).await;
                                    }
                                }
                                Err(e) => {
                                    tracing::warn!(error = %e, "episodic: promotion pass failed")
                                }
                            }
                        }
                    }
                }
                flush_signal.notify_waiters();
            }
            flush_signal.notify_waiters();
        });

        Ok(Self { tx, join, flush })
    }

    /// Best-effort enqueue. Returns `EpisodicError::Backpressure` when
    /// the channel is full so the runner can drop the request without
    /// blocking the agent loop.
    pub async fn queue(&self, req: WriteRequest) -> Result<(), EpisodicError> {
        self.tx
            .try_send(req)
            .map_err(|_| EpisodicError::Backpressure)
    }

    /// Best-effort flush — block briefly until the consumer signals it
    /// has finished draining the queue. Bounded loop; safe to time-out
    /// silently because episodic writes are best-effort by D32.
    pub async fn flush(&self) {
        // Wait until the channel is fully drained (capacity == cap).
        // Cap the wait at ~1 second so a stuck consumer never blocks
        // the run-terminal path indefinitely.
        for _ in 0..50 {
            if self.tx.capacity() == CHANNEL_CAP {
                return;
            }
            // Use the notify so we wake on every drained request;
            // the timeout guards against a hung consumer.
            let _ =
                tokio::time::timeout(std::time::Duration::from_millis(20), self.flush.notified())
                    .await;
        }
    }

    /// Test alias for [`Self::flush`]. Kept as a separate entry point
    /// (rather than a `#[cfg(test)]`-gated helper) so integration tests
    /// in `tests/` — which compile as a separate crate where
    /// `cfg(test)` items are not visible — can call it. Production
    /// code paths use [`Self::flush`] directly.
    pub async fn flush_for_tests(&self) {
        self.flush().await
    }
}

/// Build an `EpisodeRecord` from the recovery-window snapshot and
/// insert it into the workflow-local store. Returns the dedup-aware
/// `InsertOutcome` so the writer task can emit the appropriate
/// `EpisodeWritten` event with `outcome = "inserted" | "merged"` plus
/// the row's `occurrence_count`.
async fn handle_derive_and_insert(
    wl: &Arc<SqliteEpisodicStore>,
    entry: RecoveringEntrySnapshot,
    _recovery_success: crate::agent::step_record::StepRecord,
    recovery_actions: Vec<CompactAction>,
) -> Result<InsertOutcome, EpisodicError> {
    // P1.C2: the runner computes the signature at snapshot-capture time
    // using the same `compute_pre_state_signature` retrieval uses, so
    // reads and writes share a single source of truth. Re-deriving here
    // would yield a different value (`WorldModelSnapshot` is a lossy
    // projection) and every future exact-match query would miss.
    let sig = entry.pre_state_signature.clone();

    let embedder = HashedShingleEmbedder::default();
    let goal = entry.task_state_at_entry.goal.clone();
    let subgoal_text = entry
        .task_state_at_entry
        .subgoal_stack
        .last()
        .map(|s| s.text.clone());
    let query_text = match &subgoal_text {
        Some(s) => format!("{} {}", goal, s),
        None => goal.clone(),
    };
    let embedding = embedder.embed(&query_text);

    let actions_hash = RecoveryActionsHash({
        let mut h = blake3::Hasher::new();
        for a in &recovery_actions {
            h.update(a.tool_name.as_bytes());
            h.update(b"\x1f");
            h.update(a.brief_args.as_bytes());
            h.update(b"\x1e");
        }
        h.finalize().to_hex().as_str()[..16].to_string()
    });

    let now = Utc::now();
    let record = EpisodeRecord {
        episode_id: format!("ep_{}", Ulid::new()),
        scope: EpisodeScope::WorkflowLocal,
        workflow_hash: entry.workflow_hash,
        pre_state_signature: sig,
        goal,
        subgoal_text,
        failure_signature: FailureSignature {
            failed_tool: entry.triggering_error.failed_tool,
            error_kind: entry.triggering_error.error_kind,
            consecutive_errors_at_entry: entry.triggering_error.consecutive_errors_at_entry,
        },
        recovery_actions,
        recovery_actions_hash: actions_hash,
        outcome_summary: "subgoal completed after recovery".into(),
        pre_state_snapshot: entry.world_model_at_entry,
        goal_subgoal_embedding: embedding,
        embedding_impl_id: embedder.impl_id().into(),
        occurrence_count: 1,
        created_at: now,
        last_seen_at: now,
        last_retrieved_at: None,
        // P1.H3: populate with the events.jsonl ref captured at
        // snapshot time so D36's orphan-ref sweep has something to
        // resolve.
        step_record_refs: entry.events_jsonl_ref.clone().into_iter().collect(),
    };

    wl.insert(record).await
}

/// Translate an [`InsertOutcome`] into an [`AgentEvent::EpisodeWritten`]
/// payload. `Inserted` and `MergedWithExisting` both surface as a
/// single emission so frontends only have to handle one event shape;
/// `outcome` distinguishes them. `Dropped` collapses to a 0-occurrence
/// emission so subscribers can still observe the writer's decision —
/// the runner does not currently produce `Dropped` outcomes (the
/// store's `insert` returns either `Inserted` or `MergedWithExisting`),
/// but we keep the mapping exhaustive in case future store rules add
/// it.
fn event_from_insert_outcome(
    run_id: uuid::Uuid,
    outcome: InsertOutcome,
) -> crate::agent::types::AgentEvent {
    match outcome {
        InsertOutcome::Inserted { episode_id } => crate::agent::types::AgentEvent::EpisodeWritten {
            run_id,
            episode_id,
            outcome: "inserted".into(),
            occurrence_count: 1,
        },
        InsertOutcome::MergedWithExisting {
            episode_id,
            new_occurrence_count,
        } => crate::agent::types::AgentEvent::EpisodeWritten {
            run_id,
            episode_id,
            outcome: "merged".into(),
            occurrence_count: new_occurrence_count,
        },
        InsertOutcome::Dropped { reason } => crate::agent::types::AgentEvent::EpisodeWritten {
            run_id,
            episode_id: String::new(),
            outcome: format!("dropped: {reason}"),
            occurrence_count: 0,
        },
    }
}

/// Walk workflow-local rows touched during this run and copy
/// promotion-eligible ones into the global store. Returns the global
/// episode IDs actually written (or merged into an existing global row,
/// which still counts as promoted) plus the count of candidates the
/// gate rejected.
///
/// The promotion gate uses pure [`should_promote`] from
/// `episodic::promotion`: a row is promoted when its workflow-local
/// `occurrence_count >= 2` OR a row with the same
/// `pre_state_signature` already exists in global (cross-workflow
/// confirmation).
async fn promote_matching_episodes(
    wl: &Arc<SqliteEpisodicStore>,
    global: &Arc<SqliteEpisodicStore>,
    workflow_hash: &str,
    run_started_at: chrono::DateTime<chrono::Utc>,
) -> Result<(Vec<String>, usize), EpisodicError> {
    use rusqlite::params;
    let wl_conn = wl.conn.clone();
    let g_conn = global.conn.clone();
    let workflow_hash = workflow_hash.to_string();

    tokio::task::spawn_blocking(move || -> Result<(Vec<String>, usize), EpisodicError> {
        let mut promoted_ids: Vec<String> = Vec::new();
        let mut skipped: usize = 0;
        let wl = lock_conn(&wl_conn)?;
        let g = lock_conn(&g_conn)?;

        // P1.M3: only episodes touched (inserted or merged) during this
        // run participate. `last_seen_at` is bumped on both fresh insert
        // (= `created_at`) and on merge, so it's the run-scoping
        // timestamp we need.
        let mut stmt = wl.prepare(
            "SELECT episode_id, pre_state_signature, occurrence_count
               FROM episodes
              WHERE workflow_hash = ?1
                AND datetime(last_seen_at) >= datetime(?2)",
        )?;
        let rows: Vec<(String, String, i64)> = stmt
            .query_map(params![workflow_hash, run_started_at.to_rfc3339()], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?,
                ))
            })?
            .filter_map(|r| r.ok())
            .collect();

        for (ep_id, sig, count) in rows {
            let global_has: i64 = g
                .query_row(
                    "SELECT COUNT(*) FROM episodes WHERE pre_state_signature = ?1",
                    params![sig],
                    |r| r.get(0),
                )
                .unwrap_or(0);

            if !should_promote(count as u32, global_has > 0) {
                skipped += 1;
                continue;
            }

            // Copy the row into global, flipping scope.
            let row = wl.query_row(
                "SELECT workflow_hash, pre_state_signature, goal, subgoal_text,
                        failure_signature_json, recovery_actions_json, recovery_actions_hash,
                        outcome_summary, pre_state_snapshot_json, embedding_blob,
                        embedding_impl_id, occurrence_count, created_at, last_seen_at,
                        last_retrieved_at, step_record_refs_json
                   FROM episodes WHERE episode_id = ?1",
                params![ep_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, Option<String>>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, String>(5)?,
                        row.get::<_, String>(6)?,
                        row.get::<_, String>(7)?,
                        row.get::<_, String>(8)?,
                        row.get::<_, Vec<u8>>(9)?,
                        row.get::<_, String>(10)?,
                        row.get::<_, i64>(11)?,
                        row.get::<_, String>(12)?,
                        row.get::<_, String>(13)?,
                        row.get::<_, Option<String>>(14)?,
                        row.get::<_, String>(15)?,
                    ))
                },
            )?;

            let global_episode_id = format!("ep_{}", Ulid::new());

            // INSERT OR IGNORE respects the UNIQUE
            // (scope, pre_state_signature, recovery_actions_hash)
            // index; on conflict we bump `occurrence_count`.
            let inserted = g.execute(
                "INSERT OR IGNORE INTO episodes (
                    episode_id, scope, workflow_hash, pre_state_signature, goal,
                    subgoal_text, failure_signature_json, recovery_actions_json,
                    recovery_actions_hash, outcome_summary, pre_state_snapshot_json,
                    embedding_blob, embedding_impl_id, occurrence_count,
                    created_at, last_seen_at, last_retrieved_at, step_record_refs_json
                ) VALUES (?1, 'global', ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)",
                params![
                    global_episode_id,
                    row.0,
                    row.1,
                    row.2,
                    row.3,
                    row.4,
                    row.5,
                    row.6,
                    row.7,
                    row.8,
                    row.9,
                    row.10,
                    row.11,
                    row.12,
                    row.13,
                    row.14,
                    row.15,
                ],
            )?;

            if inserted == 0 {
                // Dedup hit on global: bump occurrence_count + last_seen_at.
                let _ = g.execute(
                    "UPDATE episodes
                        SET occurrence_count = occurrence_count + 1,
                            last_seen_at = datetime('now')
                      WHERE scope = 'global'
                        AND pre_state_signature = ?1
                        AND recovery_actions_hash = ?2",
                    params![row.1, row.6],
                );
                // Merged into an existing global row — still counts as
                // promoted for telemetry.
                promoted_ids.push(global_episode_id);
            } else {
                promoted_ids.push(global_episode_id);
            }
        }

        Ok((promoted_ids, skipped))
    })
    .await
    .map_err(join_err)?
}
