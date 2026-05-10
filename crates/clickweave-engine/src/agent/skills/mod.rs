//! Procedural-skills layer (Spec 3).
//!
//! On-disk markdown skill files with YAML frontmatter, a per-project +
//! opt-in global directory tier, an in-memory `SkillIndex` rebuilt per
//! run, and an extractor + replay engine wired into the Spec 1 agent
//! loop.
//!
//! Phase 1 lands the pure-logic modules (types, signatures, frontmatter
//! parser, provenance tracer, loop folding, substitution, outcome
//! predicate, render block). Filesystem I/O, the file watcher, the
//! extractor, the retrieval scorer, and the replay engine arrive in
//! later phases. Everything in this module is `#[allow(dead_code)]`
//! until those phases wire it into `runner.rs`.

#![allow(dead_code)]

pub mod emitter;
pub mod extractor;
pub mod frontmatter;
pub mod index;
pub mod loop_folding;
pub mod outcome;
pub mod parser;
pub mod patch;
pub mod prose_generator;
pub mod provenance;
pub mod render;
pub mod replay;
pub mod retrieval;
pub mod section_history;
pub mod signature;
pub mod store;
pub mod substitution;
pub mod types;
pub mod walkthrough;
pub mod watcher;
pub mod watcher_consumer;

/// Wire-format version stamped into every `SKILL.md` frontmatter and
/// every `replay.json` sidecar. Bumped on any breaking format change;
/// loaders reject `schema_version > SKILL_SCHEMA_VERSION` with
/// `SkillError::UnsupportedSchemaVersion`.
pub const SKILL_SCHEMA_VERSION: u32 = 1;

pub use emitter::emit_skill_md;
pub use index::SkillIndex;
pub use parser::parse_skill_md;
pub use patch::{
    ActionSketchReplacement, MarkdownReplacement, ReplaySidecarMutation, SkillLintError,
    SkillPatch, SkillPatchPrimitive, apply_patch_to_skill, apply_replay_mutations,
    lint_skill_patch,
};
pub use replay::{ReplayJson, ReplayParseError, ReplayStepBundle, SkillFrame, parse_replay_json};
pub use store::{MoveReport, SkillStore, legacy_basename, move_skills_to_project, slugify};
pub use types::{
    ActionSketchStep, ApplicabilityHints, ApplicabilitySignature, BindingCorrection, BindingRef,
    CaptureClause, CaptureSource, ExpectedWorldModelDelta, Fidelity, LoopPredicate, MaybeExtracted,
    OutcomePredicate, OutputDeclaration, ParameterSlot, ProvenanceEntry, RecordedStep,
    RetrievedSkill, Skill, SkillContext, SkillError, SkillFrontmatter, SkillFrontmatterVariable,
    SkillId, SkillRefinementProposal, SkillScope, SkillSection, SkillState, SkillStats,
    SubgoalSignature,
};
