use super::*;

#[test]
fn take_ax_snapshot_success_populates_last_native_ax_snapshot() {
    let mut r = StateRunner::new_for_test("g".to_string());
    r.step_index = 5;
    let body = "uid=a1g3 button \"OK\"\n  uid=a2g3 textbox";
    r.update_continuity_after_tool_success("take_ax_snapshot", body);
    let ax = r.world_model.last_native_ax_snapshot.as_ref().unwrap();
    assert_eq!(ax.value.captured_at_step, 5);
    assert!(ax.value.element_count >= 2);
    assert!(ax.value.ax_tree_text.contains("uid=a1g3"));
}

#[test]
fn take_screenshot_success_populates_last_screenshot_ref() {
    let mut r = StateRunner::new_for_test("g".to_string());
    r.step_index = 4;
    let body = r#"{"screenshot_id":"ss-abc","width":1440,"height":900}"#;
    r.update_continuity_after_tool_success("take_screenshot", body);
    let s = r.world_model.last_screenshot.as_ref().unwrap();
    assert_eq!(s.value.screenshot_id, "ss-abc");
    assert_eq!(s.value.captured_at_step, 4);
}

#[test]
fn non_snapshot_tool_does_not_touch_continuity() {
    let mut r = StateRunner::new_for_test("g".to_string());
    r.update_continuity_after_tool_success("cdp_click", "ok");
    assert!(r.world_model.last_native_ax_snapshot.is_none());
    assert!(r.world_model.last_screenshot.is_none());
}
