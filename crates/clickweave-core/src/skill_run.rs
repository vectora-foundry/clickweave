//! Skill-keyed run record types (D27, D28).
//!
//! `SkillRun` records a single invocation of a skill: when it ran, how
//! it ended, per-section outcomes, and how many repair iterations were
//! needed. Runs are persisted under
//! `<base>/.clickweave/skills/<skill_id>/runs/<run_id>.json` with a
//! sibling `events.jsonl` under `<run_id>/` for trace events. Retention
//! is capped at the last 20 runs per skill.
//!
//! Replaces the deleted node-keyed `NodeRun` record. Step-level state
//! (per-section status during execution) is tracked here instead of on
//! a per-node basis because skill execution iterates `[ActionSketchStep]`
//! grouped under `##` headings, not a graph of nodes.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use uuid::Uuid;

use crate::node_params::RunStatus;

/// Per-section run outcome carried inside a [`SkillRun`].
///
/// Sections are addressed by their `<!-- section: <id> -->` markers in
/// `SKILL.md`. Outcomes drive the per-section state pill on the run
/// timeline and the fidelity dot aggregation in Phase 2.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "snake_case")]
pub enum SectionOutcome {
    /// Section has not started yet.
    Pending,
    /// Section is currently executing.
    Running,
    /// Section completed without intervention.
    Succeeded,
    /// Section completed but at least one repair iteration was needed.
    Repaired,
    /// Section failed and the run halted (or moved on after the user
    /// chose to skip — the per-section status is still `Failed`).
    Failed,
    /// Section was deliberately skipped (e.g. operator chose Skip on a
    /// supervision pause).
    Skipped,
}

/// Persisted record of a single skill execution.
///
/// One JSON document per run is written at
/// `<skills>/<skill_id>/runs/<run_id>.json`; trace events stream to
/// `<skills>/<skill_id>/runs/<run_id>/events.jsonl` under the same
/// directory.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct SkillRun {
    pub run_id: Uuid,
    /// Skill identifier (matches the directory name on disk).
    pub skill_id: String,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    pub status: RunStatus,
    pub duration_ms: Option<u64>,
    /// Per-section outcomes keyed by section ID
    /// (`<!-- section: <id> -->` marker).
    #[serde(default)]
    pub per_section_outcome: HashMap<String, SectionOutcome>,
    #[serde(default)]
    pub repair_count: u32,
}

impl SkillRun {
    /// Create a fresh run with `status = Ok` (treated as "running" until
    /// `finalize_*` writes a terminal status). Per-section outcomes
    /// start empty; the runner fills them in as it iterates sections.
    pub fn new(skill_id: impl Into<String>) -> Self {
        Self {
            run_id: Uuid::new_v4(),
            skill_id: skill_id.into(),
            started_at: Utc::now(),
            finished_at: None,
            status: RunStatus::Ok,
            duration_ms: None,
            per_section_outcome: HashMap::new(),
            repair_count: 0,
        }
    }
}
