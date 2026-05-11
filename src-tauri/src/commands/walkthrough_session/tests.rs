use super::*;
use clickweave_core::{MouseButton, cdp::rand_ephemeral_port};

fn click_event(timestamp: u64, x: f64, y: f64) -> WalkthroughEvent {
    WalkthroughEvent {
        id: Uuid::new_v4(),
        timestamp,
        kind: WalkthroughEventKind::MouseClicked {
            x,
            y,
            button: MouseButton::Left,
            click_count: 1,
            modifiers: vec![],
        },
    }
}

fn stopped_event(timestamp: u64) -> WalkthroughEvent {
    WalkthroughEvent {
        id: Uuid::new_v4(),
        timestamp,
        kind: WalkthroughEventKind::Stopped,
    }
}

// --- strip_recording_bar_click ---

#[test]
fn strip_removes_click_inside_bar() {
    let bar = (100.0, 200.0, 300.0, 50.0);
    let mut events = vec![
        click_event(1, 50.0, 50.0),   // outside bar — keep
        click_event(2, 150.0, 220.0), // inside bar (last click)
    ];
    strip_recording_bar_click(&mut events, bar);
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].timestamp, 1);
}

#[test]
fn strip_keeps_click_outside_bar() {
    let bar = (100.0, 200.0, 300.0, 50.0);
    let mut events = vec![
        click_event(1, 50.0, 50.0),
        click_event(2, 50.0, 100.0), // outside bar
    ];
    strip_recording_bar_click(&mut events, bar);
    assert_eq!(events.len(), 2);
}

#[test]
fn strip_noop_when_no_clicks() {
    let bar = (100.0, 200.0, 300.0, 50.0);
    let mut events = vec![stopped_event(1)];
    strip_recording_bar_click(&mut events, bar);
    assert_eq!(events.len(), 1);
}

#[test]
fn strip_removes_all_events_with_same_timestamp() {
    let bar = (100.0, 200.0, 300.0, 50.0);
    let mut events = vec![
        click_event(1, 50.0, 50.0),   // different ts — keep
        click_event(2, 150.0, 220.0), // inside bar, ts=2
        stopped_event(2),             // same ts as bar click — also removed
    ];
    strip_recording_bar_click(&mut events, bar);
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].timestamp, 1);
}

// --- rand_ephemeral_port ---

#[test]
fn ephemeral_port_in_range() {
    for _ in 0..100 {
        let port = rand_ephemeral_port();
        assert!(
            (49152..=65535).contains(&port),
            "port {port} outside ephemeral range"
        );
    }
}
