//! Direct tests for `queue_invalidations_for_tool_success` and
//! `queue_snapshot_stale_if_aged` — both fire pending events that
//! `observe()` drains.

use super::*;
use crate::agent::world_model::{
    AxSnapshotData, Fresh, FreshnessSource, InvalidationEvent, ScreenshotRef, SnapshotKind,
};
use serde_json::json;

fn runner() -> StateRunner {
    StateRunner::new_for_test("test goal".to_string())
}

#[test]
fn focus_window_queues_focus_changing() {
    let mut r = runner();
    r.queue_invalidations_for_tool_success("focus_window", &json!({"app_name": "Safari"}));
    assert!(matches!(
        r.pending_events.as_slice(),
        [InvalidationEvent::FocusChanging { tool }] if tool == "focus_window"
    ));
}

#[test]
fn launch_app_queues_focus_and_lifecycle() {
    let mut r = runner();
    r.queue_invalidations_for_tool_success("launch_app", &json!({"app_name": "Mail"}));
    assert_eq!(r.pending_events.len(), 2);
    assert!(matches!(
        r.pending_events[0],
        InvalidationEvent::FocusChanging { .. }
    ));
    assert!(matches!(
        r.pending_events[1],
        InvalidationEvent::AppLifecycle { .. }
    ));
}

#[test]
fn quit_app_queues_focus_and_lifecycle() {
    let mut r = runner();
    r.queue_invalidations_for_tool_success("quit_app", &json!({"app_name": "Mail"}));
    assert_eq!(r.pending_events.len(), 2);
}

#[test]
fn cdp_navigate_queues_navigation_with_url() {
    let mut r = runner();
    r.queue_invalidations_for_tool_success(
        "cdp_navigate",
        &json!({"url": "https://example.com/login"}),
    );
    match r.pending_events.as_slice() {
        [InvalidationEvent::CdpNavigation { new_url }] => {
            assert_eq!(new_url, "https://example.com/login");
        }
        _ => panic!("expected CdpNavigation event"),
    }
}

#[test]
fn cdp_select_page_queues_navigation_even_without_url() {
    let mut r = runner();
    r.queue_invalidations_for_tool_success("cdp_select_page", &json!({"page_index": 1}));
    assert!(matches!(
        r.pending_events.as_slice(),
        [InvalidationEvent::CdpNavigation { new_url }] if new_url.is_empty()
    ));
}

#[test]
fn unrelated_tool_queues_nothing() {
    let mut r = runner();
    r.queue_invalidations_for_tool_success("cdp_click", &json!({"uid": "d1"}));
    assert!(r.pending_events.is_empty());
}

#[test]
fn snapshot_stale_fires_only_for_aged_ax_field() {
    let mut r = runner();
    r.world_model.last_native_ax_snapshot = Some(Fresh {
        value: AxSnapshotData {
            snapshot_id: "ax-0".into(),
            element_count: 0,
            captured_at_step: 0,
            ax_tree_text: String::new(),
        },
        written_at: 0,
        source: FreshnessSource::DirectObservation,
        ttl_steps: Some(2),
    });
    r.step_index = 5; // age = 5, TTL = 2 → should fire.
    r.queue_snapshot_stale_if_aged();
    assert!(matches!(
        r.pending_events.as_slice(),
        [InvalidationEvent::SnapshotStale {
            kind: SnapshotKind::NativeAx,
            age_steps: 5,
        }]
    ));
}

#[test]
fn snapshot_stale_no_op_when_within_ttl() {
    let mut r = runner();
    r.world_model.last_screenshot = Some(Fresh {
        value: ScreenshotRef {
            screenshot_id: "ss-0".into(),
            captured_at_step: 0,
        },
        written_at: 3,
        source: FreshnessSource::DirectObservation,
        ttl_steps: Some(8),
    });
    r.step_index = 5; // age = 2, TTL = 8 → no event.
    r.queue_snapshot_stale_if_aged();
    assert!(r.pending_events.is_empty());
}

#[test]
fn stale_ax_does_not_invalidate_fresh_screenshot() {
    // The bug being prevented: AX captured at step 0 (TTL 2) and
    // a screenshot captured at step 4 (TTL 4). At step 5, AX is
    // stale (age 5 > TTL 2) but the screenshot is fresh
    // (age 1 < TTL 4). A single `SnapshotStale { age_steps = 5 }`
    // event would have dragged the screenshot down too; the new
    // shape queues per-kind so apply only clears AX.
    let mut r = runner();
    r.world_model.last_native_ax_snapshot = Some(Fresh {
        value: AxSnapshotData {
            snapshot_id: "ax-0".into(),
            element_count: 0,
            captured_at_step: 0,
            ax_tree_text: String::new(),
        },
        written_at: 0,
        source: FreshnessSource::DirectObservation,
        ttl_steps: Some(2),
    });
    r.world_model.last_screenshot = Some(Fresh {
        value: ScreenshotRef {
            screenshot_id: "ss-1".into(),
            captured_at_step: 4,
        },
        written_at: 4,
        source: FreshnessSource::DirectObservation,
        ttl_steps: Some(4),
    });
    r.step_index = 5;
    r.queue_snapshot_stale_if_aged();
    let queued = std::mem::take(&mut r.pending_events);
    r.world_model.apply_events(queued);
    assert!(
        r.world_model.last_native_ax_snapshot.is_none(),
        "stale AX must be cleared"
    );
    assert!(
        r.world_model.last_screenshot.is_some(),
        "fresh screenshot must survive AX going stale"
    );
}
