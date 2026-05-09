//! Online skill extractor that fires at every `CompleteSubgoal`
//! boundary.
//!
//! Walks the recorded action sequence between the matching
//! `PushSubgoal` and `CompleteSubgoal` mutations, builds an action
//! sketch via [`provenance::build_action_sketch`], computes the
//! subgoal + applicability signatures, and either:
//!
//! - **Merges** into an existing project-local draft skill in the same
//!   `(id, version)` family when the new sketch matches byte-for-byte
//!   (bumps `occurrence_count`, appends a `ProvenanceEntry`, and
//!   re-emits the file at `version + 1`).
//! - **Inserts** a fresh `(id, v + 1)` row when an existing
//!   project-local draft skill at the same signature has a structurally
//!   different sketch.
//! - **Inserts** a brand new draft skill at `version 1` when no
//!   skill at that signature exists yet.
//!
//! Disabled when `skill_ctx.enabled == false` — the runner threads the
//! enable flag through directly so disabling is one branch on a single
//! field.

#![allow(dead_code)]

use std::sync::Arc;

use chrono::Utc;
use parking_lot::RwLock;
use serde_json::Value;
use uuid::Uuid;

use super::index::SkillIndex;
use super::provenance::build_action_sketch;
use super::signature::compute_applicability_signature;
use super::store::SkillStore;
use super::types::{
    ActionSketchStep, ApplicabilityHints, ApplicabilitySignature, MaybeExtracted, OutcomePredicate,
    ProvenanceEntry, RecordedStep, Skill, SkillContext, SkillError, SkillScope, SkillState,
    SkillStats, SubgoalSignature,
};
use crate::agent::task_state::Milestone;
use crate::agent::world_model::WorldModel;

/// Online extraction at the `CompleteSubgoal` boundary. Returns
/// [`MaybeExtracted::Skipped`] when the layer is disabled or the
/// action sequence is empty (extraction needs at least one step).
#[allow(clippy::too_many_arguments)]
pub async fn maybe_extract_skill(
    completed: &Milestone,
    action_sequence: &[RecordedStep],
    pre_state_signature: SubgoalSignature,
    post_state_world_model: &WorldModel,
    skill_index: &Arc<RwLock<SkillIndex>>,
    skill_store: &SkillStore,
    skill_ctx: &SkillContext,
    run_id: Uuid,
    workflow_hash: &str,
    step_index: usize,
    produced_node_ids: &[Uuid],
) -> Result<MaybeExtracted, SkillError> {
    if !skill_ctx.enabled {
        return Ok(MaybeExtracted::Skipped {
            reason: "disabled".into(),
        });
    }
    if action_sequence.is_empty() {
        return Ok(MaybeExtracted::Skipped {
            reason: "empty action sequence".into(),
        });
    }

    let subgoal_text = completed.text.clone();
    let applicability_signature = compute_applicability_signature(post_state_world_model);
    let applicability = applicability_hints_from_world_model(
        post_state_world_model,
        applicability_signature.clone(),
    );

    let new_sketch = build_action_sketch(action_sequence);
    if action_sketch_contains_unverified_side_effect(&new_sketch) {
        return Ok(MaybeExtracted::Skipped {
            reason: "unverified side-effectful action".into(),
        });
    }

    // Look at every skill in the same signature family. Phase 3
    // matches purely on `subgoal_signature`; Phase 4+5 layer in the
    // applicability comparison so cross-app skills don't collide.
    let candidates = skill_index
        .read()
        .skills_with_signature(&pre_state_signature);
    let mergeable_draft_candidates: Vec<_> = candidates
        .iter()
        .filter(|skill| is_mergeable_project_local_draft(skill.as_ref()))
        .cloned()
        .collect();

    let now = Utc::now();
    let provenance_entry = ProvenanceEntry {
        run_id: run_id.to_string(),
        step_index,
        completed_at: now,
        workflow_hash: workflow_hash.to_string(),
    };

    if let Some(existing) = mergeable_draft_candidates
        .iter()
        .filter(|s| sketches_equivalent(&s.action_sketch, &new_sketch))
        .max_by_key(|s| s.version)
    {
        // Same family + same sketch → merge into a new version with
        // bumped occurrence_count + appended provenance + recomputed
        // success_rate. Files are immutable once written, so the merge
        // emits a fresh version rather than mutating the source row.
        let mut merged: Skill = (**existing).clone();
        merged.version += 1;
        merged.stats.occurrence_count += 1;
        merged.stats.last_seen_at = Some(now);
        merged.provenance.push(provenance_entry);
        merged.updated_at = now;
        // Layer the fresh node-lineage onto the running list (for
        // `prune_skill_lineage_for_nodes` selective-delete).
        merged
            .produced_node_ids
            .extend_from_slice(produced_node_ids);
        // Recompute success_rate as `successes / occurrence_count`,
        // counting every recorded sequence as a success in Phase 3
        // (failure-stamping lands with replay in Phase 4).
        merged.stats.success_rate = 1.0;

        skill_store.write_skill(&merged)?;
        let occurrence_count = merged.stats.occurrence_count;
        let version = merged.version;
        let id = merged.id.clone();
        skill_index.write().upsert(merged);
        return Ok(MaybeExtracted::Merged {
            skill_id: id,
            version,
            occurrence_count,
        });
    }

    if let Some(divergent) = mergeable_draft_candidates.iter().max_by_key(|s| s.version) {
        // Same signature, divergent sketch → emit a new version on
        // top of the highest existing project-local draft version in
        // the family.
        let id = divergent.id.clone();
        let version = divergent.version + 1;
        let skill = build_draft_skill(
            id.clone(),
            version,
            subgoal_text,
            pre_state_signature,
            applicability,
            new_sketch,
            produced_node_ids,
            provenance_entry,
            now,
        );
        skill_store.write_skill(&skill)?;
        skill_index.write().upsert(skill);
        return Ok(MaybeExtracted::Inserted {
            skill_id: id,
            version,
        });
    }

    // No mergeable project-local draft → brand-new draft skill at
    // version 1. Existing confirmed/promoted/global skills at the
    // signature remain immutable retrieval sources.
    let id = synthesize_unique_draft_skill_id(
        &completed.text,
        &pre_state_signature,
        &candidates,
        skill_index,
    );
    let skill = build_draft_skill(
        id.clone(),
        1,
        subgoal_text,
        pre_state_signature,
        applicability,
        new_sketch,
        produced_node_ids,
        provenance_entry,
        now,
    );
    skill_store.write_skill(&skill)?;
    skill_index.write().upsert(skill);
    Ok(MaybeExtracted::Inserted {
        skill_id: id,
        version: 1,
    })
}

fn is_mergeable_project_local_draft(skill: &Skill) -> bool {
    skill.scope == SkillScope::ProjectLocal
        && skill.state == SkillState::Draft
        && !skill.edited_by_user
}

fn action_sketch_contains_unverified_side_effect(steps: &[ActionSketchStep]) -> bool {
    steps.iter().any(|step| match step {
        ActionSketchStep::ToolCall {
            tool,
            args,
            expected_world_model_delta,
            ..
        } => {
            expected_world_model_delta.changed_fields.is_empty()
                && script_tool_args_have_obvious_side_effect(tool, args)
        }
        ActionSketchStep::Loop { body, .. } => action_sketch_contains_unverified_side_effect(body),
    })
}

fn script_tool_args_have_obvious_side_effect(tool: &str, args: &Value) -> bool {
    if !matches!(
        tool,
        "cdp_evaluate_script" | "evaluate_script" | "execute_script"
    ) && !tool.ends_with("_evaluate_script")
        && !tool.ends_with("_execute_script")
    {
        return false;
    }

    ["function", "script", "code"]
        .iter()
        .filter_map(|key| args.get(*key).and_then(Value::as_str))
        .any(script_source_has_obvious_side_effect)
}

fn script_source_has_obvious_side_effect(source: &str) -> bool {
    let source = source.to_ascii_lowercase();
    [
        ".click(",
        ".dispatch_event(",
        ".dispatchevent(",
        ".submit(",
        ".focus(",
        ".blur(",
        ".scroll",
        ".setattribute(",
        ".removeattribute(",
        ".value =",
        ".checked =",
        ".selected =",
        ".innerhtml =",
        ".textcontent =",
        "localstorage.setitem(",
        "sessionstorage.setitem(",
        "window.location",
        "location.href",
        "location =",
        "history.pushstate(",
        "history.replacestate(",
    ]
    .iter()
    .any(|needle| source.contains(needle))
}

#[allow(clippy::too_many_arguments)]
fn build_draft_skill(
    id: String,
    version: u32,
    subgoal_text: String,
    subgoal_signature: SubgoalSignature,
    applicability: ApplicabilityHints,
    action_sketch: Vec<ActionSketchStep>,
    produced_node_ids: &[Uuid],
    provenance_entry: ProvenanceEntry,
    now: chrono::DateTime<Utc>,
) -> Skill {
    let name = humanize_id(&id);
    Skill {
        id,
        version,
        state: SkillState::Draft,
        scope: SkillScope::ProjectLocal,
        name,
        description: String::new(),
        tags: vec![],
        subgoal_text,
        subgoal_signature,
        applicability,
        parameter_schema: vec![],
        action_sketch,
        outputs: vec![],
        outcome_predicate: OutcomePredicate::SubgoalCompleted {
            post_state_world_model_signature: None,
        },
        provenance: vec![provenance_entry],
        stats: SkillStats {
            occurrence_count: 1,
            success_rate: 1.0,
            last_seen_at: Some(now),
            last_invoked_at: None,
        },
        edited_by_user: false,
        created_at: now,
        updated_at: now,
        produced_node_ids: produced_node_ids.to_vec(),
        body: String::new(),
        schema_version: super::SKILL_SCHEMA_VERSION,
        variables: vec![],
        sections: vec![],
        replay: None,
    }
}

fn applicability_hints_from_world_model(
    wm: &WorldModel,
    signature: ApplicabilitySignature,
) -> ApplicabilityHints {
    let apps = wm
        .focused_app
        .as_ref()
        .map(|f| vec![f.value.name.clone()])
        .unwrap_or_default();
    let hosts = wm
        .cdp_page
        .as_ref()
        .and_then(|p| url::Url::parse(&p.value.url).ok())
        .and_then(|u| u.host_str().map(|s| s.to_string()))
        .map(|h| vec![h])
        .unwrap_or_default();
    ApplicabilityHints {
        apps,
        hosts,
        signature,
    }
}

/// Two action sketches are "equivalent" when their structural shape
/// (step ordering + tool names + sub-skill ids + loop sketches) is
/// identical. Argument values that the provenance pass replaced with
/// `{{captured.*}}` references are compared verbatim, but unequal
/// literal values (e.g. different timestamps) signal divergence.
fn sketches_equivalent(a: &[ActionSketchStep], b: &[ActionSketchStep]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b.iter()).all(|(x, y)| match (x, y) {
        (
            ActionSketchStep::ToolCall {
                tool: t1, args: a1, ..
            },
            ActionSketchStep::ToolCall {
                tool: t2, args: a2, ..
            },
        ) => t1 == t2 && a1 == a2,
        (ActionSketchStep::Loop { body: b1, .. }, ActionSketchStep::Loop { body: b2, .. }) => {
            sketches_equivalent(b1, b2)
        }
        _ => false,
    })
}

/// Convert a freeform subgoal text into a stable kebab-case skill id.
/// Slug helper mirrors `store::slugify` to keep filenames + ids
/// aligned. Empty or punctuation-only inputs fall back to a uuid so
/// the id is always non-empty and unique.
pub fn synthesize_skill_id(subgoal_text: &str) -> String {
    let slug = super::store::slugify(subgoal_text);
    if slug.is_empty() {
        format!("skill-{}", Uuid::new_v4())
    } else {
        slug
    }
}

/// Convert a freeform subgoal text plus the context signature that made it
/// applicable into a stable skill id. Including the signature prevents two
/// equal labels in different apps/hosts from racing onto the same `*-v1.md`
/// file.
pub fn synthesize_skill_id_for_signature(
    subgoal_text: &str,
    signature: &SubgoalSignature,
) -> String {
    let slug = super::store::slugify(subgoal_text);
    let suffix = signature.0.chars().take(8).collect::<String>();
    let suffix = if suffix.is_empty() {
        Uuid::new_v4().to_string()
    } else {
        suffix
    };
    if slug.is_empty() {
        format!("skill-{suffix}")
    } else {
        format!("{slug}-{suffix}")
    }
}

fn synthesize_unique_draft_skill_id(
    subgoal_text: &str,
    signature: &SubgoalSignature,
    candidates: &[Arc<Skill>],
    skill_index: &Arc<RwLock<SkillIndex>>,
) -> String {
    let base = synthesize_skill_id_for_signature(subgoal_text, signature);
    let candidate_id_exists = candidates.iter().any(|skill| skill.id == base);
    if !candidate_id_exists && skill_index.read().get(&base, 1).is_none() {
        return base;
    }

    for _ in 0..16 {
        let suffix = Uuid::new_v4().simple().to_string();
        let id = format!("{}-draft-{}", base, &suffix[..8]);
        if skill_index.read().get(&id, 1).is_none() {
            return id;
        }
    }

    format!("{}-draft-{}", base, Uuid::new_v4().simple())
}

fn humanize_id(id: &str) -> String {
    if id.is_empty() {
        return id.to_string();
    }
    let mut out = String::with_capacity(id.len());
    for (i, ch) in id.chars().enumerate() {
        if ch == '-' {
            out.push(' ');
        } else if i == 0 {
            out.push(ch.to_ascii_uppercase());
        } else {
            out.push(ch);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::super::types::*;
    use super::*;
    use crate::agent::episodic::HashedShingleEmbedder;
    use crate::agent::skills::signature::compute_subgoal_signature;
    use crate::agent::step_record::WorldModelSnapshot;
    use crate::agent::task_state::SubgoalId;
    use crate::agent::world_model::WorldModel;

    fn fixture_recorded_step(tool: &str, body: &str) -> RecordedStep {
        fixture_recorded_step_with_args(tool, serde_json::json!({"x": 1}), body)
    }

    fn fixture_recorded_step_with_args(
        tool: &str,
        arguments: serde_json::Value,
        body: &str,
    ) -> RecordedStep {
        let wm = WorldModel::default();
        RecordedStep {
            tool_name: tool.into(),
            arguments,
            result_text: body.into(),
            world_model_pre: WorldModelSnapshot::from_world_model(&wm),
            world_model_post: WorldModelSnapshot::from_world_model(&wm),
        }
    }

    fn fixture_milestone(text: &str) -> Milestone {
        Milestone {
            subgoal_id: SubgoalId::new(),
            text: text.into(),
            summary: "done".into(),
            pushed_at_step: 0,
            completed_at_step: 1,
        }
    }

    fn fixture_index_with_store() -> (
        tempfile::TempDir,
        Arc<RwLock<SkillIndex>>,
        SkillStore,
        SkillContext,
    ) {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().to_path_buf();
        let store = SkillStore::new(dir.clone());
        let embedder = Arc::new(HashedShingleEmbedder::default());
        let index = Arc::new(RwLock::new(SkillIndex::empty(embedder)));
        let ctx = SkillContext {
            enabled: true,
            project_skills_dir: dir,
            global_skills_dir: None,
            project_id: "p".into(),
        };
        (tmp, index, store, ctx)
    }

    #[tokio::test]
    async fn skipped_when_disabled() {
        let (_tmp, idx, store, mut ctx) = fixture_index_with_store();
        ctx.enabled = false;
        let wm = WorldModel::default();
        let m = fixture_milestone("open Telegram");
        let out = maybe_extract_skill(
            &m,
            &[fixture_recorded_step("click", "{}")],
            compute_subgoal_signature(&m.text, &wm),
            &wm,
            &idx,
            &store,
            &ctx,
            Uuid::nil(),
            "wfh",
            1,
            &[],
        )
        .await
        .unwrap();
        assert!(matches!(out, MaybeExtracted::Skipped { .. }));
    }

    #[tokio::test]
    async fn first_invocation_inserts_draft_at_version_1() {
        let (_tmp, idx, store, ctx) = fixture_index_with_store();
        let wm = WorldModel::default();
        let m = fixture_milestone("open vesna chat");
        let out = maybe_extract_skill(
            &m,
            &[fixture_recorded_step("click", "{}")],
            compute_subgoal_signature(&m.text, &wm),
            &wm,
            &idx,
            &store,
            &ctx,
            Uuid::nil(),
            "wfh",
            1,
            &[],
        )
        .await
        .unwrap();
        match out {
            MaybeExtracted::Inserted { version, .. } => assert_eq!(version, 1),
            other => panic!("expected Inserted, got {:?}", other),
        }
        // Index now has one entry, store has one file.
        assert_eq!(idx.read().len(), 1);
    }

    #[tokio::test]
    async fn skips_unverified_side_effectful_script_without_world_model_delta() {
        let (_tmp, idx, store, ctx) = fixture_index_with_store();
        let wm = WorldModel::default();
        let m = fixture_milestone("open target");
        let out = maybe_extract_skill(
            &m,
            &[fixture_recorded_step_with_args(
                "cdp_evaluate_script",
                serde_json::json!({
                    "function": "() => document.querySelector('button')?.click()"
                }),
                "\"clicked\"",
            )],
            compute_subgoal_signature(&m.text, &wm),
            &wm,
            &idx,
            &store,
            &ctx,
            Uuid::nil(),
            "wfh",
            1,
            &[],
        )
        .await
        .unwrap();

        match out {
            MaybeExtracted::Skipped { reason } => {
                assert_eq!(reason, "unverified side-effectful action");
            }
            other => panic!("expected skipped extraction, got {:?}", other),
        }
        assert_eq!(idx.read().len(), 0);
    }

    #[tokio::test]
    async fn second_identical_invocation_merges_with_bump() {
        let (_tmp, idx, store, ctx) = fixture_index_with_store();
        let wm = WorldModel::default();
        let m = fixture_milestone("open vesna chat");
        let action = vec![fixture_recorded_step("click", "{}")];
        maybe_extract_skill(
            &m,
            &action,
            compute_subgoal_signature(&m.text, &wm),
            &wm,
            &idx,
            &store,
            &ctx,
            Uuid::nil(),
            "wfh",
            1,
            &[],
        )
        .await
        .unwrap();
        let out = maybe_extract_skill(
            &m,
            &action,
            compute_subgoal_signature(&m.text, &wm),
            &wm,
            &idx,
            &store,
            &ctx,
            Uuid::nil(),
            "wfh",
            2,
            &[],
        )
        .await
        .unwrap();
        match out {
            MaybeExtracted::Merged {
                occurrence_count, ..
            } => {
                assert_eq!(occurrence_count, 2);
            }
            other => panic!("expected Merged, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn divergent_invocation_inserts_new_version() {
        let (_tmp, idx, store, ctx) = fixture_index_with_store();
        let wm = WorldModel::default();
        let m = fixture_milestone("open vesna chat");
        // First invocation: one click step.
        maybe_extract_skill(
            &m,
            &[fixture_recorded_step("click", "{}")],
            compute_subgoal_signature(&m.text, &wm),
            &wm,
            &idx,
            &store,
            &ctx,
            Uuid::nil(),
            "wfh",
            1,
            &[],
        )
        .await
        .unwrap();
        // Second invocation: divergent (different tool sequence).
        let out = maybe_extract_skill(
            &m,
            &[
                fixture_recorded_step("type_text", "{}"),
                fixture_recorded_step("press_key", "{}"),
            ],
            compute_subgoal_signature(&m.text, &wm),
            &wm,
            &idx,
            &store,
            &ctx,
            Uuid::nil(),
            "wfh",
            2,
            &[],
        )
        .await
        .unwrap();
        match out {
            MaybeExtracted::Inserted { version, .. } => assert_eq!(version, 2),
            other => panic!("expected Inserted v2, got {:?}", other),
        }
        // Two skills now in the index, both under the same id.
        assert_eq!(idx.read().len(), 2);
    }

    #[test]
    fn synthesize_skill_id_falls_back_to_uuid_when_input_unslugifiable() {
        // Pure-punctuation input: slugify returns empty, helper falls
        // back to a uuid-prefixed id so it's never empty.
        let id = synthesize_skill_id("!!!");
        assert!(id.starts_with("skill-"));
    }
}
