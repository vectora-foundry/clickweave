//! Shared types for the episodic memory layer.
//!
//! All Spec 2 public types live here so the rest of the module can import
//! from one place. Private helpers stay in their owning module.
//!
//! Phase 1 introduces the type surface only — the SQLite store and the
//! async writer are wired up in Phase 2. Module-level `#[allow(dead_code)]`
//! mirrors the existing Spec 1 modules' pattern (see `world_model.rs`,
//! `step_record.rs`).

#![allow(dead_code)]

use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::agent::step_record::{StepRecord, WorldModelSnapshot};
use crate::agent::task_state::TaskState;

/// Which store a row lives in (D21).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "snake_case")]
pub enum EpisodeScope {
    WorkflowLocal,
    Global,
}

impl EpisodeScope {
    pub fn as_str(&self) -> &'static str {
        match self {
            EpisodeScope::WorkflowLocal => "workflow_local",
            EpisodeScope::Global => "global",
        }
    }
}

/// Stable 16-char hex fingerprint of a WorldModel's structural shape (D22, D37).
/// Built by `signature::compute_pre_state_signature`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct PreStateSignature(pub String);

/// Stable hash over the recovery action sequence (D28).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct RecoveryActionsHash(pub String);

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct CompactAction {
    pub tool_name: String,
    pub brief_args: String,   // harness-authored, cap ~120 chars
    pub outcome_kind: String, // "ok" | "error" | etc.
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct FailureSignature {
    pub failed_tool: String,
    pub error_kind: String,
    pub consecutive_errors_at_entry: u32,
}

/// Full row in the SQLite store (D25, D37).
///
/// Phase 1 deviation: this struct only derives `Serialize`. The plan
/// requires `Deserialize` too, but `WorldModelSnapshot` and `TaskState`
/// (Spec 1 types) are serialize-only today. Adding `Deserialize` to those
/// upstream types would touch files outside Phase 1's scope. Phase 2 must
/// either round-trip through a serialize-only projection or extend the
/// upstream derives before the SQLite store reads rows back.
#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct EpisodeRecord {
    pub episode_id: String,
    pub scope: EpisodeScope,
    pub workflow_hash: String, // workflow.id (UUID) — always populated (D37)
    pub pre_state_signature: PreStateSignature,
    pub goal: String,
    pub subgoal_text: Option<String>,
    pub failure_signature: FailureSignature,
    pub recovery_actions: Vec<CompactAction>,
    pub recovery_actions_hash: RecoveryActionsHash,
    pub outcome_summary: String,
    pub pre_state_snapshot: WorldModelSnapshot,
    pub goal_subgoal_embedding: Vec<f32>,
    pub embedding_impl_id: String,
    pub occurrence_count: u32,
    pub created_at: DateTime<Utc>,
    pub last_seen_at: DateTime<Utc>,
    pub last_retrieved_at: Option<DateTime<Utc>>,
    pub step_record_refs: Vec<String>,
}

/// Per-run episodic wiring constructed by the Tauri layer (D34).
#[derive(Debug, Clone)]
pub struct EpisodicContext {
    pub enabled: bool,
    pub workflow_local_path: PathBuf,
    pub global_path: Option<PathBuf>, // Some iff global participation is on (D35)
    pub workflow_hash: String,        // workflow.id UUID
}

impl EpisodicContext {
    /// Returns a context with all paths disabled — used when the Tauri layer
    /// decides episodic should not run at all on this run.
    pub fn disabled() -> Self {
        Self {
            enabled: false,
            workflow_local_path: PathBuf::new(),
            global_path: None,
            workflow_hash: String::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "snake_case")]
pub enum RetrievalTrigger {
    RunStart,
    RecoveringEntry,
}

#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct ScoreBreakdown {
    pub structured_match: bool,
    pub text_similarity: f32,
    pub occurrence_boost: f32,
    pub decay_factor: f32,
    pub final_score: f32,
}

#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct RetrievedEpisode {
    pub episode: EpisodeRecord,
    pub scope: EpisodeScope,
    pub score_breakdown: ScoreBreakdown,
}

#[derive(Debug, Clone)]
pub enum InsertOutcome {
    Inserted {
        episode_id: String,
    },
    MergedWithExisting {
        episode_id: String,
        new_occurrence_count: u32,
    },
    Dropped {
        reason: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "snake_case")]
pub enum PromotionTerminalKind {
    Clean,         // AgentDone without disagreement, clean completion
    SkipPromotion, // CompletionDisagreement, LoopDetected, errored terminals
}

/// Captured at `Recovering`-entry on `StateRunner`; consumed at the matching
/// exit inside Spec 1's RecoverySucceeded guard.
///
/// **P1.C2 fix:** carries the `pre_state_signature` computed at query-time
/// (with the same `compute_pre_state_signature` function the retrieval used),
/// so writes and future exact-match queries share a single source of truth.
/// **P1.H3 fix:** carries `events_jsonl_ref`, an absolute path to the
/// currently-active execution's `events.jsonl`, so the write-side populates
/// `step_record_refs` and D36's orphan-ref sweep has something to resolve.
#[derive(Debug, Clone)]
pub struct RecoveringEntrySnapshot {
    pub entered_at_step: usize,
    pub world_model_at_entry: WorldModelSnapshot,
    pub task_state_at_entry: TaskState,
    pub triggering_error: TriggeringError,
    pub workflow_hash: String,
    pub pre_state_signature: PreStateSignature,
    pub active_watch_slots: Vec<crate::agent::task_state::WatchSlotName>,
    pub events_jsonl_ref: Option<String>,
}

#[derive(Debug, Clone)]
pub struct TriggeringError {
    pub failed_tool: String,
    pub error_kind: String,
    pub consecutive_errors_at_entry: u32,
    pub step_index: usize,
}

/// Messages sent from the runner into the writer task over a bounded channel.
///
/// `DeriveAndInsert` carries `Box<RecoveringEntrySnapshot>` and
/// `Box<StepRecord>` so the enum stays small (clippy::large_enum_variant).
/// The runner constructs at most one of these per `Recovering -> Executing`
/// transition, so the heap allocation is negligible relative to the SQLite
/// write the writer task subsequently performs.
#[derive(Debug)]
pub enum WriteRequest {
    DeriveAndInsert {
        entry: Box<RecoveringEntrySnapshot>,
        recovery_success: Box<StepRecord>,
        recovery_actions: Vec<CompactAction>,
    },
    PromotePass {
        workflow_hash: String,
        terminal_kind: PromotionTerminalKind,
        /// P1.M3 fix: only episodes whose `last_seen_at >= run_started_at`
        /// participate. A clean run with no recovery must not re-promote
        /// old rows or inflate global `occurrence_count` on unrelated
        /// episodes. Required by every PromotePass.
        run_started_at: DateTime<Utc>,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum EpisodicError {
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("encode: {0}")]
    Encode(String),
    #[error("decode: {0}")]
    Decode(String),
    #[error("backpressure: writer channel full")]
    Backpressure,
    #[error("disabled: episodic is not active on this run")]
    Disabled,
}
