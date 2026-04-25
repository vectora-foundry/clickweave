//! SQLite-backed episodic store (D26).
//!
//! Phase 2 implements the `EpisodicStore` trait against a real `rusqlite`
//! connection in WAL mode. The connection is wrapped in `Arc<Mutex<_>>`
//! (D38) so the store can be cheaply cloned across the writer task and
//! the runner's retrieval call sites without losing the SQLite
//! single-thread-per-connection invariant. Per-method bodies are
//! offloaded to `tokio::task::spawn_blocking` so the async runtime is
//! never blocked on a SQLite read or write.
//!
//! Failure isolation (D32): every read path is fail-soft via
//! `unwrap_or_default()` so a single corrupted row never poisons a
//! retrieval.

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use async_trait::async_trait;
use rusqlite::{Connection, params};

use crate::agent::episodic::retrieval::ScoreWeights;
use crate::agent::episodic::types::{
    EpisodeRecord, EpisodeScope, EpisodicError, FailureSignature, InsertOutcome, PreStateSignature,
    RecoveryActionsHash, RetrievalQuery, RetrievedEpisode,
};

/// Column list shared by every `SELECT * FROM episodes` query path. Kept
/// as a const so the structured-stage and fallback-stage queries in
/// `retrieve()` cannot drift apart.
const EPISODE_SELECT_COLUMNS: &str = "episode_id, scope, workflow_hash, pre_state_signature, goal, subgoal_text, \
     failure_signature_json, recovery_actions_json, recovery_actions_hash, \
     outcome_summary, pre_state_snapshot_json, embedding_blob, embedding_impl_id, \
     occurrence_count, created_at, last_seen_at, last_retrieved_at, \
     step_record_refs_json";

/// Lock the connection mutex, mapping poison errors into the
/// `EpisodicError::Encode` variant so spawn_blocking closures can
/// `?`-propagate.
pub(crate) fn lock_conn(
    conn: &Mutex<Connection>,
) -> Result<MutexGuard<'_, Connection>, EpisodicError> {
    conn.lock()
        .map_err(|e| EpisodicError::Encode(format!("mutex poisoned: {e}")))
}

/// Map a `JoinError` from `spawn_blocking` into the `Encode` variant.
pub(crate) fn join_err(e: tokio::task::JoinError) -> EpisodicError {
    EpisodicError::Encode(format!("join: {e}"))
}

#[async_trait]
pub trait EpisodicStore: Send + Sync {
    async fn insert(&self, episode: EpisodeRecord) -> Result<InsertOutcome, EpisodicError>;
    async fn retrieve(
        &self,
        query: &RetrievalQuery<'_>,
        k: usize,
    ) -> Result<Vec<RetrievedEpisode>, EpisodicError>;
    async fn prune_lru(&self, cap: usize) -> Result<usize, EpisodicError>;
}

pub struct SqliteEpisodicStore {
    pub(crate) conn: Arc<Mutex<Connection>>,
    pub(crate) scope: EpisodeScope,
    pub(crate) path: PathBuf,
    /// Config-derived score weights (P1.M2). Set at construction so
    /// `retrieve()` doesn't have to re-read `AgentConfig`.
    pub score_weights: ScoreWeights,
    /// Half-life (in days) for the time-decay factor in scoring (P1.M2).
    pub decay_halflife_days: f32,
    /// Maximum rows to retain per scope before LRU eviction kicks in
    /// (P1.M2). `insert()` calls `prune_lru(self.max_per_scope)` after
    /// every fresh row write.
    pub max_per_scope: usize,
}

const SCHEMA_V1: &str = r#"
CREATE TABLE IF NOT EXISTS episodes (
    episode_id              TEXT PRIMARY KEY,
    scope                   TEXT NOT NULL,
    workflow_hash           TEXT NOT NULL,
    pre_state_signature     TEXT NOT NULL,
    goal                    TEXT NOT NULL,
    subgoal_text            TEXT,
    failure_signature_json  TEXT NOT NULL,
    recovery_actions_json   TEXT NOT NULL,
    recovery_actions_hash   TEXT NOT NULL,
    outcome_summary         TEXT NOT NULL,
    pre_state_snapshot_json TEXT NOT NULL,
    embedding_blob          BLOB NOT NULL,
    embedding_impl_id       TEXT NOT NULL,
    occurrence_count        INTEGER NOT NULL DEFAULT 1,
    created_at              TEXT NOT NULL,
    last_seen_at            TEXT NOT NULL,
    last_retrieved_at       TEXT,
    step_record_refs_json   TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_episodes_pre_state_signature
    ON episodes (pre_state_signature);
CREATE INDEX IF NOT EXISTS idx_episodes_scope_signature
    ON episodes (scope, pre_state_signature);
CREATE UNIQUE INDEX IF NOT EXISTS idx_episodes_dedup
    ON episodes (scope, pre_state_signature, recovery_actions_hash);
CREATE INDEX IF NOT EXISTS idx_episodes_last_retrieved
    ON episodes (scope, last_retrieved_at);
"#;

impl SqliteEpisodicStore {
    /// Back-compat constructor used in tests and for the writer's
    /// internally-managed stores. Production runner code calls
    /// [`Self::new_with_config`] with values derived from `AgentConfig`.
    pub fn new(path: &Path, scope: EpisodeScope) -> Result<Self, EpisodicError> {
        Self::new_with_config(path, scope, ScoreWeights::default(), 90.0, 500)
    }

    /// Production constructor (P1.M2): config-tuned weights, half-life,
    /// and per-scope cap. `StateRunner::new` (Phase 3) calls this with
    /// values derived from `AgentConfig`.
    pub fn new_with_config(
        path: &Path,
        scope: EpisodeScope,
        score_weights: ScoreWeights,
        decay_halflife_days: f32,
        max_per_scope: usize,
    ) -> Result<Self, EpisodicError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| EpisodicError::Encode(format!("create parent dir: {e}")))?;
        }
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.execute_batch(SCHEMA_V1)?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            scope,
            path: path.to_path_buf(),
            score_weights,
            decay_halflife_days,
            max_per_scope,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn scope(&self) -> EpisodeScope {
        self.scope
    }

    /// Integration-test helper: returns the total row count. Not for
    /// production callers — use the retrieve/insert trait methods there.
    pub fn row_count_for_tests(&self) -> Result<u64, EpisodicError> {
        let conn = lock_conn(&self.conn)?;
        let n: i64 = conn.query_row("SELECT COUNT(*) FROM episodes", [], |r| r.get(0))?;
        Ok(n as u64)
    }

    /// List every row in this store that was inserted or merged
    /// during the current run, identified by `workflow_hash` and
    /// `since` (typically the run-started timestamp). The
    /// `last_seen_at` column is the run-scoping timestamp because
    /// both fresh insert (= `created_at`) and merge bump it.
    ///
    /// Used by the run-terminal promotion pass (D31) instead of
    /// the duplicated SQL it used to carry. Routing the eligibility
    /// scan through one place keeps the workflow-local read shape
    /// in lock-step with [`row_to_episode`] so promotion reads
    /// the same fields retrieval does.
    pub async fn list_run_touched(
        &self,
        workflow_hash: &str,
        since: chrono::DateTime<chrono::Utc>,
    ) -> Result<Vec<EpisodeRecord>, EpisodicError> {
        let conn = self.conn.clone();
        let workflow_hash = workflow_hash.to_string();
        tokio::task::spawn_blocking(move || -> Result<Vec<EpisodeRecord>, EpisodicError> {
            let conn = lock_conn(&conn)?;
            let sql = format!(
                "SELECT {EPISODE_SELECT_COLUMNS} FROM episodes \
                 WHERE workflow_hash = ?1 \
                   AND datetime(last_seen_at) >= datetime(?2)"
            );
            let mut stmt = conn.prepare(&sql)?;
            let rows: Vec<EpisodeRecord> = stmt
                .query_map(params![workflow_hash, since.to_rfc3339()], row_to_episode)?
                .filter_map(|r| r.ok())
                .collect();
            Ok(rows)
        })
        .await
        .map_err(join_err)?
    }

    /// Count rows in this store with the given `pre_state_signature`
    /// (any scope). Used by the promotion gate's "global has a row
    /// with this signature already" branch (D31).
    pub async fn count_with_signature(
        &self,
        sig: &PreStateSignature,
    ) -> Result<u64, EpisodicError> {
        let conn = self.conn.clone();
        let sig = sig.0.clone();
        tokio::task::spawn_blocking(move || -> Result<u64, EpisodicError> {
            let conn = lock_conn(&conn)?;
            let n: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM episodes WHERE pre_state_signature = ?1",
                    params![sig],
                    |r| r.get(0),
                )
                .unwrap_or(0);
            Ok(n as u64)
        })
        .await
        .map_err(join_err)?
    }
}

/// Construction-time configuration for `SqliteEpisodicStore` and the
/// `EpisodicWriter` that owns one. Aggregates the `AgentConfig` knobs
/// `StateRunner::new_with_episodic` already feeds into the retrieval
/// stores so the writer-owned stores can be opened with the *same*
/// values instead of the back-compat defaults `Self::new` hands out.
///
/// F3 fix: prior to this struct, `EpisodicWriter::spawn` opened both
/// stores via `SqliteEpisodicStore::new`, which hard-coded the
/// per-scope cap to 500 — a configured 2000-row global cap was
/// silently ignored on the write side.
#[derive(Debug, Clone)]
pub struct EpisodicStoreConfig {
    pub score_weights: ScoreWeights,
    pub decay_halflife_days: f32,
    pub max_per_scope_workflow: usize,
    pub max_per_scope_global: usize,
}

impl Default for EpisodicStoreConfig {
    fn default() -> Self {
        Self {
            score_weights: ScoreWeights::default(),
            decay_halflife_days: 90.0,
            max_per_scope_workflow: 500,
            max_per_scope_global: 2000,
        }
    }
}

#[async_trait]
impl EpisodicStore for SqliteEpisodicStore {
    async fn insert(&self, episode: EpisodeRecord) -> Result<InsertOutcome, EpisodicError> {
        let conn = self.conn.clone();
        let outcome_result =
            tokio::task::spawn_blocking(move || -> Result<InsertOutcome, EpisodicError> {
                let conn = lock_conn(&conn)?;

                // Serialize JSON fields
                let failure_json = serde_json::to_string(&episode.failure_signature)
                    .map_err(|e| EpisodicError::Encode(format!("failure_signature: {e}")))?;
                let actions_json = serde_json::to_string(&episode.recovery_actions)
                    .map_err(|e| EpisodicError::Encode(format!("recovery_actions: {e}")))?;
                let snapshot_json = serde_json::to_string(&episode.pre_state_snapshot)
                    .map_err(|e| EpisodicError::Encode(format!("pre_state_snapshot: {e}")))?;
                let refs_json = serde_json::to_string(&episode.step_record_refs)
                    .map_err(|e| EpisodicError::Encode(format!("step_record_refs: {e}")))?;
                let embedding_blob = bincode::serialize(&episode.goal_subgoal_embedding)
                    .map_err(|e| EpisodicError::Encode(format!("embedding: {e}")))?;

                let inserted = conn.execute(
                    "INSERT OR IGNORE INTO episodes (
                        episode_id, scope, workflow_hash, pre_state_signature, goal, subgoal_text,
                        failure_signature_json, recovery_actions_json, recovery_actions_hash,
                        outcome_summary, pre_state_snapshot_json, embedding_blob, embedding_impl_id,
                        occurrence_count, created_at, last_seen_at, last_retrieved_at,
                        step_record_refs_json
                    ) VALUES (
                        ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18
                    )",
                    params![
                        episode.episode_id,
                        episode.scope.as_str(),
                        episode.workflow_hash,
                        episode.pre_state_signature.0,
                        episode.goal,
                        episode.subgoal_text,
                        failure_json,
                        actions_json,
                        episode.recovery_actions_hash.0,
                        episode.outcome_summary,
                        snapshot_json,
                        embedding_blob,
                        episode.embedding_impl_id,
                        episode.occurrence_count as i64,
                        episode.created_at.to_rfc3339(),
                        episode.last_seen_at.to_rfc3339(),
                        episode.last_retrieved_at.as_ref().map(|t| t.to_rfc3339()),
                        refs_json,
                    ],
                )?;

                if inserted == 1 {
                    return Ok(InsertOutcome::Inserted {
                        episode_id: episode.episode_id,
                    });
                }

                // Duplicate hit on the dedup index — merge semantics per D28.
                let (existing_id, existing_refs): (String, String) = conn.query_row(
                    "SELECT episode_id, step_record_refs_json FROM episodes
                      WHERE scope = ?1 AND pre_state_signature = ?2 AND recovery_actions_hash = ?3",
                    params![
                        episode.scope.as_str(),
                        episode.pre_state_signature.0,
                        episode.recovery_actions_hash.0
                    ],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )?;

                // Union step_record_refs (string-equality dedup).
                let mut merged_refs: Vec<String> = serde_json::from_str(&existing_refs)
                    .map_err(|e| EpisodicError::Decode(format!("existing refs: {e}")))?;
                for r in &episode.step_record_refs {
                    if !merged_refs.iter().any(|e| e == r) {
                        merged_refs.push(r.clone());
                    }
                }
                let merged_refs_json = serde_json::to_string(&merged_refs)
                    .map_err(|e| EpisodicError::Encode(format!("merged refs: {e}")))?;

                let new_count: i64 = conn.query_row(
                    "UPDATE episodes
                        SET occurrence_count = occurrence_count + 1,
                            last_seen_at = ?2,
                            step_record_refs_json = ?3
                      WHERE episode_id = ?1
                     RETURNING occurrence_count",
                    params![
                        existing_id,
                        episode.last_seen_at.to_rfc3339(),
                        merged_refs_json
                    ],
                    |row| row.get(0),
                )?;

                Ok(InsertOutcome::MergedWithExisting {
                    episode_id: existing_id,
                    new_occurrence_count: new_count as u32,
                })
            })
            .await
            .map_err(join_err)?;

        let outcome = outcome_result?;

        // P1.M2: prune LRU only after fresh-insert paths grew the row count.
        // Dedup-merges keep row count constant, so they skip pruning.
        if matches!(outcome, InsertOutcome::Inserted { .. }) {
            let _ = self.prune_lru(self.max_per_scope).await;
        }
        Ok(outcome)
    }

    async fn retrieve(
        &self,
        query: &RetrievalQuery<'_>,
        k: usize,
    ) -> Result<Vec<RetrievedEpisode>, EpisodicError> {
        use crate::agent::episodic::embedder::{Embedder, HashedShingleEmbedder, nan_safe_desc};
        use crate::agent::episodic::retrieval::score;

        let conn = self.conn.clone();
        let scope = self.scope;
        let sig = query.pre_state_signature.0.clone();
        let goal = query.goal.to_string();
        let subgoal = query.subgoal_text.map(|s| s.to_string());
        let now = query.now;
        // P1.M2: capture config-derived tuning before moving into spawn_blocking.
        let store_weights = self.score_weights;
        let store_halflife = self.decay_halflife_days;

        tokio::task::spawn_blocking(move || -> Result<Vec<RetrievedEpisode>, EpisodicError> {
            let conn = lock_conn(&conn)?;

            // Stage 1: structured exact-match on (scope, pre_state_signature).
            let structured_sql = format!(
                "SELECT {EPISODE_SELECT_COLUMNS} FROM episodes \
                 WHERE scope = ?1 AND pre_state_signature = ?2"
            );
            let mut stmt = conn.prepare(&structured_sql)?;
            let structured_rows: Vec<EpisodeRecord> = stmt
                .query_map(params![scope.as_str(), sig], row_to_episode)?
                .filter_map(|r| r.ok())
                .collect();

            let fallback = structured_rows.is_empty();
            let candidates: Vec<EpisodeRecord> = if fallback {
                // F5 fix: score every row in scope, ordered
                // deterministically. The previous implementation
                // sliced the first 200 rows in undefined SQLite row
                // order, which (a) made the best semantic match
                // invisible to scoring once a store grew past 200
                // rows even within the configured 500/2000 cap, and
                // (b) made fallback retrieval nondeterministic
                // because SQLite row order is not a stable contract.
                // The configured per-scope cap (500 workflow / 2000
                // global) is the only bound on candidate count, so
                // a full-scope scan stays within design budget.
                // Ordering by `last_seen_at DESC, occurrence_count
                // DESC, episode_id` gives a stable tie-break across
                // repeated queries.
                let fallback_sql = format!(
                    "SELECT {EPISODE_SELECT_COLUMNS} FROM episodes \
                     WHERE scope = ?1 \
                     ORDER BY last_seen_at DESC, occurrence_count DESC, episode_id"
                );
                let mut stmt = conn.prepare(&fallback_sql)?;
                stmt.query_map(params![scope.as_str()], row_to_episode)?
                    .filter_map(|r| r.ok())
                    .collect()
            } else {
                structured_rows
            };

            // Stage 2: score in Rust.
            let embedder = HashedShingleEmbedder::default();
            let query_text = match &subgoal {
                Some(s) => format!("{} {}", goal, s),
                None => goal.clone(),
            };
            let query_embedding = embedder.embed(&query_text);

            let weights = store_weights;
            let halflife = store_halflife;

            let mut scored: Vec<(EpisodeRecord, _)> = candidates
                .into_iter()
                .map(|c| {
                    let breakdown = score(&c, &query_embedding, now, weights, halflife, !fallback);
                    (c, breakdown)
                })
                .collect();

            scored.sort_by(|a, b| nan_safe_desc(a.1.final_score, b.1.final_score));
            scored.truncate(k);

            // Update last_retrieved_at for the rows we returned.
            for (ep, _) in &scored {
                let _ = conn.execute(
                    "UPDATE episodes SET last_retrieved_at = ?2 WHERE episode_id = ?1",
                    params![ep.episode_id, now.to_rfc3339()],
                );
            }

            Ok(scored
                .into_iter()
                .map(|(ep, breakdown)| RetrievedEpisode {
                    scope: ep.scope,
                    episode: ep,
                    score_breakdown: breakdown,
                })
                .collect())
        })
        .await
        .map_err(join_err)?
    }

    async fn prune_lru(&self, cap: usize) -> Result<usize, EpisodicError> {
        let conn = self.conn.clone();
        let scope = self.scope;
        tokio::task::spawn_blocking(move || -> Result<usize, EpisodicError> {
            let conn = lock_conn(&conn)?;

            let count: i64 = conn.query_row(
                "SELECT COUNT(*) FROM episodes WHERE scope = ?1",
                params![scope.as_str()],
                |row| row.get(0),
            )?;

            if (count as usize) <= cap {
                return Ok(0);
            }

            let to_delete = (count as usize) - cap;

            // Preserve: rows created in the last hour are never pruned (grace
            // window so a freshly written run-local row can't be evicted by
            // older runs' rows). Evict: oldest last_retrieved_at (NULLs
            // first), then oldest last_seen_at as the tiebreaker.
            let deleted = conn.execute(
                "DELETE FROM episodes
                  WHERE scope = ?1
                    AND episode_id IN (
                        SELECT episode_id FROM episodes
                         WHERE scope = ?1
                           AND datetime(created_at) < datetime('now', '-1 hour')
                         ORDER BY (last_retrieved_at IS NULL) DESC,
                                  last_retrieved_at ASC,
                                  last_seen_at ASC
                         LIMIT ?2
                    )",
                params![scope.as_str(), to_delete as i64],
            )?;

            Ok(deleted)
        })
        .await
        .map_err(join_err)?
    }
}

/// Decode one SQLite row into an `EpisodeRecord`. Fail-soft per D32:
/// JSON / bincode parse failures fall back to default values rather
/// than poisoning the entire retrieval result.
fn row_to_episode(row: &rusqlite::Row<'_>) -> rusqlite::Result<EpisodeRecord> {
    use chrono::{DateTime, Utc};

    let scope_str: String = row.get("scope")?;
    let scope = match scope_str.as_str() {
        "workflow_local" => EpisodeScope::WorkflowLocal,
        "global" => EpisodeScope::Global,
        other => {
            return Err(rusqlite::Error::InvalidColumnType(
                0,
                format!("unknown scope: {other}"),
                rusqlite::types::Type::Text,
            ));
        }
    };

    let created_at: String = row.get("created_at")?;
    let last_seen_at: String = row.get("last_seen_at")?;
    let last_retrieved_at: Option<String> = row.get("last_retrieved_at")?;

    let embedding_blob: Vec<u8> = row.get("embedding_blob")?;
    let goal_subgoal_embedding: Vec<f32> =
        bincode::deserialize(&embedding_blob).unwrap_or_default();

    fn parse_or_default<T: serde::de::DeserializeOwned + Default>(s: &str) -> T {
        serde_json::from_str(s).unwrap_or_default()
    }
    let failure_signature: FailureSignature =
        parse_or_default(&row.get::<_, String>("failure_signature_json")?);
    let recovery_actions = parse_or_default(&row.get::<_, String>("recovery_actions_json")?);
    let pre_state_snapshot = parse_or_default(&row.get::<_, String>("pre_state_snapshot_json")?);
    let step_record_refs: Vec<String> =
        parse_or_default(&row.get::<_, String>("step_record_refs_json")?);

    fn parse_rfc3339_or_now(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s)
            .map(|dt| dt.with_timezone(&Utc))
            .unwrap_or_else(|_| Utc::now())
    }

    Ok(EpisodeRecord {
        episode_id: row.get("episode_id")?,
        scope,
        workflow_hash: row.get("workflow_hash")?,
        pre_state_signature: PreStateSignature(row.get("pre_state_signature")?),
        goal: row.get("goal")?,
        subgoal_text: row.get("subgoal_text")?,
        failure_signature,
        recovery_actions,
        recovery_actions_hash: RecoveryActionsHash(row.get("recovery_actions_hash")?),
        outcome_summary: row.get("outcome_summary")?,
        pre_state_snapshot,
        goal_subgoal_embedding,
        embedding_impl_id: row.get("embedding_impl_id")?,
        occurrence_count: row.get::<_, i64>("occurrence_count")? as u32,
        created_at: parse_rfc3339_or_now(&created_at),
        last_seen_at: parse_rfc3339_or_now(&last_seen_at),
        last_retrieved_at: last_retrieved_at
            .and_then(|s| DateTime::parse_from_rfc3339(&s).ok())
            .map(|dt| dt.with_timezone(&Utc)),
        step_record_refs,
    })
}
