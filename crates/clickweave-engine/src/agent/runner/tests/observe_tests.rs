use super::*;

#[test]
fn observe_applies_pending_events_and_infers_phase() {
    let mut runner = StateRunner::new_for_test("goal".to_string());
    runner.queue_invalidation(InvalidationEvent::FocusChanging {
        tool: "launch_app".to_string(),
    });
    runner.observe();
    assert_eq!(
        runner.task_state.phase,
        crate::agent::phase::Phase::Exploring
    );
}
