use super::*;
use crate::agent::permissions::ToolAnnotations;
use crate::agent::task_state::TaskStateMutation;
use serde_json::json;
use std::collections::HashMap;

#[test]
fn detects_obvious_side_effectful_eval_but_allows_read_only_eval() {
    let annotations = HashMap::new();

    assert!(is_unverified_side_effect_action(
        "cdp_evaluate_script",
        &json!({"function": "() => document.querySelector('button')?.click()"}),
        &annotations,
    ));
    assert!(!is_unverified_side_effect_action(
        "cdp_evaluate_script",
        &json!({"function": "() => Array.from(document.querySelectorAll('button')).map((b) => b.textContent)"}),
        &annotations,
    ));
}

#[test]
fn uses_open_world_destructive_annotations_for_future_tools() {
    let mut annotations = HashMap::new();
    annotations.insert(
        "future_action".to_string(),
        ToolAnnotations {
            destructive_hint: Some(true),
            open_world_hint: Some(true),
            ..ToolAnnotations::default()
        },
    );

    assert!(is_unverified_side_effect_action(
        "future_action",
        &json!({}),
        &annotations,
    ));
}

#[test]
fn guard_completion_after_unverified_side_effect_strips_complete_and_blocks_done() {
    let mut turn = AgentTurn {
        mutations: vec![
            TaskStateMutation::PushSubgoal {
                text: "find target".to_string(),
            },
            TaskStateMutation::CompleteSubgoal {
                summary: "target open".to_string(),
            },
        ],
        action: AgentAction::AgentDone {
            summary: "done".to_string(),
        },
    };

    assert!(guard_completion_after_unverified_side_effect(
        Some("[UNVERIFIED SIDE EFFECT] previous result"),
        &mut turn,
    ));
    assert_eq!(turn.mutations.len(), 1);
    assert!(matches!(
        turn.mutations[0],
        TaskStateMutation::PushSubgoal { .. }
    ));
    assert!(matches!(turn.action, AgentAction::AgentReplan { .. }));
}
