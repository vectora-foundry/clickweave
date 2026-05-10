//! Walkthrough → skill conversion.
//!
//! Provides a direct path from `WalkthroughAction` →
//! `Vec<ActionSketchStep>` without a `Workflow` intermediary.
//! Used by the `save_walkthrough_as_skill` Tauri command.

use chrono::Utc;
use clickweave_core::walkthrough::{WalkthroughAction, WalkthroughActionKind};
use serde_json::json;

use super::extractor::synthesize_skill_id_for_signature;
use super::signature::{
    compute_applicability_signature_from_parts, compute_subgoal_signature_from_parts,
};
use super::types::{
    ActionSketchStep, ApplicabilityHints, ExpectedWorldModelDelta, OutcomePredicate,
    ProvenanceEntry, Skill, SkillError, SkillScope, SkillState, SkillStats,
};

// ── Direct path: actions → ActionSketchStep[] ──────────────────────────

/// Convert a slice of `WalkthroughAction` directly into `ActionSketchStep`s
/// without an intermediate `Workflow`. Each confirmed (non-candidate) action
/// becomes one `ToolCall` step. Candidate hover actions are skipped — only
/// confirmed actions produce steps.
///
/// Returns an error if there are no confirmed actions to convert.
pub fn actions_to_sketch(
    actions: &[WalkthroughAction],
) -> Result<Vec<ActionSketchStep>, SkillError> {
    let confirmed: Vec<_> = actions.iter().filter(|a| !a.candidate).collect();
    if confirmed.is_empty() {
        return Err(SkillError::InvalidParameters(
            "walkthrough has no confirmed actions".to_string(),
        ));
    }

    confirmed
        .into_iter()
        .enumerate()
        .map(|(idx, action)| {
            let (tool, args) = action_kind_to_tool(&action.kind);
            Ok(ActionSketchStep::ToolCall {
                step_id: format!("s_{idx:06}"),
                tool,
                args,
                captures_pre: vec![],
                captures: vec![],
                expected_world_model_delta: ExpectedWorldModelDelta::default(),
                requires_approval: None,
            })
        })
        .collect()
}

/// Map a `WalkthroughActionKind` to its MCP tool name + arguments JSON.
fn action_kind_to_tool(kind: &WalkthroughActionKind) -> (String, serde_json::Value) {
    match kind {
        WalkthroughActionKind::Click {
            x,
            y,
            button,
            click_count,
        } => {
            let btn = match button {
                clickweave_core::MouseButton::Left => "left",
                clickweave_core::MouseButton::Right => "right",
                clickweave_core::MouseButton::Center => "middle",
            };
            (
                "click".to_string(),
                json!({ "x": x, "y": y, "button": btn, "click_count": click_count }),
            )
        }
        WalkthroughActionKind::TypeText { text } => {
            ("type_text".to_string(), json!({ "text": text }))
        }
        WalkthroughActionKind::PressKey { key, modifiers } => (
            "press_key".to_string(),
            json!({ "key": key, "modifiers": modifiers }),
        ),
        WalkthroughActionKind::Scroll { delta_y } => {
            ("scroll".to_string(), json!({ "delta_y": delta_y }))
        }
        WalkthroughActionKind::LaunchApp { app_name, .. } => {
            ("launch_app".to_string(), json!({ "app_name": app_name }))
        }
        WalkthroughActionKind::FocusWindow {
            app_name,
            window_title,
            ..
        } => {
            let mut args = json!({ "app_name": app_name });
            if let Some(title) = window_title {
                args["window_title"] = json!(title);
            }
            ("focus_window".to_string(), args)
        }
        WalkthroughActionKind::Hover { x, y, dwell_ms } => (
            "move_mouse".to_string(),
            json!({ "x": x, "y": y, "dwell_ms": dwell_ms }),
        ),
    }
}

// ── Build a Skill from an action sketch ────────────────────────────────

/// Assemble a [`Skill`] from a pre-built `action_sketch`, pre-generated prose
/// `body`, and session/project metadata.
pub fn build_skill_from_sketch(
    actions: &[WalkthroughAction],
    action_sketch: Vec<ActionSketchStep>,
    body: String,
    name: &str,
    description: &str,
    session_id: &str,
    project_id: &str,
) -> Skill {
    let now = Utc::now();
    let apps = action_apps(actions);
    let focused_app = first_action_app(actions).unwrap_or_default();
    let subgoal_signature = compute_subgoal_signature_from_parts(name, &focused_app, "");
    let id = synthesize_skill_id_for_signature(name, &subgoal_signature);
    let applicability = ApplicabilityHints {
        apps,
        hosts: vec![],
        signature: compute_applicability_signature_from_parts(&focused_app, ""),
    };

    Skill {
        id,
        version: 1,
        state: SkillState::Draft,
        scope: SkillScope::ProjectLocal,
        name: name.to_string(),
        description: description.to_string(),
        tags: vec!["walkthrough".to_string()],
        subgoal_text: name.to_string(),
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
        produced_node_ids: vec![],
        body,
        schema_version: super::SKILL_SCHEMA_VERSION,
        variables: vec![],
        sections: vec![],
        replay: None,
    }
}

// ── Helpers ─────────────────────────────────────────────────────────────

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

#[cfg(test)]
mod tests {
    use clickweave_core::walkthrough::{
        ActionConfidence, WalkthroughAction, WalkthroughActionKind,
    };
    use clickweave_core::MouseButton;
    use uuid::Uuid;

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

    // ── actions_to_sketch tests ──────────────────────────────────────

    #[test]
    fn actions_to_sketch_maps_click_and_type() {
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
        let sketch = actions_to_sketch(&actions).unwrap();
        assert_eq!(sketch.len(), 2);
        assert!(matches!(&sketch[0], ActionSketchStep::ToolCall { tool, .. } if tool == "click"));
        assert!(
            matches!(&sketch[1], ActionSketchStep::ToolCall { tool, .. } if tool == "type_text")
        );
    }

    #[test]
    fn actions_to_sketch_rejects_all_candidates() {
        let mut a = action(
            WalkthroughActionKind::Click {
                x: 1.0,
                y: 1.0,
                button: MouseButton::Left,
                click_count: 1,
            },
            None,
        );
        a.candidate = true;
        let err = actions_to_sketch(&[a]).unwrap_err();
        assert!(matches!(err, SkillError::InvalidParameters(_)));
    }

    #[test]
    fn actions_to_sketch_skips_candidate_actions() {
        let mut candidate = action(
            WalkthroughActionKind::Click {
                x: 5.0,
                y: 5.0,
                button: MouseButton::Left,
                click_count: 1,
            },
            None,
        );
        candidate.candidate = true;
        let confirmed = action(
            WalkthroughActionKind::TypeText {
                text: "hi".to_string(),
            },
            None,
        );
        let sketch = actions_to_sketch(&[candidate, confirmed]).unwrap();
        assert_eq!(sketch.len(), 1);
        assert!(
            matches!(&sketch[0], ActionSketchStep::ToolCall { tool, .. } if tool == "type_text")
        );
    }

}
