//! Walkthrough → draft skill conversion.
//!
//! Walkthrough drafts already have a semantic action stream and synthesized
//! workflow graph. The conversion keeps the skill as a leaf skill by mapping
//! the synthesized workflow nodes back to MCP tool calls, preserving the
//! existing target-resolution choices from walkthrough synthesis.

use chrono::Utc;
use clickweave_core::tool_mapping::{ToolMappingError, node_type_to_tool_invocation};
use clickweave_core::walkthrough::WalkthroughAction;
use clickweave_core::{Workflow, walkthrough};
use uuid::Uuid;

use super::extractor::synthesize_skill_id_for_signature;
use super::signature::{
    compute_applicability_signature_from_parts, compute_subgoal_signature_from_parts,
};
use super::types::{
    ActionSketchStep, ApplicabilityHints, ExpectedWorldModelDelta, OutcomePredicate,
    ProvenanceEntry, Skill, SkillError, SkillScope, SkillState, SkillStats,
};

pub fn walkthrough_to_skill(
    actions: &[WalkthroughAction],
    draft: Option<&Workflow>,
    session_id: &str,
    project_id: &str,
) -> Result<Skill, SkillError> {
    if actions.iter().all(|action| action.candidate) {
        return Err(SkillError::InvalidParameters(
            "walkthrough has no confirmed actions".to_string(),
        ));
    }

    let synthesized;
    let workflow = match draft {
        Some(draft) => draft,
        None => {
            let workflow_id = Uuid::parse_str(project_id).unwrap_or_else(|_| Uuid::new_v4());
            synthesized = walkthrough::synthesize_draft(actions, workflow_id, "Walkthrough Skill");
            &synthesized
        }
    };
    if workflow.nodes.is_empty() {
        return Err(SkillError::InvalidParameters(
            "walkthrough draft has no workflow nodes".to_string(),
        ));
    }

    let action_sketch = workflow
        .nodes
        .iter()
        .enumerate()
        .map(|(idx, node)| {
            let invocation =
                node_type_to_tool_invocation(&node.node_type).map_err(map_tool_mapping_error)?;
            Ok(ActionSketchStep::ToolCall {
                step_id: format!("s_{:06}", idx),
                tool: invocation.name,
                args: invocation.arguments,
                captures_pre: vec![],
                captures: vec![],
                expected_world_model_delta: ExpectedWorldModelDelta::default(),
            })
        })
        .collect::<Result<Vec<_>, SkillError>>()?;

    let title = if workflow.name.trim().is_empty() {
        "Walkthrough Skill"
    } else {
        workflow.name.trim()
    };
    let now = Utc::now();
    let apps = action_apps(actions);
    let focused_app = first_action_app(actions).unwrap_or_default();
    let subgoal_signature = compute_subgoal_signature_from_parts(title, &focused_app, "");
    let id = synthesize_skill_id_for_signature(title, &subgoal_signature);
    let applicability = ApplicabilityHints {
        apps,
        hosts: vec![],
        signature: compute_applicability_signature_from_parts(&focused_app, ""),
    };

    Ok(Skill {
        id,
        version: 1,
        state: SkillState::Draft,
        scope: SkillScope::ProjectLocal,
        name: title.to_string(),
        description: format!("Imported from walkthrough session {session_id}."),
        tags: vec!["walkthrough".to_string()],
        subgoal_text: title.to_string(),
        subgoal_signature,
        applicability,
        parameter_schema: vec![],
        action_sketch,
        outputs: vec![],
        outcome_predicate: OutcomePredicate::SubgoalCompleted {
            post_state_world_model_signature: None,
        },
        provenance: vec![ProvenanceEntry {
            run_id: format!("walkthrough:{session_id}"),
            step_index: 0,
            completed_at: now,
            workflow_hash: project_id.to_string(),
        }],
        stats: SkillStats {
            occurrence_count: 1,
            success_rate: 1.0,
            last_seen_at: Some(now),
            last_invoked_at: None,
        },
        edited_by_user: false,
        created_at: now,
        updated_at: now,
        produced_node_ids: workflow.nodes.iter().map(|node| node.id).collect(),
        body: String::new(),
        schema_version: super::SKILL_SCHEMA_VERSION,
        variables: vec![],
        sections: vec![],
        replay: None,
    })
}

fn action_apps(actions: &[WalkthroughAction]) -> Vec<String> {
    let mut apps = actions
        .iter()
        .filter(|action| !action.candidate)
        .filter_map(|action| action.app_name.as_deref())
        .filter(|name| !name.trim().is_empty())
        .map(|name| name.trim().to_string())
        .collect::<Vec<_>>();
    apps.sort();
    apps.dedup();
    apps
}

fn first_action_app(actions: &[WalkthroughAction]) -> Option<String> {
    actions
        .iter()
        .filter(|action| !action.candidate)
        .filter_map(|action| action.app_name.as_deref())
        .map(str::trim)
        .find(|name| !name.is_empty())
        .map(str::to_string)
}

fn map_tool_mapping_error(err: ToolMappingError) -> SkillError {
    SkillError::InvalidParameters(format!(
        "walkthrough action cannot become skill step: {err}"
    ))
}

#[cfg(test)]
mod tests {
    use clickweave_core::walkthrough::{
        ActionConfidence, WalkthroughAction, WalkthroughActionKind,
    };
    use clickweave_core::{MouseButton, Workflow};

    use super::*;

    fn action(kind: WalkthroughActionKind, app_name: Option<&str>) -> WalkthroughAction {
        WalkthroughAction {
            id: Uuid::new_v4(),
            kind,
            app_name: app_name.map(str::to_string),
            window_title: None,
            target_candidates: vec![],
            artifact_paths: vec![],
            source_event_ids: vec![Uuid::new_v4()],
            confidence: ActionConfidence::High,
            warnings: vec![],
            screenshot_meta: None,
            candidate: false,
        }
    }

    #[test]
    fn walkthrough_to_skill_builds_leaf_tool_sketch_from_draft() {
        let workflow_id = Uuid::new_v4();
        let actions = vec![
            action(
                WalkthroughActionKind::Click {
                    x: 12.0,
                    y: 34.0,
                    button: MouseButton::Left,
                    click_count: 1,
                },
                Some("Calculator"),
            ),
            action(
                WalkthroughActionKind::TypeText {
                    text: "42".to_string(),
                },
                Some("Calculator"),
            ),
        ];
        let draft = walkthrough::synthesize_draft(&actions, workflow_id, "Enter answer");

        let skill = walkthrough_to_skill(
            &actions,
            Some(&draft),
            "550e8400-e29b-41d4-a716-446655440000",
            &workflow_id.to_string(),
        )
        .unwrap();

        assert_eq!(skill.name, "Enter answer");
        assert_eq!(skill.state, SkillState::Draft);
        assert_eq!(skill.scope, SkillScope::ProjectLocal);
        assert_eq!(skill.stats.occurrence_count, 1);
        assert_eq!(skill.produced_node_ids.len(), 2);
        assert_eq!(skill.applicability.apps, vec!["Calculator"]);
        assert_eq!(
            skill.subgoal_signature,
            compute_subgoal_signature_from_parts("Enter answer", "Calculator", "")
        );
        assert_eq!(
            skill.applicability.signature,
            compute_applicability_signature_from_parts("Calculator", "")
        );
        assert!(matches!(
            &skill.action_sketch[0],
            ActionSketchStep::ToolCall { tool, .. } if tool == "click"
        ));
        assert!(matches!(
            &skill.action_sketch[1],
            ActionSketchStep::ToolCall { tool, .. } if tool == "type_text"
        ));
    }

    #[test]
    fn walkthrough_to_skill_ids_include_app_signature() {
        let workflow_id = Uuid::new_v4();
        let calc_actions = vec![action(
            WalkthroughActionKind::Click {
                x: 12.0,
                y: 34.0,
                button: MouseButton::Left,
                click_count: 1,
            },
            Some("Calculator"),
        )];
        let notes_actions = vec![action(
            WalkthroughActionKind::Click {
                x: 12.0,
                y: 34.0,
                button: MouseButton::Left,
                click_count: 1,
            },
            Some("Notes"),
        )];
        let calc_draft = walkthrough::synthesize_draft(&calc_actions, workflow_id, "Save result");
        let notes_draft = walkthrough::synthesize_draft(&notes_actions, workflow_id, "Save result");

        let calc = walkthrough_to_skill(
            &calc_actions,
            Some(&calc_draft),
            "550e8400-e29b-41d4-a716-446655440000",
            &workflow_id.to_string(),
        )
        .unwrap();
        let notes = walkthrough_to_skill(
            &notes_actions,
            Some(&notes_draft),
            "550e8400-e29b-41d4-a716-446655440001",
            &workflow_id.to_string(),
        )
        .unwrap();

        assert_ne!(calc.subgoal_signature, notes.subgoal_signature);
        assert_ne!(calc.id, notes.id);
    }

    #[test]
    fn walkthrough_signature_uses_first_action_app_not_sorted_app_list() {
        let workflow_id = Uuid::new_v4();
        let actions = vec![
            action(
                WalkthroughActionKind::Click {
                    x: 1.0,
                    y: 1.0,
                    button: MouseButton::Left,
                    click_count: 1,
                },
                Some("Zeta"),
            ),
            action(
                WalkthroughActionKind::Click {
                    x: 2.0,
                    y: 2.0,
                    button: MouseButton::Left,
                    click_count: 1,
                },
                Some("Alpha"),
            ),
        ];
        let draft = walkthrough::synthesize_draft(&actions, workflow_id, "Cross app");

        let skill = walkthrough_to_skill(
            &actions,
            Some(&draft),
            "550e8400-e29b-41d4-a716-446655440002",
            &workflow_id.to_string(),
        )
        .unwrap();

        assert_eq!(skill.applicability.apps, vec!["Alpha", "Zeta"]);
        assert_eq!(
            skill.applicability.signature,
            compute_applicability_signature_from_parts("Zeta", "")
        );
    }

    #[test]
    fn walkthrough_to_skill_rejects_empty_draft() {
        let workflow = Workflow {
            id: Uuid::new_v4(),
            name: "Empty".to_string(),
            ..Workflow::default()
        };

        let err = walkthrough_to_skill(&[], Some(&workflow), "session", &workflow.id.to_string())
            .unwrap_err();
        assert!(matches!(err, SkillError::InvalidParameters(_)));
    }
}
