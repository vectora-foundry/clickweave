//! Four-layer `SkillPatch` type with structural lint.
//!
//! A `SkillPatch` represents a requested atomic change to a skill's four
//! on-disk layers: SKILL.md prose (`markdown_replacements`), the fenced
//! `action_sketch` JSON (`action_sketch_replacements`), the YAML frontmatter
//! `variables` list (`variables_additions`), and the `replay.json` sidecar
//! (`replay_sidecar_mutations`).
//!
//! `lint_skill_patch` validates the post-patch skill *before* any filesystem
//! write is attempted, so a lint rejection leaves the on-disk state unchanged.
//! The [`SkillPatchPrimitive`] discriminant declares the semantic intent of a
//! patch so the harness can surface the right diff preview to the user.

#![allow(dead_code)]

use serde::{Deserialize, Serialize};

use super::replay::{ReplayJson, SectionHistoryEntry};
use super::types::{ActionSketchStep, Skill, SkillError, SkillFrontmatterVariable, SkillId};

// ── Patch layers ────────────────────────────────────────────────────────────

/// A targeted prose replacement inside the SKILL.md body. Both
/// `old_text` and `new_text` are UTF-8 strings; the apply function
/// replaces the first occurrence of `old_text` in the body. Overlapping
/// replacements in a single patch are an error.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarkdownReplacement {
    /// The exact string to find in the current body prose (excluding the
    /// fenced action_sketch block).
    pub old_text: String,
    /// The replacement string. May be empty (deletion).
    pub new_text: String,
}

/// A targeted replacement inside the `action_sketch` JSON. The path
/// addresses a specific step by `step_id`; `field` names the top-level
/// key inside the step's JSON object to update.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionSketchReplacement {
    pub step_id: String,
    /// Top-level field inside the `ActionSketchStep::ToolCall` args or
    /// at the step level. Use `"args"` to replace the entire args object.
    pub field: String,
    pub new_value: serde_json::Value,
}

/// Mutations to the `replay.json` sidecar applied in the same atomic
/// write as the SKILL.md changes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ReplaySidecarMutation {
    /// Remove all recorded signals for a step (used when the binding
    /// target changes and old signals are no longer valid).
    ClearSignals { step_id: String },
    /// Record that a section was split into new sections at a given
    /// skill version. Appended to `replay.json::section_history`.
    AppendSectionHistory {
        retired: String,
        split_into: Vec<String>,
        at_version: u32,
    },
    /// Remove the entire step bundle for a step that was deleted from
    /// the action_sketch.
    DeleteStepBundle { step_id: String },
    /// Override or clear the `requires_approval` flag for a step.
    UpdateRequiresApproval {
        step_id: String,
        value: Option<bool>,
    },
}

/// Semantic intent of a patch. Determines the diff preview label and
/// the set of structural lint rules that apply.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SkillPatchPrimitive {
    /// `skill_patch_rebind_target` — changes a step's target kind / args and
    /// clears the old signals so the replay engine re-records from scratch.
    Rebind,
    /// `skill_patch_reorder_sections` — reorders `##` sections and the
    /// corresponding contiguous action_sketch step ranges. No sidecar
    /// mutations.
    Reorder,
    /// `skill_patch_promote_to_variable` — lifts a literal into a
    /// frontmatter `variables` entry, replaces the literal in prose with
    /// `{{variable_name}}`, and updates the matching action_sketch arg.
    Promote,
    /// Catch-all for agent-generated prose edits that don't fit one of the
    /// three named primitives.
    FreeFormProse,
}

// ── The patch ───────────────────────────────────────────────────────────────

/// A four-layer atomic skill patch. Created by one of the three named
/// primitive helpers (`skill_patch_rebind_target`, etc.) or constructed
/// directly for free-form prose edits.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillPatch {
    pub skill_id: SkillId,
    pub markdown_replacements: Vec<MarkdownReplacement>,
    pub action_sketch_replacements: Vec<ActionSketchReplacement>,
    pub variables_additions: Vec<SkillFrontmatterVariable>,
    pub replay_sidecar_mutations: Vec<ReplaySidecarMutation>,
    pub primitive: SkillPatchPrimitive,
}

// ── Lint errors ─────────────────────────────────────────────────────────────

/// A single lint violation found during structural lint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkillLintError {
    /// `{{variable_name}}` reference in the body or action_sketch args does
    /// not resolve to any entry in `skill.variables`.
    UnresolvedVariableRef(String),
    /// A `<!-- step: step_id -->` marker in the prose does not correspond to
    /// a top-level step in the action_sketch, or vice versa.
    OrphanStepMarker(String),
    /// Two sections share the same `id`.
    DuplicateSectionId(String),
    /// Two steps share the same `step_id`.
    DuplicateStepId(String),
    /// An `action_sketch_replacement` referenced a `step_id` that is not
    /// present in the (post-patch) action_sketch.
    UnknownStepId(String),
    /// A sidecar mutation referenced a step_id that is not present in the
    /// post-patch action_sketch.
    SidecarStepIdMismatch { step_id: String, reason: String },
    /// A `replay_sidecar_mutation::DeleteStepBundle` was issued for a
    /// step_id that still exists in the post-patch action_sketch.
    DeleteBundleForLiveStep(String),
    /// The frontmatter still fails to parse as valid YAML after the patch
    /// (caught at apply time, not lint time, but surfaced here for uniformity).
    FrontmatterInvalid(String),
}

impl std::fmt::Display for SkillLintError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnresolvedVariableRef(name) => {
                write!(f, "unresolved variable reference: {{{{{name}}}}}")
            }
            Self::OrphanStepMarker(id) => write!(f, "orphan step marker: {id}"),
            Self::DuplicateSectionId(id) => write!(f, "duplicate section_id: {id}"),
            Self::DuplicateStepId(id) => write!(f, "duplicate step_id: {id}"),
            Self::UnknownStepId(id) => write!(f, "unknown step_id in replacement: {id}"),
            Self::SidecarStepIdMismatch { step_id, reason } => {
                write!(f, "sidecar step_id mismatch for {step_id}: {reason}")
            }
            Self::DeleteBundleForLiveStep(id) => {
                write!(f, "DeleteStepBundle issued for live step: {id}")
            }
            Self::FrontmatterInvalid(msg) => write!(f, "frontmatter invalid after patch: {msg}"),
        }
    }
}

// ── Apply helpers (pure functions) ──────────────────────────────────────────

/// Apply all `markdown_replacements` to the prose body in order. Returns
/// an error (via `SkillError::InvalidParameters`) if a replacement's
/// `old_text` is not found in the current body.
pub fn apply_markdown_replacements(
    body: &str,
    replacements: &[MarkdownReplacement],
) -> Result<String, SkillError> {
    let mut current = body.to_string();
    for r in replacements {
        if !current.contains(r.old_text.as_str()) {
            return Err(SkillError::InvalidParameters(format!(
                "markdown_replacement: old_text {:?} not found in body",
                r.old_text
            )));
        }
        current = current.replacen(&r.old_text, &r.new_text, 1);
    }
    Ok(current)
}

/// Apply all `action_sketch_replacements` to the action_sketch in-memory
/// representation. Only `ToolCall` steps support field-level replacements;
/// `Loop` steps are addressed by their own `step_id` at the loop level
/// but inner body steps are not directly patchable in Phase 1.
pub fn apply_action_sketch_replacements(
    mut sketch: Vec<ActionSketchStep>,
    replacements: &[ActionSketchReplacement],
) -> Result<Vec<ActionSketchStep>, SkillError> {
    for r in replacements {
        let step = find_step_mut(&mut sketch, &r.step_id).ok_or_else(|| {
            SkillError::InvalidParameters(format!(
                "action_sketch_replacement: step_id {:?} not found",
                r.step_id
            ))
        })?;
        match step {
            ActionSketchStep::ToolCall { args, .. } if r.field == "args" => {
                *args = r.new_value.clone();
            }
            ActionSketchStep::ToolCall { args, .. } => {
                // Patch a named sub-key inside `args`.
                let obj = args.as_object_mut().ok_or_else(|| {
                    SkillError::InvalidParameters(format!(
                        "step {:?}: args is not an object; cannot patch field {:?}",
                        r.step_id, r.field
                    ))
                })?;
                obj.insert(r.field.clone(), r.new_value.clone());
            }
            ActionSketchStep::Loop { .. } => {
                return Err(SkillError::InvalidParameters(format!(
                    "action_sketch_replacement: step {:?} is a Loop; loop-level patches unsupported in Phase 1",
                    r.step_id
                )));
            }
        }
    }
    Ok(sketch)
}

/// Apply `variables_additions` to the skill's variables list. Duplicate
/// variable names are rejected.
pub fn apply_variables_additions(
    mut variables: Vec<SkillFrontmatterVariable>,
    additions: &[SkillFrontmatterVariable],
) -> Result<Vec<SkillFrontmatterVariable>, SkillError> {
    for addition in additions {
        if variables.iter().any(|v| v.name == addition.name) {
            return Err(SkillError::InvalidParameters(format!(
                "variables_additions: variable {:?} already exists",
                addition.name
            )));
        }
        variables.push(addition.clone());
    }
    Ok(variables)
}

/// Apply `replay_sidecar_mutations` to a `ReplayJson` in-memory value.
pub fn apply_replay_mutations(
    mut replay: ReplayJson,
    mutations: &[ReplaySidecarMutation],
) -> Result<ReplayJson, SkillError> {
    for mutation in mutations {
        match mutation {
            ReplaySidecarMutation::ClearSignals { step_id } => {
                if let Some(bundle) = replay.steps.get_mut(step_id) {
                    bundle.signals.clear();
                }
                // Absent bundle is a no-op — the signal list is already empty.
            }
            ReplaySidecarMutation::AppendSectionHistory {
                retired,
                split_into,
                at_version,
            } => {
                replay.section_history.push(SectionHistoryEntry {
                    retired: retired.clone(),
                    split_into: split_into.clone(),
                    at_version: *at_version,
                    at: chrono::Utc::now(),
                });
            }
            ReplaySidecarMutation::DeleteStepBundle { step_id } => {
                replay.steps.remove(step_id);
            }
            ReplaySidecarMutation::UpdateRequiresApproval { step_id, value } => {
                let bundle = replay.steps.entry(step_id.clone()).or_default();
                bundle.requires_approval = *value;
            }
        }
    }
    Ok(replay)
}

/// Apply the full patch to an in-memory `(Skill, ReplayJson)` pair and
/// return the updated values. This is a pure function — no I/O is
/// performed. The caller is responsible for the journal write.
pub fn apply_patch_to_skill(
    skill: &Skill,
    replay: ReplayJson,
    patch: &SkillPatch,
) -> Result<(Skill, ReplayJson), SkillError> {
    let new_body = apply_markdown_replacements(&skill.body, &patch.markdown_replacements)?;
    let new_sketch = apply_action_sketch_replacements(
        skill.action_sketch.clone(),
        &patch.action_sketch_replacements,
    )?;
    let new_variables =
        apply_variables_additions(skill.variables.clone(), &patch.variables_additions)?;
    let new_replay = apply_replay_mutations(replay, &patch.replay_sidecar_mutations)?;

    let mut new_skill = skill.clone();
    new_skill.body = new_body;
    new_skill.action_sketch = new_sketch;
    new_skill.variables = new_variables;

    Ok((new_skill, new_replay))
}

// ── Structural lint ──────────────────────────────────────────────────────────

/// Run structural lint on the post-patch `Skill` and `ReplayJson`. All
/// violations are collected rather than short-circuiting so the caller
/// can surface the full list in the diff preview.
///
/// Must be called *before* the journal write opens. A non-empty error
/// list must abort the apply without creating any `.tx/` state.
pub fn lint_skill_patch(
    new_skill: &Skill,
    new_replay: &ReplayJson,
    patch: &SkillPatch,
) -> Result<(), Vec<SkillLintError>> {
    let mut errors = Vec::new();

    // 1. Section id uniqueness.
    let mut seen_sections = std::collections::HashSet::new();
    for section in &new_skill.sections {
        if !seen_sections.insert(section.id.clone()) {
            errors.push(SkillLintError::DuplicateSectionId(section.id.clone()));
        }
    }

    // 2. Step id uniqueness (top-level only in Phase 1).
    let all_top_level_step_ids: Vec<String> = new_skill
        .action_sketch
        .iter()
        .map(|s| top_level_step_id(s).to_string())
        .collect();
    let mut seen_steps = std::collections::HashSet::new();
    for id in &all_top_level_step_ids {
        if !seen_steps.insert(id.clone()) {
            errors.push(SkillLintError::DuplicateStepId(id.clone()));
        }
    }

    // 3. Step marker / top-level action_sketch correspondence.
    // Only enforced when markers are present (same as parser rule).
    let marker_step_ids: Vec<String> = new_skill
        .sections
        .iter()
        .flat_map(|s| s.step_ids.iter().cloned())
        .collect();
    if !marker_step_ids.is_empty() {
        let top_level_set: std::collections::BTreeSet<&str> =
            all_top_level_step_ids.iter().map(|s| s.as_str()).collect();
        for id in &marker_step_ids {
            if !top_level_set.contains(id.as_str()) {
                errors.push(SkillLintError::OrphanStepMarker(id.clone()));
            }
        }
    }

    // 4. Every action_sketch_replacement step_id must exist in the
    // post-patch sketch.
    for r in &patch.action_sketch_replacements {
        if !all_top_level_step_ids.contains(&r.step_id) {
            errors.push(SkillLintError::UnknownStepId(r.step_id.clone()));
        }
    }

    // 5. Sidecar mutation step_ids must be consistent with post-patch sketch.
    for mutation in &patch.replay_sidecar_mutations {
        match mutation {
            ReplaySidecarMutation::ClearSignals { step_id }
            | ReplaySidecarMutation::UpdateRequiresApproval { step_id, .. } => {
                if !all_top_level_step_ids.contains(step_id) {
                    errors.push(SkillLintError::SidecarStepIdMismatch {
                        step_id: step_id.clone(),
                        reason: "step not in post-patch action_sketch".into(),
                    });
                }
            }
            ReplaySidecarMutation::DeleteStepBundle { step_id } => {
                // DeleteStepBundle must only reference steps that are
                // absent from the post-patch sketch.
                if all_top_level_step_ids.contains(step_id) {
                    errors.push(SkillLintError::DeleteBundleForLiveStep(step_id.clone()));
                }
            }
            ReplaySidecarMutation::AppendSectionHistory { .. } => {
                // No step-id reference to validate.
            }
        }
    }

    // 6. Variable references in body resolve.
    let variable_names: std::collections::HashSet<&str> = new_skill
        .variables
        .iter()
        .map(|v| v.name.as_str())
        .collect();
    for cap in variable_references_in_body(&new_skill.body) {
        if !variable_names.contains(cap) {
            errors.push(SkillLintError::UnresolvedVariableRef(cap.to_string()));
        }
    }

    // 7. replay.json step keys must be a subset of action_sketch step ids
    // after the patch.
    let all_sketch_ids = collect_all_sketch_step_ids(&new_skill.action_sketch);
    for replay_step_id in new_replay.steps.keys() {
        if !all_sketch_ids.contains(replay_step_id.as_str()) {
            errors.push(SkillLintError::SidecarStepIdMismatch {
                step_id: replay_step_id.clone(),
                reason: "replay.json key not in post-patch action_sketch".into(),
            });
        }
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

// ── Internal helpers ─────────────────────────────────────────────────────────

fn top_level_step_id(step: &ActionSketchStep) -> &str {
    match step {
        ActionSketchStep::ToolCall { step_id, .. } => step_id,
        ActionSketchStep::Loop { step_id, .. } => step_id,
    }
}

fn find_step_mut<'a>(
    sketch: &'a mut [ActionSketchStep],
    step_id: &str,
) -> Option<&'a mut ActionSketchStep> {
    sketch.iter_mut().find(|s| top_level_step_id(s) == step_id)
}

/// Collect all step_ids recursively (top-level + loop body).
fn collect_all_sketch_step_ids(sketch: &[ActionSketchStep]) -> std::collections::HashSet<&str> {
    let mut out = std::collections::HashSet::new();
    for step in sketch {
        match step {
            ActionSketchStep::ToolCall { step_id, .. } => {
                out.insert(step_id.as_str());
            }
            ActionSketchStep::Loop { step_id, body, .. } => {
                out.insert(step_id.as_str());
                out.extend(collect_all_sketch_step_ids(body));
            }
        }
    }
    out
}

/// Extract `{{variable_name}}` references from a body string.
fn variable_references_in_body(body: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut remaining = body;
    while let Some(open) = remaining.find("{{") {
        let after_open = &remaining[open + 2..];
        if let Some(close) = after_open.find("}}") {
            let name = after_open[..close].trim();
            if !name.is_empty() {
                out.push(name);
            }
            remaining = &after_open[close + 2..];
        } else {
            break;
        }
    }
    out
}

// ── Named-primitive constructors ─────────────────────────────────────────────

impl SkillPatch {
    /// Synthesize a `Rebind` patch from the args passed to the
    /// `skill_patch_rebind_target` pseudo-tool. Returns an error string
    /// when a required argument is missing or malformed.
    ///
    /// The patch carries:
    /// - One `ActionSketchReplacement` that rewrites the step's entire `args`
    ///   with `new_target_args`.
    /// - One `ReplaySidecarMutation::ClearSignals` so the replay engine
    ///   re-records from scratch with the new target.
    pub fn from_rebind_target_args(args: &serde_json::Value) -> Result<Self, String> {
        let skill_id = args
            .get("skill_id")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| {
                "skill_patch_rebind_target: missing required field `skill_id`".to_string()
            })?
            .to_string();
        let step_id = args
            .get("step_id")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| {
                "skill_patch_rebind_target: missing required field `step_id`".to_string()
            })?
            .to_string();
        let new_target_args = args.get("new_target_args").cloned().ok_or_else(|| {
            "skill_patch_rebind_target: missing required field `new_target_args`".to_string()
        })?;

        Ok(SkillPatch {
            skill_id,
            markdown_replacements: vec![],
            action_sketch_replacements: vec![ActionSketchReplacement {
                step_id: step_id.clone(),
                field: "args".to_string(),
                new_value: new_target_args,
            }],
            variables_additions: vec![],
            replay_sidecar_mutations: vec![ReplaySidecarMutation::ClearSignals { step_id }],
            primitive: SkillPatchPrimitive::Rebind,
        })
    }

    /// Synthesize a `Reorder` patch from the args passed to the
    /// `skill_patch_reorder_sections` pseudo-tool. Returns an error string
    /// when a required argument is missing or malformed.
    ///
    /// The patch carries no layer mutations on its own — section reordering
    /// requires the full in-memory skill body (parsed sections) which is not
    /// available at parse time. The harness resolves the reorder by reading
    /// the skill from disk and applying the ordering at dispatch time. The
    /// patch records the desired `ordered_section_ids` in the
    /// `markdown_replacements` field as a sentinel so downstream code can
    /// identify the intent without re-parsing the LLM args.
    ///
    /// **Note:** the actual markdown reorder and action_sketch step
    /// reorder are applied in a later phase when the skill is loaded.
    /// This constructor only validates the required fields and stores
    /// the section order.
    pub fn from_reorder_sections_args(args: &serde_json::Value) -> Result<Self, String> {
        let skill_id = args
            .get("skill_id")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| {
                "skill_patch_reorder_sections: missing required field `skill_id`".to_string()
            })?
            .to_string();
        let ordered_section_ids = args
            .get("ordered_section_ids")
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| {
                "skill_patch_reorder_sections: missing required field `ordered_section_ids`"
                    .to_string()
            })?
            .iter()
            .map(|v| {
                v.as_str()
                    .map(str::to_string)
                    .ok_or_else(|| {
                        "skill_patch_reorder_sections: `ordered_section_ids` must be an array of strings"
                            .to_string()
                    })
            })
            .collect::<Result<Vec<_>, _>>()?;

        if ordered_section_ids.is_empty() {
            return Err(
                "skill_patch_reorder_sections: `ordered_section_ids` must not be empty".to_string(),
            );
        }

        // Encode the desired order as a sentinel `MarkdownReplacement`
        // with `old_text = "__reorder__"` so phase-N apply code can
        // distinguish a reorder patch from a prose edit without inspecting
        // the `primitive` discriminant. The `new_text` carries the
        // newline-joined section id list.
        Ok(SkillPatch {
            skill_id,
            markdown_replacements: vec![MarkdownReplacement {
                old_text: "__reorder__".to_string(),
                new_text: ordered_section_ids.join("\n"),
            }],
            action_sketch_replacements: vec![],
            variables_additions: vec![],
            replay_sidecar_mutations: vec![],
            primitive: SkillPatchPrimitive::Reorder,
        })
    }

    /// Synthesize a `Promote` patch from the args passed to the
    /// `skill_patch_promote_to_variable` pseudo-tool. Returns an error
    /// string when a required argument is missing or malformed.
    ///
    /// The patch carries:
    /// - A `SkillFrontmatterVariable` entry in `variables_additions` for the
    ///   new variable.
    /// - One `ActionSketchReplacement` that updates the addressed arg to the
    ///   `{{variable_name}}` template reference.
    ///
    /// Prose replacement and image-crop signal clearing are deferred to the
    /// apply phase which has the full body text; this constructor only
    /// captures the parameter fields.
    pub fn from_promote_to_variable_args(args: &serde_json::Value) -> Result<Self, String> {
        let skill_id = args
            .get("skill_id")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| {
                "skill_patch_promote_to_variable: missing required field `skill_id`".to_string()
            })?
            .to_string();
        let step_id = args
            .get("step_id")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| {
                "skill_patch_promote_to_variable: missing required field `step_id`".to_string()
            })?
            .to_string();
        let arg_path = args
            .get("arg_path")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| {
                "skill_patch_promote_to_variable: missing required field `arg_path`".to_string()
            })?
            .to_string();
        let variable_name = args
            .get("variable_name")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| {
                "skill_patch_promote_to_variable: missing required field `variable_name`"
                    .to_string()
            })?
            .to_string();
        let variable_type = args
            .get("variable_type")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| {
                "skill_patch_promote_to_variable: missing required field `variable_type`"
                    .to_string()
            })?
            .to_string();
        let default = args.get("default").cloned();

        Ok(SkillPatch {
            skill_id,
            markdown_replacements: vec![],
            action_sketch_replacements: vec![ActionSketchReplacement {
                step_id,
                field: arg_path,
                new_value: serde_json::Value::String(format!("{{{{{variable_name}}}}}")),
            }],
            variables_additions: vec![SkillFrontmatterVariable {
                name: variable_name,
                type_: variable_type,
                description: None,
                default,
            }],
            replay_sidecar_mutations: vec![],
            primitive: SkillPatchPrimitive::Promote,
        })
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::skills::replay::ReplayJson;
    use crate::agent::skills::types::*;

    fn minimal_tool_step(step_id: &str, tool: &str) -> ActionSketchStep {
        ActionSketchStep::ToolCall {
            step_id: step_id.to_string(),
            tool: tool.to_string(),
            args: serde_json::json!({}),
            captures_pre: vec![],
            captures: vec![],
            expected_world_model_delta: ExpectedWorldModelDelta::default(),
            requires_approval: None,
        }
    }

    fn minimal_skill(id: &str) -> Skill {
        let now = chrono::Utc::now();
        Skill {
            id: id.into(),
            version: 1,
            state: SkillState::Draft,
            scope: SkillScope::ProjectLocal,
            name: "Test".into(),
            description: "desc".into(),
            tags: vec![],
            subgoal_text: "open".into(),
            subgoal_signature: SubgoalSignature("sig".into()),
            applicability: ApplicabilityHints {
                apps: vec![],
                hosts: vec![],
                signature: ApplicabilitySignature("appsig".into()),
            },
            parameter_schema: vec![],
            action_sketch: vec![minimal_tool_step("s_001", "click")],
            outputs: vec![],
            outcome_predicate: OutcomePredicate::SubgoalCompleted {
                post_state_world_model_signature: None,
            },
            provenance: vec![],
            stats: SkillStats::default(),
            edited_by_user: false,
            created_at: now,
            updated_at: now,
            produced_node_ids: vec![],
            body: String::new(),
            schema_version: 1,
            variables: vec![],
            sections: vec![],
            replay: None,
        }
    }

    fn empty_replay(skill_id: &str) -> ReplayJson {
        ReplayJson {
            skill_id: skill_id.into(),
            schema_version: 1,
            ..Default::default()
        }
    }

    // -- lint::variable_references positive case --

    #[test]
    fn lint_passes_when_variable_refs_resolve() {
        let mut skill = minimal_skill("skl_var");
        skill.body = "Click {{button_label}}".into();
        skill.variables = vec![SkillFrontmatterVariable {
            name: "button_label".into(),
            type_: "string".into(),
            description: None,
            default: None,
        }];
        let replay = empty_replay("skl_var");
        let patch = SkillPatch {
            skill_id: "skl_var".into(),
            markdown_replacements: vec![],
            action_sketch_replacements: vec![],
            variables_additions: vec![],
            replay_sidecar_mutations: vec![],
            primitive: SkillPatchPrimitive::FreeFormProse,
        };
        assert!(lint_skill_patch(&skill, &replay, &patch).is_ok());
    }

    // -- lint::variable_references negative case --

    #[test]
    fn lint_fails_when_variable_ref_unresolved() {
        let mut skill = minimal_skill("skl_unresolved");
        skill.body = "Click {{missing_var}}".into();
        let replay = empty_replay("skl_unresolved");
        let patch = SkillPatch {
            skill_id: "skl_unresolved".into(),
            markdown_replacements: vec![],
            action_sketch_replacements: vec![],
            variables_additions: vec![],
            replay_sidecar_mutations: vec![],
            primitive: SkillPatchPrimitive::FreeFormProse,
        };
        let errs = lint_skill_patch(&skill, &replay, &patch).unwrap_err();
        assert!(
            errs.iter().any(
                |e| matches!(e, SkillLintError::UnresolvedVariableRef(n) if n == "missing_var")
            )
        );
    }

    // -- lint::orphan_step_marker positive case --

    #[test]
    fn lint_passes_with_no_markers() {
        let skill = minimal_skill("skl_nomarkers");
        let replay = empty_replay("skl_nomarkers");
        let patch = SkillPatch {
            skill_id: "skl_nomarkers".into(),
            markdown_replacements: vec![],
            action_sketch_replacements: vec![],
            variables_additions: vec![],
            replay_sidecar_mutations: vec![],
            primitive: SkillPatchPrimitive::FreeFormProse,
        };
        assert!(lint_skill_patch(&skill, &replay, &patch).is_ok());
    }

    // -- lint::orphan_step_marker negative case --

    #[test]
    fn lint_fails_with_orphan_step_marker() {
        let mut skill = minimal_skill("skl_orphan");
        skill.sections = vec![SkillSection {
            id: "sec_1".into(),
            heading: "Launch".into(),
            level: 2,
            step_ids: vec!["s_999".into()], // orphan — not in action_sketch
            body_range: (0, 10),
        }];
        let replay = empty_replay("skl_orphan");
        let patch = SkillPatch {
            skill_id: "skl_orphan".into(),
            markdown_replacements: vec![],
            action_sketch_replacements: vec![],
            variables_additions: vec![],
            replay_sidecar_mutations: vec![],
            primitive: SkillPatchPrimitive::FreeFormProse,
        };
        let errs = lint_skill_patch(&skill, &replay, &patch).unwrap_err();
        assert!(
            errs.iter()
                .any(|e| matches!(e, SkillLintError::OrphanStepMarker(id) if id == "s_999"))
        );
    }

    // -- lint::duplicate_section_id negative case --

    #[test]
    fn lint_fails_with_duplicate_section_id() {
        let mut skill = minimal_skill("skl_dup_sec");
        skill.sections = vec![
            SkillSection {
                id: "sec_1".into(),
                heading: "A".into(),
                level: 2,
                step_ids: vec![],
                body_range: (0, 5),
            },
            SkillSection {
                id: "sec_1".into(),
                heading: "B".into(),
                level: 2,
                step_ids: vec![],
                body_range: (5, 10),
            },
        ];
        let replay = empty_replay("skl_dup_sec");
        let patch = SkillPatch {
            skill_id: "skl_dup_sec".into(),
            markdown_replacements: vec![],
            action_sketch_replacements: vec![],
            variables_additions: vec![],
            replay_sidecar_mutations: vec![],
            primitive: SkillPatchPrimitive::FreeFormProse,
        };
        let errs = lint_skill_patch(&skill, &replay, &patch).unwrap_err();
        assert!(
            errs.iter()
                .any(|e| matches!(e, SkillLintError::DuplicateSectionId(id) if id == "sec_1"))
        );
    }

    // -- lint::delete_bundle_for_live_step negative case --

    #[test]
    fn lint_fails_delete_bundle_for_live_step() {
        let skill = minimal_skill("skl_del_live");
        let replay = empty_replay("skl_del_live");
        let patch = SkillPatch {
            skill_id: "skl_del_live".into(),
            markdown_replacements: vec![],
            action_sketch_replacements: vec![],
            variables_additions: vec![],
            replay_sidecar_mutations: vec![ReplaySidecarMutation::DeleteStepBundle {
                step_id: "s_001".into(), // still in action_sketch
            }],
            primitive: SkillPatchPrimitive::FreeFormProse,
        };
        let errs = lint_skill_patch(&skill, &replay, &patch).unwrap_err();
        assert!(
            errs.iter()
                .any(|e| matches!(e, SkillLintError::DeleteBundleForLiveStep(id) if id == "s_001"))
        );
    }

    // -- apply_markdown_replacements --

    #[test]
    fn markdown_replacement_applies_first_occurrence() {
        let body = "Click the Save button. Then click Save again.";
        let replacements = vec![MarkdownReplacement {
            old_text: "Save".into(),
            new_text: "Submit".into(),
        }];
        let result = apply_markdown_replacements(body, &replacements).unwrap();
        assert_eq!(result, "Click the Submit button. Then click Save again.");
    }

    #[test]
    fn markdown_replacement_fails_when_old_text_not_found() {
        let body = "Click OK".to_string();
        let replacements = vec![MarkdownReplacement {
            old_text: "Cancel".into(),
            new_text: "Abort".into(),
        }];
        let err = apply_markdown_replacements(&body, &replacements).unwrap_err();
        assert!(matches!(err, SkillError::InvalidParameters(_)));
    }

    // -- apply_action_sketch_replacements --

    #[test]
    fn action_sketch_replacement_updates_args() {
        let sketch = vec![minimal_tool_step("s_001", "click")];
        let replacements = vec![ActionSketchReplacement {
            step_id: "s_001".into(),
            field: "args".into(),
            new_value: serde_json::json!({"x": 100, "y": 200}),
        }];
        let result = apply_action_sketch_replacements(sketch, &replacements).unwrap();
        match &result[0] {
            ActionSketchStep::ToolCall { args, .. } => {
                assert_eq!(args["x"], 100);
            }
            _ => panic!("expected ToolCall"),
        }
    }

    #[test]
    fn action_sketch_replacement_fails_for_unknown_step() {
        let sketch = vec![minimal_tool_step("s_001", "click")];
        let replacements = vec![ActionSketchReplacement {
            step_id: "s_999".into(),
            field: "args".into(),
            new_value: serde_json::json!({}),
        }];
        let err = apply_action_sketch_replacements(sketch, &replacements).unwrap_err();
        assert!(matches!(err, SkillError::InvalidParameters(_)));
    }

    // -- apply_replay_mutations --

    #[test]
    fn clear_signals_removes_all_signals_for_step() {
        use crate::agent::skills::replay::{ReplayStepBundle, Signal};
        let mut steps = std::collections::HashMap::new();
        steps.insert(
            "s_001".to_string(),
            ReplayStepBundle {
                signals: vec![Signal::Coords { x: 10, y: 20 }],
                ..Default::default()
            },
        );
        let replay = ReplayJson {
            skill_id: "skl_clear".into(),
            schema_version: 1,
            steps,
            section_history: vec![],
        };
        let mutations = vec![ReplaySidecarMutation::ClearSignals {
            step_id: "s_001".into(),
        }];
        let result = apply_replay_mutations(replay, &mutations).unwrap();
        assert!(result.steps["s_001"].signals.is_empty());
    }

    #[test]
    fn append_section_history_adds_entry() {
        let replay = empty_replay("skl_hist");
        let mutations = vec![ReplaySidecarMutation::AppendSectionHistory {
            retired: "sec_old".into(),
            split_into: vec!["sec_a".into(), "sec_b".into()],
            at_version: 2,
        }];
        let result = apply_replay_mutations(replay, &mutations).unwrap();
        assert_eq!(result.section_history.len(), 1);
        assert_eq!(result.section_history[0].retired, "sec_old");
        assert_eq!(result.section_history[0].split_into.len(), 2);
    }
}
