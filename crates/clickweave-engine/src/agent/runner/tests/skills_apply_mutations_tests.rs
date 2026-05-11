//! Spec 3 Phase 3 unit tests for the runner-side scratch fields
//! populated by `apply_mutations`.

use super::*;
use crate::agent::world_model::{AppKind, FocusedApp, Fresh, FreshnessSource};

fn focused_app(name: &str) -> Fresh<FocusedApp> {
    Fresh {
        value: FocusedApp {
            name: name.to_string(),
            kind: AppKind::Native,
            pid: 1,
        },
        written_at: 0,
        source: FreshnessSource::DirectObservation,
        ttl_steps: None,
    }
}

#[test]
fn push_subgoal_records_id_and_push_idx() {
    let mut r = StateRunner::new_for_test("g".to_string());
    r.apply_mutations(&[TaskStateMutation::PushSubgoal {
        text: "open chat".into(),
    }]);
    assert_eq!(r.last_pushed_subgoal_ids.len(), 1);
    assert_eq!(r.push_idx_stack.len(), 1);
    assert_eq!(r.push_idx_stack[0], 0); // recorded_steps was empty
    assert_eq!(r.push_signature_stack.len(), 1);
    assert_eq!(r.produced_node_ids_stack.len(), 1);
    assert!(r.produced_node_ids_stack[0].is_empty());
}

#[test]
fn complete_subgoal_drains_push_idx_into_extraction_queue() {
    let mut r = StateRunner::new_for_test("g".to_string());
    r.apply_mutations(&[TaskStateMutation::PushSubgoal {
        text: "open chat".into(),
    }]);
    r.apply_mutations(&[TaskStateMutation::CompleteSubgoal {
        summary: "done".into(),
    }]);
    assert!(r.push_idx_stack.is_empty(), "push_idx popped on complete");
    assert!(
        r.push_signature_stack.is_empty(),
        "push signature popped on complete"
    );
    assert_eq!(
        r.completed_subgoal_extraction_queue.len(),
        1,
        "extraction queue carries the completed milestone",
    );
}

#[test]
fn complete_subgoal_carries_push_time_signature() {
    let mut r = StateRunner::new_for_test("g".to_string());
    r.world_model.focused_app = Some(focused_app("Finder"));
    let push_sig =
        crate::agent::skills::signature::compute_subgoal_signature("open inbox", &r.world_model);

    r.apply_mutations(&[TaskStateMutation::PushSubgoal {
        text: "open inbox".into(),
    }]);
    r.world_model.focused_app = Some(focused_app("Mail"));
    let completion_sig =
        crate::agent::skills::signature::compute_subgoal_signature("open inbox", &r.world_model);
    r.apply_mutations(&[TaskStateMutation::CompleteSubgoal {
        summary: "done".into(),
    }]);

    let (_, _, queued_sig, _) = r
        .completed_subgoal_extraction_queue
        .first()
        .expect("queued extraction");
    assert_eq!(queued_sig, &push_sig);
    assert_ne!(queued_sig, &completion_sig);
}

#[test]
fn last_pushed_subgoal_ids_cleared_each_batch() {
    let mut r = StateRunner::new_for_test("g".to_string());
    r.apply_mutations(&[TaskStateMutation::PushSubgoal {
        text: "first".into(),
    }]);
    r.apply_mutations(&[]); // empty batch — should still clear
    assert!(r.last_pushed_subgoal_ids.is_empty());
}

#[test]
fn nested_subgoals_queue_produced_nodes_per_frame() {
    let mut r = StateRunner::new_for_test("g".to_string());
    let outer_node = uuid::Uuid::new_v4();
    let inner_node = uuid::Uuid::new_v4();
    let after_inner_node = uuid::Uuid::new_v4();

    r.apply_mutations(&[TaskStateMutation::PushSubgoal {
        text: "outer".into(),
    }]);
    r.record_produced_node_id(outer_node);

    r.apply_mutations(&[TaskStateMutation::PushSubgoal {
        text: "inner".into(),
    }]);
    r.record_produced_node_id(inner_node);

    r.apply_mutations(&[TaskStateMutation::CompleteSubgoal {
        summary: "inner done".into(),
    }]);
    r.record_produced_node_id(after_inner_node);

    r.apply_mutations(&[TaskStateMutation::CompleteSubgoal {
        summary: "outer done".into(),
    }]);

    assert!(r.produced_node_ids_stack.is_empty());
    assert_eq!(r.completed_subgoal_extraction_queue.len(), 2);
    assert_eq!(
        r.completed_subgoal_extraction_queue[0].3,
        vec![inner_node],
        "inner frame only records nodes produced after the inner push",
    );
    assert_eq!(
        r.completed_subgoal_extraction_queue[1].3,
        vec![outer_node, inner_node, after_inner_node],
        "outer frame records every node produced while it was active",
    );
}

#[test]
fn complete_with_empty_stack_records_warning_not_panic() {
    let mut r = StateRunner::new_for_test("g".to_string());
    let warnings = r.apply_mutations(&[TaskStateMutation::CompleteSubgoal {
        summary: "done".into(),
    }]);
    assert!(!warnings.is_empty());
}
