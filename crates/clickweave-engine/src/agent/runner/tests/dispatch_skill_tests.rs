//! Phase 4 lookup-and-validate coverage for `StateRunner::dispatch_skill`.
//! The per-step expansion (Task 4.3+) is deferred; these tests pin
//! the foundation so the resume seam stays stable.

use super::*;
use crate::agent::skills::types::{
    ApplicabilityHints, ApplicabilitySignature, ExpectedWorldModelDelta, OutcomePredicate,
    ParameterSlot, ProvenanceEntry, Skill, SkillState, SkillStats, SubgoalSignature,
};
use crate::agent::skills::{ActionSketchStep, SkillIndex, SkillScope};
use chrono::Utc;
use std::sync::Arc;
use tempfile::TempDir;
use tokio::sync::mpsc;

fn make_skill(id: &str, version: u32, state: SkillState, schema: Vec<ParameterSlot>) -> Skill {
    let now = Utc::now();
    Skill {
        id: id.to_string(),
        version,
        state,
        scope: SkillScope::ProjectLocal,
        name: format!("Skill {id}"),
        description: "test skill".to_string(),
        tags: vec![],
        subgoal_text: "open the file".to_string(),
        subgoal_signature: SubgoalSignature("sg".to_string()),
        applicability: ApplicabilityHints {
            apps: vec![],
            hosts: vec![],
            signature: ApplicabilitySignature("app".to_string()),
        },
        parameter_schema: schema,
        action_sketch: vec![ActionSketchStep::ToolCall {
            step_id: "s_test_noop".to_string(),
            tool: "noop".to_string(),
            args: serde_json::json!({}),
            captures_pre: vec![],
            captures: vec![],
            expected_world_model_delta: ExpectedWorldModelDelta::default(),
        }],
        outputs: vec![],
        outcome_predicate: OutcomePredicate::SubgoalCompleted {
            post_state_world_model_signature: None,
        },
        provenance: vec![ProvenanceEntry {
            run_id: uuid::Uuid::new_v4().to_string(),
            step_index: 0,
            completed_at: now,
            workflow_hash: "h".to_string(),
        }],
        stats: SkillStats {
            occurrence_count: 1,
            success_rate: 0.5,
            last_seen_at: Some(now),
            last_invoked_at: None,
        },
        edited_by_user: false,
        created_at: now,
        updated_at: now,
        produced_node_ids: vec![],
        body: "# Test\n".to_string(),
        schema_version: crate::agent::skills::SKILL_SCHEMA_VERSION,
        variables: vec![],
        sections: vec![],
        replay: None,
    }
}

fn tool_step(tool: &str) -> ActionSketchStep {
    ActionSketchStep::ToolCall {
        step_id: format!("s_test_{tool}"),
        tool: tool.to_string(),
        args: serde_json::json!({}),
        captures_pre: vec![],
        captures: vec![],
        expected_world_model_delta: ExpectedWorldModelDelta::default(),
    }
}

fn slot(name: &str, type_tag: &str, default: Option<serde_json::Value>) -> ParameterSlot {
    ParameterSlot {
        name: name.to_string(),
        type_tag: type_tag.to_string(),
        description: None,
        default,
        enum_values: None,
    }
}

fn fresh_runner_with_skill(
    skill: Option<Skill>,
) -> (StateRunner, mpsc::Receiver<RunnerOutput>, TempDir) {
    let tmp = TempDir::new().unwrap();
    let mut runner =
        StateRunner::new_for_test_with_skills("test goal".to_string(), tmp.path().to_path_buf());
    let embedder = Arc::new(crate::agent::episodic::HashedShingleEmbedder::default());
    let mut index = SkillIndex::empty(embedder);
    if let Some(s) = skill {
        index.upsert(s);
    }
    runner.skill_index = Arc::new(parking_lot::RwLock::new(index));
    let (tx, rx) = mpsc::channel(16);
    runner.event_tx = Some(tx);
    (runner, rx, tmp)
}

#[test]
fn single_step_bridge_rejects_multi_step_skill_before_partial_dispatch() {
    let mut skill = make_skill("multi", 1, SkillState::Confirmed, vec![]);
    skill.action_sketch = vec![tool_step("first"), tool_step("second")];
    let frame = SkillFrame::new(Arc::new(skill), serde_json::json!({}));

    match StateRunner::skill_frame_to_single_step_action(&frame) {
        AgentAction::AgentReplan { reason } => {
            assert!(
                reason.contains("2 replay steps"),
                "reason should explain unsupported multi-step replay: {reason}"
            );
        }
        other => panic!("expected fail-closed replan, got {:?}", other),
    }
}

#[test]
fn single_step_bridge_dispatches_exactly_one_tool_step() {
    let skill = make_skill("single", 3, SkillState::Confirmed, vec![]);
    let frame = SkillFrame::new(Arc::new(skill), serde_json::json!({}));

    match StateRunner::skill_frame_to_single_step_action(&frame) {
        AgentAction::ToolCall {
            tool_name,
            tool_call_id,
            ..
        } => {
            assert_eq!(tool_name, "noop");
            assert_eq!(tool_call_id, "skill-single-v3-step-0");
        }
        other => panic!("expected single-step tool call, got {:?}", other),
    }
}

#[tokio::test]
async fn unknown_id_yields_replan_naming_the_id() {
    let (mut runner, _rx, _tmp) = fresh_runner_with_skill(None);
    let err = runner
        .dispatch_skill("never_extracted", 1, serde_json::json!({}))
        .await
        .expect_err("missing skill must fail");
    assert!(err.contains("never_extracted"), "reason: {err}");
}

#[tokio::test]
async fn draft_state_is_rejected() {
    let skill = make_skill("draftish", 1, SkillState::Draft, vec![]);
    let (mut runner, _rx, _tmp) = fresh_runner_with_skill(Some(skill));
    let err = runner
        .dispatch_skill("draftish", 1, serde_json::json!({}))
        .await
        .expect_err("draft must not invoke");
    assert!(err.contains("draft"), "reason: {err}");
}

#[tokio::test]
async fn invalid_parameters_yield_replan() {
    let skill = make_skill(
        "needs_count",
        1,
        SkillState::Confirmed,
        vec![slot("count", "integer", None)],
    );
    let (mut runner, _rx, _tmp) = fresh_runner_with_skill(Some(skill));
    let err = runner
        .dispatch_skill("needs_count", 1, serde_json::json!({}))
        .await
        .expect_err("missing required field must fail");
    assert!(err.contains("count"), "reason: {err}");
}

#[tokio::test]
async fn confirmed_emits_invoked_event_and_marks_invoked() {
    let skill = make_skill(
        "confirm_ok",
        2,
        SkillState::Confirmed,
        vec![slot("name", "string", None)],
    );
    let (mut runner, mut rx, _tmp) = fresh_runner_with_skill(Some(skill));
    let frame = runner
        .dispatch_skill("confirm_ok", 2, serde_json::json!({"name": "x"}))
        .await
        .expect("confirmed skill should resolve");
    assert_eq!(frame.skill.id, "confirm_ok");
    assert_eq!(frame.skill.version, 2);
    assert_eq!(frame.next_step, 0);

    let stamped = runner
        .skill_index
        .read()
        .get("confirm_ok", 2)
        .unwrap()
        .stats
        .last_invoked_at;
    assert!(stamped.is_some());

    let event = rx
        .try_recv()
        .expect("SkillInvoked must be emitted")
        .into_event()
        .expect("SkillInvoked must be a durable event");
    match event {
        AgentEvent::SkillInvoked {
            skill_id,
            version,
            parameter_count,
            ..
        } => {
            assert_eq!(skill_id, "confirm_ok");
            assert_eq!(version, 2);
            assert_eq!(parameter_count, 1);
        }
        other => panic!("expected SkillInvoked, got {:?}", other),
    }
}
