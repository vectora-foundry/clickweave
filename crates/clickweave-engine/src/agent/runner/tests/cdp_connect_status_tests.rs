use super::*;

#[test]
fn record_cdp_connect_failure_writes_fresh_status() {
    let mut runner = StateRunner::new_for_test("g".to_string());
    assert!(runner.world_model.cdp_connect_status.is_none());
    runner.record_cdp_connect_failure("probe_app failed for X: y".to_string());
    let status = runner
        .world_model
        .cdp_connect_status
        .as_ref()
        .expect("status set");
    assert_eq!(status.value, "probe_app failed for X: y");
    assert_eq!(status.written_at, runner.step_index);
}

#[test]
fn second_failure_overwrites_first() {
    let mut runner = StateRunner::new_for_test("g".to_string());
    runner.record_cdp_connect_failure("first".to_string());
    runner.record_cdp_connect_failure("second".to_string());
    assert_eq!(
        runner
            .world_model
            .cdp_connect_status
            .as_ref()
            .unwrap()
            .value,
        "second",
    );
}
