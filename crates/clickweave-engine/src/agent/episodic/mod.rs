//! Episodic memory layer for the Spec 2 agent.
//!
//! See `/Users/x0/Work/clickweave-vault/docs/design/2026-04-24_agent-episodic-memory.md`
//! for the full design rationale. High-level shape:
//!
//! - Primary use case: recovery reuse (D20).
//! - Storage: SQLite per scope (D26), two-tier workflow-local + global (D21).
//! - Retrieval: hybrid — structured `PreStateSignature` primary + text
//!   similarity secondary (D22), fires at run-start and `Recovering` entry (D24).
//! - Writes: async, piggyback on Spec 1's `RecoverySucceeded` StepRecord (D30).
//! - Failure isolation: never fail the agent run (D32).

pub mod embedder;
pub mod promotion;
pub mod render;
pub mod retrieval;
pub mod signature;
pub mod store;
pub mod types;
pub mod write;

pub use types::{
    CompactAction, EpisodeRecord, EpisodeScope, EpisodicContext, EpisodicError, FailureSignature,
    InsertOutcome, PreStateSignature, PromotionTerminalKind, RecoveringEntrySnapshot,
    RecoveryActionsHash, RetrievalTrigger, RetrievedEpisode, ScoreBreakdown, TriggeringError,
    WriteRequest,
};
// `pub use embedder::{Embedder, HashedShingleEmbedder};` lands in Task 1.4.
// `pub use store::{...}` and `pub use write::EpisodicWriter;` land in Phase 2.
