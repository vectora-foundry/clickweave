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
use crate::agent::episodic::store::{EpisodicStore, EpisodicStoreConfig, SqliteEpisodicStore};
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
    /// Detached on drop. `JoinHandle` does not abort the task on drop
    /// in tokio; instead, dropping the writer drops `tx`, which closes
    /// the channel, and the worker exits cleanly after draining the
    /// remaining messages from `rx`.
    #[allow(dead_code)]
    join: tokio::task::JoinHandle<()>,
}

impl EpisodicWriter {
    /// Back-compat constructor for tests / callers that don't carry an
    /// explicit store config. Delegates to [`Self::spawn_with_config`]
    /// with `EpisodicStoreConfig::default()`. Production callers should
    /// use `spawn_with_config` directly so the `AgentConfig`-derived
    /// score weights, half-life, and per-scope caps reach the stores.
    /// `event_tx`, when `Some`, receives `EpisodeWritten` /
    /// `EpisodePromoted` emissions per the D33 contract.
    /// `run_id` is the runner's active-run UUID; it is captured at
    /// spawn time so emitted events pass the frontend's stale-run
    /// filter even after the runner moves on.
    pub fn spawn(
        ctx: EpisodicContext,
        event_tx: Option<mpsc::Sender<crate::agent::types::AgentEvent>>,
        run_id: uuid::Uuid,
    ) -> Result<Self, EpisodicError> {
        Self::spawn_with_config(ctx, EpisodicStoreConfig::default(), event_tx, run_id)
    }

    /// Production constructor. Opens the workflow-local and
    /// (optional) global stores with the *configured* score weights,
    /// half-life, and per-scope caps so values from `AgentConfig` are
    /// honored end to end.
    pub fn spawn_with_config(
        ctx: EpisodicContext,
        store_config: EpisodicStoreConfig,
        event_tx: Option<mpsc::Sender<crate::agent::types::AgentEvent>>,
        run_id: uuid::Uuid,
    ) -> Result<Self, EpisodicError> {
        let (tx, mut rx) = mpsc::channel::<WriteRequest>(CHANNEL_CAP);

        let wl = Arc::new(SqliteEpisodicStore::new_with_config(
            &ctx.workflow_local_path,
            EpisodeScope::WorkflowLocal,
            store_config.score_weights,
            store_config.decay_halflife_days,
            store_config.max_per_scope_workflow,
        )?);
        let global: Option<Arc<SqliteEpisodicStore>> = match &ctx.global_path {
            Some(p) => Some(Arc::new(SqliteEpisodicStore::new_with_config(
                p,
                EpisodeScope::Global,
                store_config.score_weights,
                store_config.decay_halflife_days,
                store_config.max_per_scope_global,
            )?)),
            None => None,
        };

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
                                    // `DeriveAndInsert` always targets the
                                    // workflow-local store (Spec 2 D30).
                                    // Promotion writes go through a
                                    // separate path with their own event.
                                    let event = event_from_insert_outcome(
                                        run_id,
                                        outcome,
                                        crate::agent::episodic::EpisodeScope::WorkflowLocal,
                                    );
                                    let _ = tx.send(event).await;
                                }
                            }
                            Err(e) => {
                                tracing::warn!(error = %e, "episodic: derive_and_insert failed");
                                // Surface write failures through the
                                // event channel as well so the UI and
                                // event consumers learn that an
                                // episodic write was lost. Bounded
                                // message with a stable `episodic:`
                                // prefix; non-sensitive.
                                if let Some(tx) = &event_tx_task {
                                    let _ = tx
                                        .send(crate::agent::types::AgentEvent::Warning {
                                            message: format!(
                                                "episodic: write dropped: derive_and_insert failed: {e}"
                                            ),
                                        })
                                        .await;
                                }
                            }
                        }
                    }
                    WriteRequest::PromotePass {
                        workflow_hash,
                        terminal_kind,
                        run_started_at,
                    } => {
                        if matches!(terminal_kind, PromotionTerminalKind::SkipPromotion) {
                            continue;
                        }
                        if let Some(g) = &global {
                            match promote_matching_episodes(
                                &wl,
                                g,
                                &workflow_hash,
                                run_started_at,
                                event_tx_task.as_ref(),
                                run_id,
                            )
                            .await
                            {
                                Ok((promoted, skipped)) => {
                                    if let Some(tx) = &event_tx_task {
                                        let event =
                                            crate::agent::types::AgentEvent::EpisodePromoted {
                                                run_id,
                                                promoted_episode_ids: promoted,
                                                skipped_count: skipped,
                                            };
                                        let _ = tx.send(event).await;
                                    }
                                }
                                Err(e) => {
                                    tracing::warn!(error = %e, "episodic: promotion pass failed");
                                    // See the DeriveAndInsert error
                                    // arm — same rationale, same prefix
                                    // scheme so consumers can match on
                                    // `episodic: ...` for memory-loss
                                    // signals.
                                    if let Some(tx) = &event_tx_task {
                                        let _ = tx
                                            .send(crate::agent::types::AgentEvent::Warning {
                                                message: format!(
                                                    "episodic: promotion dropped: {e}"
                                                ),
                                            })
                                            .await;
                                    }
                                }
                            }
                        }
                    }
                    // Barrier sentinel — by the time the worker pops
                    // this off the channel and reaches this arm, every
                    // prior message has been fully processed (including
                    // its SQL commit), so acking here gives the caller
                    // a real "all writes are visible" signal. The
                    // receive side may have been dropped already (e.g.
                    // a flush timed out and went out of scope); the
                    // ack send is best-effort.
                    WriteRequest::Flush { ack } => {
                        let _ = ack.send(());
                    }
                }
            }
        });

        Ok(Self { tx, join })
    }

    /// Best-effort enqueue. Returns `EpisodicError::Backpressure` when
    /// the channel is full so the runner can drop the request without
    /// blocking the agent loop.
    pub async fn queue(&self, req: WriteRequest) -> Result<(), EpisodicError> {
        self.tx
            .try_send(req)
            .map_err(|_| EpisodicError::Backpressure)
    }

    /// Block until every previously-queued request has been fully
    /// processed, including its SQL commit. Implemented with an
    /// in-channel `Flush` sentinel that the worker acks via a oneshot,
    /// so this is a real "writes are visible to other connections on
    /// the same DB file" barrier — not a channel-empty heuristic.
    ///
    /// Total wall-clock bound is ~1 s so a stuck consumer never blocks
    /// the run-terminal path indefinitely. The single timeout wraps
    /// both the sentinel enqueue and the ack receive, because
    /// `Sender::send` itself awaits a free permit when the channel is
    /// at capacity — without the wrapping timeout, a saturated channel
    /// (rare, but possible if the runner queued a burst right before
    /// terminal) could block the flush past its bound. A timeout, a
    /// closed channel (worker already exited), or a dropped ack all
    /// silently return; D32 keeps episodic best-effort.
    pub async fn flush(&self) {
        let (ack_tx, ack_rx) = tokio::sync::oneshot::channel();
        let tx = self.tx.clone();
        let _ = tokio::time::timeout(std::time::Duration::from_secs(1), async move {
            if tx.send(WriteRequest::Flush { ack: ack_tx }).await.is_err() {
                return;
            }
            let _ = ack_rx.await;
        })
        .await;
    }

    /// Test alias for [`Self::flush`]. Kept as a separate entry point
    /// (rather than a `#[cfg(test)]`-gated helper) so integration tests
    /// in `tests/` — which compile as a separate crate where
    /// `cfg(test)` items are not visible — can call it. Production
    /// code paths use [`Self::flush`] directly.
    pub async fn flush_for_tests(&self) {
        self.flush().await
    }

    /// Return a clone of the internal channel sender.
    ///
    /// The returned sender shares the same worker task: messages sent on it
    /// are processed by the same consumer loop and the same SQLite
    /// connections, so there is no second database connection. The channel
    /// stays alive until **all** senders — the one owned by the writer and
    /// any clones — are dropped.
    ///
    /// Intended for callers that need to enqueue requests on the writer
    /// after the writer itself has been moved into an inner scope (e.g.
    /// `StateRunner::run` consumes the runner; the Tauri command can hold
    /// a cloned sender, queue a `PromotePass` after `run` returns, then
    /// drop the sender to let the worker exit cleanly).
    pub fn sender(&self) -> mpsc::Sender<WriteRequest> {
        self.tx.clone()
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
    // The runner computes the signature at snapshot-capture time
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
        // Populate with the events.jsonl ref captured at
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
    scope: crate::agent::episodic::EpisodeScope,
) -> crate::agent::types::AgentEvent {
    match outcome {
        InsertOutcome::Inserted { episode_id } => crate::agent::types::AgentEvent::EpisodeWritten {
            run_id,
            outcome: "inserted".into(),
            episode_id,
            scope,
            occurrence_count: 1,
        },
        InsertOutcome::MergedWithExisting {
            episode_id,
            new_occurrence_count,
        } => crate::agent::types::AgentEvent::EpisodeWritten {
            run_id,
            outcome: "merged".into(),
            episode_id,
            scope,
            occurrence_count: new_occurrence_count,
        },
        InsertOutcome::Dropped { reason } => crate::agent::types::AgentEvent::EpisodeWritten {
            run_id,
            outcome: format!("dropped: {reason}"),
            episode_id: String::new(),
            scope,
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
    event_tx: Option<&mpsc::Sender<crate::agent::types::AgentEvent>>,
    run_id: uuid::Uuid,
) -> Result<(Vec<String>, usize), EpisodicError> {
    // Route global writes through `SqliteEpisodicStore::insert` so
    // insert + dedup-merge + step_record_refs union + LRU prune all
    // share the same code path the workflow-local writes already
    // use. Duplicating the SQL inline here would (a) bypass
    // `prune_lru` so a configured global cap could grow unbounded,
    // (b) lose provenance refs on dedup-merge (the existing global
    // row's `step_record_refs_json` would not get unioned with the
    // workflow-local row's refs), and (c) push a freshly-minted
    // `episode_id` to telemetry on a dedup-merge even though that
    // row was never actually inserted, so the IDs in
    // `EpisodePromoted::promoted_episode_ids` would not resolve in
    // the global store.
    let touched = wl.list_run_touched(workflow_hash, run_started_at).await?;

    let mut promoted_ids: Vec<String> = Vec::new();
    let mut skipped: usize = 0;

    for record in touched {
        let global_has = global
            .count_with_signature(&record.pre_state_signature)
            .await
            .unwrap_or(0)
            > 0;

        if !should_promote(record.occurrence_count, global_has) {
            skipped += 1;
            continue;
        }

        // Build the candidate global row from the workflow-local
        // record, flipping scope and minting a fresh `episode_id`
        // candidate. On dedup-merge inside `insert`, the existing
        // global `episode_id` wins; on fresh insert this candidate
        // ID lands as-is. Either way, `InsertOutcome` reports the
        // ID actually present in the global store.
        let now = chrono::Utc::now();
        let candidate = EpisodeRecord {
            episode_id: format!("ep_{}", Ulid::new()),
            scope: EpisodeScope::Global,
            // Reset the run-scoped timestamps so the global row's
            // `last_seen_at` reflects "first seen in global on this
            // promotion." Carrying `created_at` from the workflow-local
            // row preserves the rough age signal for decay scoring;
            // resetting `last_seen_at` to `now` keeps recency consistent
            // with the freshly-promoted state.
            occurrence_count: 1,
            last_seen_at: now,
            last_retrieved_at: None,
            ..record
        };

        match global.insert(candidate).await {
            Ok(outcome) => {
                // Capture the promoted ID (if any) before the
                // outcome is consumed by the event-emission helper.
                // D33 contract: `agent://episode_written` fires on
                // every successful insert/merge with `scope: "global"`,
                // matching the workflow-local emission path so
                // consumers see both stores' writes through one event.
                let promoted_id = match &outcome {
                    InsertOutcome::Inserted { episode_id }
                    | InsertOutcome::MergedWithExisting { episode_id, .. } => {
                        Some(episode_id.clone())
                    }
                    InsertOutcome::Dropped { .. } => None,
                };
                if let Some(tx) = event_tx {
                    let event = event_from_insert_outcome(
                        run_id,
                        outcome,
                        crate::agent::episodic::EpisodeScope::Global,
                    );
                    let _ = tx.send(event).await;
                }
                match promoted_id {
                    Some(id) => promoted_ids.push(id),
                    None => skipped += 1,
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "episodic: global promotion insert failed");
                skipped += 1;
            }
        }
    }

    Ok((promoted_ids, skipped))
}
