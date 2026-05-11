use super::*;
use crate::agent::step_record::{BoundaryKind, StepRecord, WorldModelSnapshot};
use std::sync::{Arc, Mutex};

fn sample_record(step_index: usize, boundary: BoundaryKind) -> StepRecord {
    StepRecord {
        step_index,
        boundary_kind: boundary,
        world_model_snapshot: WorldModelSnapshot::from_world_model(&WorldModel::default()),
        task_state_snapshot: TaskState::new("goal".to_string()),
        action_taken: serde_json::json!({"kind":"agent_done","summary":"done"}),
        outcome: serde_json::json!({"kind":"completed"}),
        timestamp: chrono::Utc::now(),
    }
}

#[test]
fn write_step_record_is_noop_when_no_storage_attached() {
    // No storage means no panic, no events file, nothing persisted.
    let r = StateRunner::new_for_test("g".to_string());
    r.write_step_record(&sample_record(0, BoundaryKind::Terminal));
}

#[test]
fn write_step_record_appends_to_events_jsonl_when_storage_attached() {
    let tmp = tempfile::tempdir().unwrap();
    let mut storage = clickweave_core::storage::RunStorage::new(tmp.path(), "test-workflow");
    let exec_dir = storage.begin_execution().expect("begin_execution");
    let storage = Arc::new(Mutex::new(storage));

    let r = StateRunner::new_for_test("g".to_string()).with_storage(storage.clone());
    r.write_step_record(&sample_record(1, BoundaryKind::SubgoalCompleted));
    r.write_step_record(&sample_record(2, BoundaryKind::Terminal));

    let events_path = tmp
        .path()
        .join(".clickweave")
        .join("runs")
        .join("test-workflow")
        .join(&exec_dir)
        .join("events.jsonl");
    let contents = std::fs::read_to_string(&events_path)
        .unwrap_or_else(|e| panic!("read {:?} failed: {}", events_path, e));
    let subgoal: Vec<_> = contents
        .lines()
        .filter(|l| l.contains("\"boundary_kind\":\"subgoal_completed\""))
        .collect();
    assert_eq!(subgoal.len(), 1);
    let terminal: Vec<_> = contents
        .lines()
        .filter(|l| l.contains("\"boundary_kind\":\"terminal\""))
        .collect();
    assert_eq!(terminal.len(), 1);
}
