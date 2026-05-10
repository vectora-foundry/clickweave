//! Public types for the procedural-skills layer.
//!
//! Phase 1 introduces the type surface only — filesystem I/O, the file
//! watcher, the extractor, and the replay engine are wired up in
//! subsequent phases. Module-level `#[allow(dead_code)]` mirrors the
//! Spec 2 episodic types pattern.

#![allow(dead_code)]

use std::path::PathBuf;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::agent::step_record::WorldModelSnapshot;

/// Stable, human-friendly skill identifier (e.g. `skl_a8c4f1`). Carried
/// in `SKILL.md` frontmatter and in the on-disk directory name. Plain
/// type alias for now — a newtype can be introduced later without
/// touching the wire format.
pub type SkillId = String;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "snake_case")]
pub enum SkillScope {
    ProjectLocal,
    Global,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "snake_case")]
pub enum SkillState {
    Draft,
    Confirmed,
    Promoted,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct SubgoalSignature(pub String);

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct ApplicabilitySignature(pub String);

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct ParameterSlot {
    pub name: String,
    pub type_tag: String,
    pub description: Option<String>,
    pub default: Option<serde_json::Value>,
    pub enum_values: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub enum BindingRef {
    Captured { name: String },
    Params { name: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct OutputDeclaration {
    pub name: String,
    pub type_tag: String,
    pub from: BindingRef,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct AxDescriptorMatch {
    pub role: String,
    pub name: String,
    pub parent_name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub enum CaptureSource {
    AxDescriptor { descriptor: AxDescriptorMatch },
    ToolResult { jsonpath: String },
    Literal { value: serde_json::Value },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct CaptureClause {
    pub name: String,
    pub source: CaptureSource,
}

/// Skills-layer mirror of `agent::types::WorldModelDiff` (same
/// `changed_fields: Vec<String>` shape). Owned by this module so the
/// `Skill` value round-trips through YAML / JSON without forcing
/// `Deserialize` onto the runtime diff type. The extractor (Phase 3)
/// converts from `WorldModelDiff` at the boundary.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct ExpectedWorldModelDelta {
    pub changed_fields: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub enum ActionSketchStep {
    ToolCall {
        step_id: String,
        tool: String,
        args: serde_json::Value,
        captures_pre: Vec<CaptureClause>,
        captures: Vec<CaptureClause>,
        expected_world_model_delta: ExpectedWorldModelDelta,
        /// Explicit approval override for this step. `Some(true)` always
        /// gates; `Some(false)` always bypasses; `None` defers to the
        /// `should_gate_step` heuristic (destructive-hint + static list).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        requires_approval: Option<bool>,
    },
    Loop {
        step_id: String,
        until: LoopPredicate,
        body: Vec<ActionSketchStep>,
        max_iterations: u32,
        iteration_delay_ms: u64,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub enum LoopPredicate {
    WorldModelDelta { expr: String },
    StepCountReached { count: u32 },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub enum OutcomePredicate {
    SubgoalCompleted {
        post_state_world_model_signature: Option<String>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct ApplicabilityHints {
    pub apps: Vec<String>,
    pub hosts: Vec<String>,
    pub signature: ApplicabilitySignature,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct ProvenanceEntry {
    pub run_id: String,
    pub step_index: usize,
    pub completed_at: DateTime<Utc>,
    pub workflow_hash: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct SkillStats {
    pub occurrence_count: u32,
    pub success_rate: f32,
    pub last_seen_at: Option<DateTime<Utc>>,
    pub last_invoked_at: Option<DateTime<Utc>>,
}

/// Clickweave-internal skill metadata serialized under the
/// `clickweave:` nested key in the YAML frontmatter. Distinct from
/// [`SkillFrontmatter`] (which carries the cross-tool-portable subset
/// Claude Code / Codex CLI / Gemini Extensions consume) so external
/// agent tools see a recognizable skill shape and ignore this block.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct ClickweaveSkillMeta {
    pub state: SkillState,
    pub scope: SkillScope,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub subgoal_text: String,
    #[serde(default)]
    pub subgoal_signature: SubgoalSignature,
    pub applicability: ApplicabilityHints,
    #[serde(default)]
    pub parameter_schema: Vec<ParameterSlot>,
    #[serde(default)]
    pub outputs: Vec<OutputDeclaration>,
    pub outcome_predicate: OutcomePredicate,
    #[serde(default)]
    pub provenance: Vec<ProvenanceEntry>,
    #[serde(default)]
    pub stats: SkillStats,
    #[serde(default)]
    pub edited_by_user: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    #[serde(default)]
    pub produced_node_ids: Vec<Uuid>,
}

impl Default for ClickweaveSkillMeta {
    fn default() -> Self {
        let now = Utc::now();
        Self {
            state: SkillState::Confirmed,
            scope: SkillScope::ProjectLocal,
            tags: Vec::new(),
            subgoal_text: String::new(),
            subgoal_signature: SubgoalSignature(String::new()),
            applicability: ApplicabilityHints {
                apps: Vec::new(),
                hosts: Vec::new(),
                signature: ApplicabilitySignature(String::new()),
            },
            parameter_schema: Vec::new(),
            outputs: Vec::new(),
            outcome_predicate: OutcomePredicate::SubgoalCompleted {
                post_state_world_model_signature: None,
            },
            provenance: Vec::new(),
            stats: SkillStats::default(),
            edited_by_user: false,
            created_at: now,
            updated_at: now,
            produced_node_ids: Vec::new(),
        }
    }
}

impl SubgoalSignature {
    pub fn empty() -> Self {
        Self(String::new())
    }
}

impl Default for SubgoalSignature {
    fn default() -> Self {
        Self::empty()
    }
}

/// Per-section view of a parsed skill body. Populated by
/// `parser::parse_skill_md`; `body_range` is a UTF-8 byte range into
/// the raw markdown body (start..end of the section's prose, including
/// step markers but excluding the `##`/`###` heading line itself).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct SkillSection {
    pub id: String,
    pub heading: String,
    pub level: u8,
    pub step_ids: Vec<String>,
    pub body_range: (usize, usize),
}

/// Coarse-grained replay confidence used by per-section fidelity dots
/// (D7). Defaults to `NoData` until the replay engine in Phase 2 starts
/// stamping the underlying step bundles.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "snake_case")]
pub enum Fidelity {
    Solid,
    Repaired,
    Brittle,
    #[default]
    NoData,
}

/// Minimal `SKILL.md` YAML frontmatter, intentionally cross-tool
/// portable. Mirrors the Claude Code / Codex / Gemini skill-format
/// shape; everything else lives in the markdown body and the fenced
/// `action_sketch` block.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct SkillFrontmatter {
    pub name: String,
    pub description: String,
    pub id: SkillId,
    pub version: u32,
    pub schema_version: u32,
    #[serde(default)]
    pub variables: Vec<SkillFrontmatterVariable>,
    /// Clickweave-internal metadata. External LLM-agent tools (Claude
    /// Code, Codex CLI, Gemini Extensions) ignore unknown YAML keys, so
    /// this nested block preserves Clickweave's runtime metadata
    /// without breaking the cross-tool portability promise from D3.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clickweave: Option<ClickweaveSkillMeta>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct SkillFrontmatterVariable {
    pub name: String,
    #[serde(rename = "type")]
    pub type_: String,
    pub description: Option<String>,
    pub default: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct Skill {
    pub id: String,
    pub version: u32,
    pub state: SkillState,
    pub scope: SkillScope,
    pub name: String,
    pub description: String,
    pub tags: Vec<String>,
    pub subgoal_text: String,
    pub subgoal_signature: SubgoalSignature,
    pub applicability: ApplicabilityHints,
    pub parameter_schema: Vec<ParameterSlot>,
    pub action_sketch: Vec<ActionSketchStep>,
    pub outputs: Vec<OutputDeclaration>,
    pub outcome_predicate: OutcomePredicate,
    pub provenance: Vec<ProvenanceEntry>,
    pub stats: SkillStats,
    pub edited_by_user: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub produced_node_ids: Vec<Uuid>,
    pub body: String,
    /// Parsed marker grammar — populated by the new parser. Empty for
    /// in-memory skills built directly from `action_sketch`.
    #[serde(default)]
    pub schema_version: u32,
    #[serde(default)]
    pub variables: Vec<SkillFrontmatterVariable>,
    #[serde(default)]
    pub sections: Vec<SkillSection>,
    /// In-memory mirror of the on-disk `replay.json` sidecar. Loaded by
    /// `SkillStore::load_all`; persisted via the four-layer atomic
    /// write protocol.
    #[serde(skip)]
    pub replay: Option<crate::agent::skills::replay::ReplayJson>,
}

#[derive(Debug, Clone)]
pub struct SkillContext {
    pub enabled: bool,
    pub project_skills_dir: PathBuf,
    pub global_skills_dir: Option<PathBuf>,
    pub project_id: String,
}

impl SkillContext {
    /// Construct a context that disables every skill hook on the
    /// runner. Mirrors `EpisodicContext::disabled()` — used by tests
    /// and internal callers that don't construct skill paths.
    pub fn disabled() -> Self {
        Self {
            enabled: false,
            project_skills_dir: PathBuf::new(),
            global_skills_dir: None,
            project_id: String::new(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct RecordedStep {
    pub tool_name: String,
    pub arguments: serde_json::Value,
    pub result_text: String,
    pub world_model_pre: WorldModelSnapshot,
    pub world_model_post: WorldModelSnapshot,
}

#[derive(Debug, Clone)]
pub struct RetrievedSkill {
    pub skill: Arc<Skill>,
    pub score: f32,
}

#[derive(Debug)]
pub enum MaybeExtracted {
    Inserted {
        skill_id: String,
        version: u32,
    },
    Merged {
        skill_id: String,
        version: u32,
        occurrence_count: u32,
    },
    Skipped {
        reason: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct BindingCorrection {
    pub step_index: usize,
    pub capture_name: String,
    pub keep: bool,
    pub correction: Option<CaptureClause>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct SkillRefinementProposal {
    pub parameter_schema: Vec<ParameterSlot>,
    pub binding_corrections: Vec<BindingCorrection>,
    pub description: String,
    pub name_suggestion: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum SkillError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("yaml parse error: {0}")]
    Yaml(#[from] serde_yaml::Error),
    #[error("invalid frontmatter: {0}")]
    InvalidFrontmatter(String),
    #[error("missing frontmatter delimiter: {0}")]
    MissingFrontmatterDelimiter(String),
    #[error("skill not found: {0}@v{1}")]
    NotFound(String, u32),
    #[error("skill in draft state cannot be invoked: {0}@v{1}")]
    DraftCannotInvoke(String, u32),
    #[error("invalid parameters: {0}")]
    InvalidParameters(String),
    #[error("substitution error: {0}")]
    Substitution(String),
    #[error("outcome predicate failed: {0}")]
    OutcomeFailed(String),
    #[error("missing fenced action_sketch block in skill body")]
    MissingActionSketchFence,
    #[error("multiple fenced action_sketch blocks in skill body")]
    MultipleActionSketchFences,
    #[error("malformed action_sketch JSON: {0}")]
    MalformedActionSketchJson(serde_json::Error),
    #[error(
        "step marker / action_sketch mismatch — markers: {in_markers:?}, top-level sketch ids: {in_action_sketch_top_level:?}"
    )]
    StepMarkerMismatch {
        in_markers: Vec<String>,
        in_action_sketch_top_level: Vec<String>,
    },
    #[error("duplicate step_id in skill: {0}")]
    DuplicateStepId(String),
    #[error("duplicate section_id in skill: {0}")]
    DuplicateSectionId(String),
    #[error("unresolved variable reference: {{{{{0}}}}}")]
    UnresolvedVariableRef(String),
    #[error("unsupported skill schema_version {found}; max supported is {max_supported}")]
    UnsupportedSchemaVersion { found: u32, max_supported: u32 },
    #[error(
        "replay sidecar/action_sketch step_id mismatch — sketch ids: {skill_step_ids:?}, replay ids: {replay_step_ids:?}"
    )]
    ReplaySidecarMismatch {
        skill_step_ids: Vec<String>,
        replay_step_ids: Vec<String>,
    },
    #[error("skill file changed externally between read and write")]
    ExternalConflict,
}
