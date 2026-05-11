use super::*;
use crate::agent::task_state::TaskStateMutation;

#[test]
fn mutations_apply_in_order_before_action() {
    let mut r = StateRunner::new_for_test("g".to_string());
    let turn = AgentTurn {
        mutations: vec![
            TaskStateMutation::PushSubgoal {
                text: "a".to_string(),
            },
            TaskStateMutation::PushSubgoal {
                text: "b".to_string(),
            },
        ],
        action: AgentAction::AgentDone {
            summary: "done".to_string(),
        },
    };
    let warnings = r.apply_mutations(&turn.mutations);
    assert!(warnings.is_empty());
    assert_eq!(r.task_state.subgoal_stack.len(), 2);
    assert_eq!(r.task_state.subgoal_stack[1].text, "b");
}

#[test]
fn invalid_mutation_produces_warning_but_others_still_apply() {
    let mut r = StateRunner::new_for_test("g".to_string());
    let muts = vec![
        TaskStateMutation::PushSubgoal {
            text: "a".to_string(),
        },
        TaskStateMutation::RefuteHypothesis { index: 99 },
        TaskStateMutation::PushSubgoal {
            text: "b".to_string(),
        },
    ];
    let warnings = r.apply_mutations(&muts);
    assert_eq!(warnings.len(), 1);
    assert_eq!(r.task_state.subgoal_stack.len(), 2);
}
