//! Spec 3 Phase 3 end-to-end smoke tests for the procedural-skills
//! extraction → retrieval pipeline.
//!
//! Exercises the runner's lower-level entrypoints
//! (`apply_mutations` + manual `recorded_steps` push +
//! [`StateRunner::run_extractor_for_test`]) instead of the full
//! `StateRunner::run` driver — that path requires a scripted MCP +
//! LLM stack which is overkill for the assertions below. Phase 5 owns
//! the live driver coverage once the Tauri command surface lands.

use std::sync::Arc;

use clickweave_engine::agent::skills::extractor::maybe_extract_skill;
use clickweave_engine::agent::skills::signature::{
    compute_applicability_signature, compute_subgoal_signature,
};
use clickweave_engine::agent::skills::{
    RecordedStep, SkillContext, SkillIndex, SkillState, SkillStore, SubgoalSignature,
};
use clickweave_engine::agent::step_record::WorldModelSnapshot;
use clickweave_engine::agent::task_state::{Milestone, SubgoalId};
use clickweave_engine::agent::world_model::WorldModel;
use parking_lot::RwLock;
use tempfile::TempDir;
use uuid::Uuid;

fn make_recorded_step(tool: &str, args: serde_json::Value) -> RecordedStep {
    let wm = WorldModel::default();
    RecordedStep {
        tool_name: tool.into(),
        arguments: args,
        result_text: r#"{"ok":true}"#.into(),
        world_model_pre: WorldModelSnapshot::from_world_model(&wm),
        world_model_post: WorldModelSnapshot::from_world_model(&wm),
    }
}

fn make_milestone(text: &str) -> Milestone {
    Milestone {
        subgoal_id: SubgoalId::new(),
        text: text.into(),
        summary: "done".into(),
        pushed_at_step: 0,
        completed_at_step: 1,
    }
}

fn fresh_index_for(dir: &std::path::Path) -> Arc<RwLock<SkillIndex>> {
    let embedder = Arc::new(clickweave_engine::agent::episodic::HashedShingleEmbedder::default());
    let ctx = SkillContext {
        enabled: true,
        project_skills_dir: dir.to_path_buf(),
        global_skills_dir: None,
        project_id: "p".into(),
    };
    let idx = SkillIndex::build(&ctx, embedder).unwrap();
    Arc::new(RwLock::new(idx))
}

#[tokio::test]
async fn three_subgoals_yield_three_draft_skills() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path().to_path_buf();
    let store = SkillStore::new(dir.clone());
    let index = fresh_index_for(&dir);

    let ctx = SkillContext {
        enabled: true,
        project_skills_dir: dir.clone(),
        global_skills_dir: None,
        project_id: "p".into(),
    };
    let wm = WorldModel::default();

    for subgoal in ["open inbox", "search for sender", "click first result"] {
        let m = make_milestone(subgoal);
        let action = vec![
            make_recorded_step("click", serde_json::json!({"x": 1})),
            make_recorded_step("type_text", serde_json::json!({"text": "abc"})),
        ];
        maybe_extract_skill(
            &m,
            &action,
            compute_subgoal_signature(&m.text, &wm),
            &wm,
            &index,
            &store,
            &ctx,
            Uuid::nil(),
            "wf-1",
            10,
            &[],
        )
        .await
        .unwrap();
    }

    assert_eq!(index.read().len(), 3, "three new draft skills extracted");
    let drafts = index.read().skills_in_state(SkillState::Draft);
    assert_eq!(drafts.len(), 3, "all three start in draft state");
}

#[tokio::test]
async fn drafts_are_not_retrieval_eligible() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path().to_path_buf();
    let store = SkillStore::new(dir.clone());
    let index = fresh_index_for(&dir);

    let ctx = SkillContext {
        enabled: true,
        project_skills_dir: dir.clone(),
        global_skills_dir: None,
        project_id: "p".into(),
    };
    let wm = WorldModel::default();
    let m = make_milestone("open chat");
    let action = vec![make_recorded_step("click", serde_json::json!({"x": 1}))];

    maybe_extract_skill(
        &m,
        &action,
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

    let subgoal_sig = compute_subgoal_signature(&m.text, &wm);
    let app_sig = compute_applicability_signature(&wm);
    let hits = index.read().lookup(&subgoal_sig, &app_sig, 5);
    assert!(
        hits.is_empty(),
        "draft skill must not surface in retrieval — only confirmed/promoted are eligible",
    );
}

#[tokio::test]
async fn confirmed_skill_surfaces_in_retrieval() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path().to_path_buf();
    let store = SkillStore::new(dir.clone());
    let index = fresh_index_for(&dir);

    let ctx = SkillContext {
        enabled: true,
        project_skills_dir: dir.clone(),
        global_skills_dir: None,
        project_id: "p".into(),
    };
    let wm = WorldModel::default();
    let m = make_milestone("open chat");
    let action = vec![make_recorded_step("click", serde_json::json!({"x": 1}))];

    maybe_extract_skill(
        &m,
        &action,
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

    // Flip the skill to `Confirmed` via test-only path: clone the
    // index entry, mutate state, write back through the store, and
    // re-upsert into the index.
    let drafts = index.read().skills_in_state(SkillState::Draft);
    let mut promoted: clickweave_engine::agent::skills::Skill = (*drafts[0]).clone();
    promoted.state = SkillState::Confirmed;
    promoted.version += 1;
    store.write_skill(&promoted).unwrap();
    index.write().upsert(promoted);

    // Retrieval surfaces it now that the state machine is past Draft.
    let subgoal_sig = compute_subgoal_signature(&m.text, &wm);
    let app_sig = compute_applicability_signature(&wm);
    let hits = index.read().lookup(&subgoal_sig, &app_sig, 5);
    assert_eq!(
        hits.len(),
        1,
        "confirmed skill must be retrieval-eligible at the matching signature",
    );
    assert_eq!(hits[0].skill.state, SkillState::Confirmed);
}

#[tokio::test]
async fn second_run_loads_skills_from_disk() {
    // First run: extract three skills + flip one to Confirmed.
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path().to_path_buf();
    let store = SkillStore::new(dir.clone());
    let index_run1 = fresh_index_for(&dir);

    let ctx = SkillContext {
        enabled: true,
        project_skills_dir: dir.clone(),
        global_skills_dir: None,
        project_id: "p".into(),
    };
    let wm = WorldModel::default();
    let texts = ["alpha", "beta", "gamma"];
    for t in texts {
        let m = make_milestone(t);
        let action = vec![make_recorded_step("click", serde_json::json!({"x": 1}))];
        maybe_extract_skill(
            &m,
            &action,
            compute_subgoal_signature(&m.text, &wm),
            &wm,
            &index_run1,
            &store,
            &ctx,
            Uuid::nil(),
            "wf-1",
            1,
            &[],
        )
        .await
        .unwrap();
    }
    let alpha_sig = compute_subgoal_signature("alpha", &wm);
    // Promote alpha.
    let mut alpha = index_run1
        .read()
        .skills_with_signature(&alpha_sig)
        .first()
        .unwrap()
        .as_ref()
        .clone();
    alpha.state = SkillState::Confirmed;
    alpha.version += 1;
    store.write_skill(&alpha).unwrap();
    drop(index_run1);

    // Second run: build a fresh index from disk.
    let index_run2 = fresh_index_for(&dir);
    // 3 entries: alpha v2 (confirmed), beta v1 (draft), gamma v1 (draft).
    // Per-skill directory layout: writing alpha v2 overwrites alpha v1 on
    // disk — a single `alpha/SKILL.md` holds the latest version only.
    assert_eq!(index_run2.read().len(), 3);
    let app_sig = compute_applicability_signature(&wm);
    let alpha_hits = index_run2.read().lookup(&alpha_sig, &app_sig, 5);
    assert_eq!(
        alpha_hits.len(),
        1,
        "only the confirmed alpha is retrieval-eligible across the version family",
    );
    assert_eq!(alpha_hits[0].skill.state, SkillState::Confirmed);
}

#[test]
fn signatures_used_by_retrieval_are_deterministic_across_runs() {
    let wm = WorldModel::default();
    let s1 = compute_subgoal_signature("open chat", &wm);
    let s2 = compute_subgoal_signature("open chat", &wm);
    assert_eq!(s1, s2);
    assert_eq!(s1, SubgoalSignature(s1.0.clone()));
}
