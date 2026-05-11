//! Spec 3 Phase 3 integration tests for `maybe_extract_skill`.
//!
//! Drives the extractor through synthetic `RecordedStep` streams and
//! asserts the on-disk store + in-memory index reach the expected
//! shape: single insert, repeat-merge, divergent fork, and cross-step
//! provenance threading.

use std::sync::Arc;

use chrono::Utc;
use clickweave_engine::agent::skills::extractor::maybe_extract_skill;
use clickweave_engine::agent::skills::extractor::synthesize_skill_id_for_signature;
use clickweave_engine::agent::skills::provenance::build_action_sketch;
use clickweave_engine::agent::skills::signature::{
    compute_applicability_signature, compute_subgoal_signature,
};
use clickweave_engine::agent::skills::{
    ActionSketchStep, ApplicabilityHints, OutcomePredicate, RecordedStep, Skill, SkillContext,
    SkillIndex, SkillScope, SkillState, SkillStats, SkillStore, SubgoalSignature,
};
use clickweave_engine::agent::step_record::WorldModelSnapshot;
use clickweave_engine::agent::task_state::{Milestone, SubgoalId};
use clickweave_engine::agent::world_model::{
    AppKind, FocusedApp, Fresh, FreshnessSource, WorldModel,
};
use parking_lot::RwLock;
use tempfile::TempDir;
use uuid::Uuid;

fn step(tool: &str, args: serde_json::Value, result: &str) -> RecordedStep {
    let wm = WorldModel::default();
    RecordedStep {
        tool_name: tool.into(),
        arguments: args,
        result_text: result.into(),
        world_model_pre: WorldModelSnapshot::from_world_model(&wm),
        world_model_post: WorldModelSnapshot::from_world_model(&wm),
    }
}

fn milestone(text: &str) -> Milestone {
    Milestone {
        subgoal_id: SubgoalId::new(),
        text: text.into(),
        summary: "ok".into(),
        pushed_at_step: 0,
        completed_at_step: 1,
    }
}

fn world_model_with_app(name: &str) -> WorldModel {
    let mut wm = WorldModel::default();
    wm.focused_app = Some(Fresh {
        value: FocusedApp {
            name: name.to_string(),
            kind: AppKind::Native,
            pid: 1,
        },
        written_at: 0,
        source: FreshnessSource::DirectObservation,
        ttl_steps: None,
    });
    wm
}

fn fixture(
    enabled: bool,
) -> (
    TempDir,
    SkillStore,
    Arc<RwLock<SkillIndex>>,
    SkillContext,
    WorldModel,
) {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path().to_path_buf();
    let store = SkillStore::new(dir.clone());
    let embedder = Arc::new(clickweave_engine::agent::episodic::HashedShingleEmbedder::default());
    let index = Arc::new(RwLock::new(SkillIndex::empty(embedder)));
    let ctx = SkillContext {
        enabled,
        project_skills_dir: dir,
        global_skills_dir: None,
        project_id: "p".into(),
    };
    (tmp, store, index, ctx, WorldModel::default())
}

fn indexed_skill(
    id: String,
    version: u32,
    state: SkillState,
    scope: SkillScope,
    subgoal_text: &str,
    subgoal_signature: SubgoalSignature,
    post_state_world_model: &WorldModel,
    actions: &[RecordedStep],
) -> Skill {
    let now = Utc::now();
    Skill {
        id,
        version,
        state,
        scope,
        name: "seed skill".into(),
        description: String::new(),
        tags: vec![],
        subgoal_text: subgoal_text.into(),
        subgoal_signature,
        applicability: ApplicabilityHints {
            apps: vec![],
            hosts: vec![],
            signature: compute_applicability_signature(post_state_world_model),
        },
        parameter_schema: vec![],
        action_sketch: build_action_sketch(actions),
        outputs: vec![],
        outcome_predicate: OutcomePredicate::SubgoalCompleted {
            post_state_world_model_signature: None,
        },
        provenance: vec![],
        stats: SkillStats {
            occurrence_count: 1,
            success_rate: 1.0,
            last_seen_at: Some(now),
            last_invoked_at: None,
        },
        edited_by_user: false,
        created_at: now,
        updated_at: now,
        produced_node_ids: vec![],
        body: String::new(),
        schema_version: clickweave_engine::agent::skills::SKILL_SCHEMA_VERSION,
        variables: vec![],
        sections: vec![],
        replay: None,
    }
}

#[tokio::test]
async fn single_subgoal_three_steps_writes_one_draft() {
    let (_tmp, store, index, ctx, wm) = fixture(true);
    let m = milestone("open vesna chat");
    let actions = vec![
        step("click", serde_json::json!({"x": 1}), r#"{"ok":1}"#),
        step("type_text", serde_json::json!({"text": "hi"}), r#"{}"#),
        step("press_key", serde_json::json!({"key": "Enter"}), r#"{}"#),
    ];

    let out = maybe_extract_skill(
        &m,
        &actions,
        compute_subgoal_signature(&m.text, &wm),
        &wm,
        &index,
        &store,
        &ctx,
        Uuid::nil(),
        "wf-1",
        3,
        &[],
    )
    .await
    .unwrap();

    use clickweave_engine::agent::skills::MaybeExtracted;
    matches!(out, MaybeExtracted::Inserted { .. });
    assert_eq!(index.read().len(), 1);
    let drafts = index
        .read()
        .skills_in_state(clickweave_engine::agent::skills::SkillState::Draft);
    assert_eq!(drafts.len(), 1);
    assert_eq!(drafts[0].stats.occurrence_count, 1);
}

#[tokio::test]
async fn second_identical_invocation_merges_with_occurrence_bump() {
    let (_tmp, store, index, ctx, wm) = fixture(true);
    let m = milestone("open vesna chat");
    let actions = vec![step("click", serde_json::json!({"x": 1}), r#"{}"#)];

    maybe_extract_skill(
        &m,
        &actions,
        compute_subgoal_signature(&m.text, &wm),
        &wm,
        &index,
        &store,
        &ctx,
        Uuid::nil(),
        "wf-1",
        1,
        &[],
    )
    .await
    .unwrap();

    let out = maybe_extract_skill(
        &m,
        &actions,
        compute_subgoal_signature(&m.text, &wm),
        &wm,
        &index,
        &store,
        &ctx,
        Uuid::nil(),
        "wf-1",
        2,
        &[],
    )
    .await
    .unwrap();

    use clickweave_engine::agent::skills::MaybeExtracted;
    match out {
        MaybeExtracted::Merged {
            occurrence_count, ..
        } => assert_eq!(occurrence_count, 2),
        other => panic!("expected Merged, got {:?}", other),
    }
}

#[tokio::test]
async fn third_identical_invocation_merges_from_latest_draft_version() {
    let (_tmp, store, index, ctx, wm) = fixture(true);
    let m = milestone("open vesna chat");
    let actions = vec![step("click", serde_json::json!({"x": 1}), r#"{}"#)];

    let mut last = None;
    for step_index in 1..=3 {
        last = Some(
            maybe_extract_skill(
                &m,
                &actions,
                compute_subgoal_signature(&m.text, &wm),
                &wm,
                &index,
                &store,
                &ctx,
                Uuid::nil(),
                "wf-1",
                step_index,
                &[],
            )
            .await
            .unwrap(),
        );
    }

    use clickweave_engine::agent::skills::MaybeExtracted;
    match last.expect("third extraction result") {
        MaybeExtracted::Merged {
            version,
            occurrence_count,
            ..
        } => {
            assert_eq!(version, 3);
            assert_eq!(occurrence_count, 3);
        }
        other => panic!(
            "expected third extraction to merge into v3, got {:?}",
            other
        ),
    }

    let latest = index
        .read()
        .skills_in_state(clickweave_engine::agent::skills::SkillState::Draft)
        .into_iter()
        .max_by_key(|skill| skill.version)
        .expect("latest draft");
    assert_eq!(latest.version, 3);
    assert_eq!(latest.stats.occurrence_count, 3);
    // Per-skill directory layout: all versions of a skill share one
    // `<skill_id>/SKILL.md` file. Three merges overwrite the same file.
    assert_eq!(store.list_files().unwrap().len(), 1);
}

#[tokio::test]
async fn divergent_invocation_inserts_new_version_in_same_family() {
    let (_tmp, store, index, ctx, wm) = fixture(true);
    let m = milestone("open vesna chat");

    maybe_extract_skill(
        &m,
        &[step("click", serde_json::json!({"x": 1}), r#"{}"#)],
        compute_subgoal_signature(&m.text, &wm),
        &wm,
        &index,
        &store,
        &ctx,
        Uuid::nil(),
        "wf-1",
        1,
        &[],
    )
    .await
    .unwrap();
    let out = maybe_extract_skill(
        &m,
        &[
            step("type_text", serde_json::json!({"text": "x"}), r#"{}"#),
            step("press_key", serde_json::json!({"key": "Enter"}), r#"{}"#),
        ],
        compute_subgoal_signature(&m.text, &wm),
        &wm,
        &index,
        &store,
        &ctx,
        Uuid::nil(),
        "wf-1",
        2,
        &[],
    )
    .await
    .unwrap();

    use clickweave_engine::agent::skills::MaybeExtracted;
    match out {
        MaybeExtracted::Inserted { version, skill_id } => {
            assert_eq!(version, 2);
            // The id must match the v1 skill's id (same signature
            // family, divergent sketch produces v + 1, not a new id).
            let drafts = index
                .read()
                .skills_in_state(clickweave_engine::agent::skills::SkillState::Draft);
            let ids: Vec<_> = drafts.iter().map(|s: &Arc<Skill>| s.id.clone()).collect();
            assert!(ids.iter().all(|id| id == &skill_id));
            assert_eq!(drafts.len(), 2);
        }
        other => panic!("expected Inserted v2, got {:?}", other),
    }
}

#[tokio::test]
async fn matching_confirmed_skill_starts_project_draft_instead_of_merging() {
    let (_tmp, store, index, ctx, wm) = fixture(true);
    let m = milestone("open vesna chat");
    let actions = vec![step("click", serde_json::json!({"x": 1}), r#"{}"#)];
    let signature = compute_subgoal_signature(&m.text, &wm);
    let existing_id = synthesize_skill_id_for_signature(&m.text, &signature);
    let existing = indexed_skill(
        existing_id.clone(),
        1,
        SkillState::Confirmed,
        SkillScope::ProjectLocal,
        &m.text,
        signature.clone(),
        &wm,
        &actions,
    );
    index.write().upsert(existing);

    let out = maybe_extract_skill(
        &m,
        &actions,
        signature,
        &wm,
        &index,
        &store,
        &ctx,
        Uuid::nil(),
        "wf-1",
        2,
        &[],
    )
    .await
    .unwrap();

    use clickweave_engine::agent::skills::MaybeExtracted;
    match out {
        MaybeExtracted::Inserted { skill_id, version } => {
            assert_eq!(version, 1);
            assert_ne!(skill_id, existing_id);
        }
        other => panic!("expected fresh draft insert, got {:?}", other),
    }

    assert_eq!(
        index
            .read()
            .skills_in_state(clickweave_engine::agent::skills::SkillState::Confirmed)
            .len(),
        1,
        "confirmed source skill must remain unchanged",
    );
    let drafts = index
        .read()
        .skills_in_state(clickweave_engine::agent::skills::SkillState::Draft);
    assert_eq!(drafts.len(), 1);
    assert_eq!(drafts[0].scope, SkillScope::ProjectLocal);
    assert_eq!(drafts[0].version, 1);
    assert_ne!(drafts[0].id, existing_id);
    assert_eq!(store.list_files().unwrap().len(), 1);
}

#[tokio::test]
async fn matching_user_edited_draft_starts_fresh_draft_instead_of_versioning() {
    let (_tmp, store, index, ctx, wm) = fixture(true);
    let m = milestone("open vesna chat");
    let actions = vec![step("click", serde_json::json!({"x": 1}), r#"{}"#)];
    let signature = compute_subgoal_signature(&m.text, &wm);
    let existing_id = synthesize_skill_id_for_signature(&m.text, &signature);
    let mut existing = indexed_skill(
        existing_id.clone(),
        1,
        SkillState::Draft,
        SkillScope::ProjectLocal,
        &m.text,
        signature.clone(),
        &wm,
        &actions,
    );
    existing.edited_by_user = true;
    index.write().upsert(existing);

    let out = maybe_extract_skill(
        &m,
        &actions,
        signature,
        &wm,
        &index,
        &store,
        &ctx,
        Uuid::nil(),
        "wf-1",
        2,
        &[],
    )
    .await
    .unwrap();

    use clickweave_engine::agent::skills::MaybeExtracted;
    match out {
        MaybeExtracted::Inserted { skill_id, version } => {
            assert_eq!(version, 1);
            assert_ne!(skill_id, existing_id);
        }
        other => panic!("expected fresh draft insert, got {:?}", other),
    }

    let drafts = index
        .read()
        .skills_in_state(clickweave_engine::agent::skills::SkillState::Draft);
    assert_eq!(drafts.len(), 2);
    let edited = drafts
        .iter()
        .find(|skill| skill.id == existing_id)
        .expect("edited draft remains indexed");
    assert!(edited.edited_by_user);
    assert!(
        drafts
            .iter()
            .any(|skill| skill.id != existing_id && !skill.edited_by_user && skill.version == 1),
        "new extraction should fork into a fresh unedited draft",
    );
    assert_eq!(store.list_files().unwrap().len(), 1);
}

#[tokio::test]
async fn divergent_global_skill_starts_project_draft_family() {
    let (_tmp, store, index, ctx, wm) = fixture(true);
    let m = milestone("open vesna chat");
    let existing_actions = vec![step("click", serde_json::json!({"x": 1}), r#"{}"#)];
    let new_actions = vec![step("click", serde_json::json!({"x": 2}), r#"{}"#)];
    let signature = compute_subgoal_signature(&m.text, &wm);
    let existing_id = synthesize_skill_id_for_signature(&m.text, &signature);
    let existing = indexed_skill(
        existing_id.clone(),
        1,
        SkillState::Promoted,
        SkillScope::Global,
        &m.text,
        signature.clone(),
        &wm,
        &existing_actions,
    );
    index.write().upsert(existing);

    let out = maybe_extract_skill(
        &m,
        &new_actions,
        signature,
        &wm,
        &index,
        &store,
        &ctx,
        Uuid::nil(),
        "wf-1",
        2,
        &[],
    )
    .await
    .unwrap();

    use clickweave_engine::agent::skills::MaybeExtracted;
    match out {
        MaybeExtracted::Inserted { skill_id, version } => {
            assert_eq!(version, 1);
            assert_ne!(skill_id, existing_id);
        }
        other => panic!("expected fresh draft insert, got {:?}", other),
    }

    assert_eq!(
        index
            .read()
            .skills_in_state(clickweave_engine::agent::skills::SkillState::Promoted)
            .len(),
        1,
        "global promoted source skill must remain unchanged",
    );
    let drafts = index
        .read()
        .skills_in_state(clickweave_engine::agent::skills::SkillState::Draft);
    assert_eq!(drafts.len(), 1);
    assert_eq!(drafts[0].scope, SkillScope::ProjectLocal);
    assert_eq!(drafts[0].version, 1);
    assert_ne!(drafts[0].id, existing_id);
    assert_eq!(store.list_files().unwrap().len(), 1);
}

#[tokio::test]
async fn same_subgoal_text_in_different_contexts_does_not_overwrite_file() {
    let (_tmp, store, index, ctx, _wm) = fixture(true);
    let m = milestone("open inbox");
    let finder = world_model_with_app("Finder");
    let mail = world_model_with_app("Mail");
    let action = vec![step("click", serde_json::json!({"x": 1}), r#"{}"#)];

    maybe_extract_skill(
        &m,
        &action,
        compute_subgoal_signature(&m.text, &finder),
        &finder,
        &index,
        &store,
        &ctx,
        Uuid::nil(),
        "wf-1",
        1,
        &[],
    )
    .await
    .unwrap();
    maybe_extract_skill(
        &m,
        &action,
        compute_subgoal_signature(&m.text, &mail),
        &mail,
        &index,
        &store,
        &ctx,
        Uuid::nil(),
        "wf-1",
        2,
        &[],
    )
    .await
    .unwrap();

    let drafts = index
        .read()
        .skills_in_state(clickweave_engine::agent::skills::SkillState::Draft);
    let ids = drafts.iter().map(|s| s.id.clone()).collect::<Vec<_>>();
    assert_eq!(drafts.len(), 2);
    assert_ne!(ids[0], ids[1]);
    assert_eq!(store.list_files().unwrap().len(), 2);
}

#[tokio::test]
async fn cross_step_provenance_threads_captured_reference() {
    // Step 0 produces a result containing the literal "Vesna Petrovich".
    // Step 1 then types that exact literal — the action sketch should
    // route it as `{{captured.*}}` rather than baking the literal in.
    let (_tmp, store, index, ctx, wm) = fixture(true);
    let m = milestone("open chat");
    let actions = vec![
        step(
            "ax_select",
            serde_json::json!({"role": "row"}),
            r#"{"selected_name": "Vesna Petrovich"}"#,
        ),
        step(
            "type_text",
            serde_json::json!({"text": "Vesna Petrovich"}),
            r#"{}"#,
        ),
    ];
    maybe_extract_skill(
        &m,
        &actions,
        compute_subgoal_signature(&m.text, &wm),
        &wm,
        &index,
        &store,
        &ctx,
        Uuid::nil(),
        "wf-1",
        2,
        &[],
    )
    .await
    .unwrap();

    let drafts = index
        .read()
        .skills_in_state(clickweave_engine::agent::skills::SkillState::Draft);
    let skill = drafts.first().expect("draft skill present").clone();
    let step1_args = match &skill.action_sketch[1] {
        ActionSketchStep::ToolCall { args, .. } => args.clone(),
        other => panic!("expected ToolCall, got {:?}", other),
    };
    let text = step1_args
        .get("text")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    assert!(
        text.contains("{{captured."),
        "cross-step literal should rewrite to a captured reference; got: {text}",
    );
}

#[tokio::test]
async fn extraction_skipped_when_disabled() {
    let (_tmp, store, index, ctx, wm) = fixture(false);
    let m = milestone("any subgoal");
    let out = maybe_extract_skill(
        &m,
        &[step("click", serde_json::json!({"x": 1}), r#"{}"#)],
        compute_subgoal_signature(&m.text, &wm),
        &wm,
        &index,
        &store,
        &ctx,
        Uuid::nil(),
        "wf",
        1,
        &[],
    )
    .await
    .unwrap();
    use clickweave_engine::agent::skills::MaybeExtracted;
    match out {
        MaybeExtracted::Skipped { reason } => assert_eq!(reason, "disabled"),
        other => panic!("expected Skipped, got {:?}", other),
    }
    assert_eq!(index.read().len(), 0);
}
