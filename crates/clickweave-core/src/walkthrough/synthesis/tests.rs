use super::super::types::*;
use super::*;
use crate::{MouseButton, WindowControlAction};

#[test]
fn test_walkthrough_status_default_is_idle() {
    assert_eq!(WalkthroughStatus::default(), WalkthroughStatus::Idle);
}

#[test]
fn test_session_meta_on_disk_shape_has_no_buffers() {
    // Buffers (events, actions) live on WalkthroughSessionRuntime, not the
    // on-disk meta record. Serializing the meta must therefore not contain
    // any raw event payloads even when the runtime holds them.
    let runtime = WalkthroughSessionRuntime {
        meta: WalkthroughSessionMeta {
            id: Uuid::new_v4(),
            project_id: Uuid::new_v4(),
            started_at: 1_700_000_000_000,
            ended_at: None,
            status: WalkthroughStatus::Recording,
            warnings: vec![],
        },
        events: vec![WalkthroughEvent {
            id: Uuid::new_v4(),
            timestamp: 1_700_000_000_100,
            kind: WalkthroughEventKind::Paused,
        }],
        actions: vec![],
    };

    let json = serde_json::to_string(&runtime.meta).expect("serialize meta");
    assert!(!json.contains("Paused"));
    assert!(!json.contains("\"events\""));
    assert!(!json.contains("\"actions\""));

    let deserialized: WalkthroughSessionMeta = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(deserialized.id, runtime.meta.id);
    assert_eq!(deserialized.status, WalkthroughStatus::Recording);
}

#[test]
fn test_walkthrough_status_serde_produces_expected_strings() {
    let variants = vec![
        (WalkthroughStatus::Idle, "\"Idle\""),
        (WalkthroughStatus::Recording, "\"Recording\""),
        (WalkthroughStatus::Paused, "\"Paused\""),
        (WalkthroughStatus::Processing, "\"Processing\""),
        (WalkthroughStatus::Review, "\"Review\""),
        (WalkthroughStatus::Applied, "\"Applied\""),
        (WalkthroughStatus::Cancelled, "\"Cancelled\""),
    ];
    for (variant, expected) in &variants {
        let json = serde_json::to_string(variant).expect("serialize");
        assert_eq!(
            &json, expected,
            "WalkthroughStatus::{variant:?} serialized incorrectly"
        );
    }
}

#[test]
fn test_event_kind_serialization_roundtrip() {
    let kinds = vec![
        WalkthroughEventKind::AppFocused {
            app_name: "Calculator".to_string(),
            pid: 1234,
            window_title: Some("Calculator".to_string()),
            app_kind: AppKind::Native,
        },
        WalkthroughEventKind::MouseClicked {
            x: 100.0,
            y: 200.0,
            button: MouseButton::Left,
            click_count: 1,
            modifiers: vec![],
        },
        WalkthroughEventKind::KeyPressed {
            key: "Enter".to_string(),
            modifiers: vec![],
        },
        WalkthroughEventKind::TextCommitted {
            text: "hello".to_string(),
        },
        WalkthroughEventKind::Scrolled {
            delta_y: -3.0,
            x: None,
            y: None,
        },
        WalkthroughEventKind::ScreenshotCaptured {
            path: "/tmp/shot.png".to_string(),
            kind: ScreenshotKind::BeforeClick,
            meta: None,
            image_b64: None,
        },
        WalkthroughEventKind::VlmLabelResolved {
            label: "Submit".to_string(),
        },
        WalkthroughEventKind::HoverDetected {
            x: 100.0,
            y: 200.0,
            element_name: "Submit".to_string(),
            element_role: Some("button".to_string()),
            dwell_ms: 1500,
            app_name: Some("Signal".to_string()),
        },
        WalkthroughEventKind::Paused,
        WalkthroughEventKind::Resumed,
        WalkthroughEventKind::Stopped,
    ];

    for kind in &kinds {
        let event = WalkthroughEvent {
            id: Uuid::new_v4(),
            timestamp: 1_700_000_000_000,
            kind: kind.clone(),
        };
        let json = serde_json::to_string(&event).expect("serialize");
        let deserialized: WalkthroughEvent = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(deserialized.id, event.id);
    }
}

#[test]
fn test_hover_detected_backward_compat_without_app_name() {
    let json = r#"{"type":"HoverDetected","x":100.0,"y":200.0,"element_name":"Submit","element_role":"button","dwell_ms":1500}"#;
    let kind: WalkthroughEventKind = serde_json::from_str(json).unwrap();
    match kind {
        WalkthroughEventKind::HoverDetected { app_name, .. } => {
            assert_eq!(app_name, None);
        }
        _ => panic!("wrong variant"),
    }
}

#[test]
fn test_app_focused_backward_compat_without_app_kind() {
    let json = r#"{"type":"AppFocused","app_name":"Calculator","pid":1234,"window_title":null}"#;
    let kind: WalkthroughEventKind = serde_json::from_str(json).unwrap();
    match kind {
        WalkthroughEventKind::AppFocused { app_kind, .. } => {
            assert_eq!(app_kind, AppKind::Native);
        }
        _ => panic!("wrong variant"),
    }
}

#[test]
fn test_action_kind_serialization_roundtrip() {
    let kinds = vec![
        WalkthroughActionKind::LaunchApp {
            app_name: "Calculator".to_string(),
            app_kind: AppKind::Native,
        },
        WalkthroughActionKind::FocusWindow {
            app_name: "Calculator".to_string(),
            window_title: Some("Calculator".to_string()),
            app_kind: AppKind::Native,
        },
        WalkthroughActionKind::Click {
            x: 100.0,
            y: 200.0,
            button: MouseButton::Left,
            click_count: 1,
        },
        WalkthroughActionKind::TypeText {
            text: "hello".to_string(),
        },
        WalkthroughActionKind::PressKey {
            key: "Enter".to_string(),
            modifiers: vec![],
        },
        WalkthroughActionKind::Scroll { delta_y: -3.0 },
    ];

    for kind in &kinds {
        let json = serde_json::to_string(kind).expect("serialize");
        let _deserialized: WalkthroughActionKind =
            serde_json::from_str(&json).expect("deserialize");
    }
}

#[test]
fn test_target_candidate_serialization_roundtrip() {
    let candidates = vec![
        TargetCandidate::AccessibilityLabel {
            label: "Submit".to_string(),
            role: Some("AXButton".to_string()),
        },
        TargetCandidate::OcrText {
            text: "Submit".to_string(),
        },
        TargetCandidate::ImageCrop {
            path: "/tmp/crop.png".to_string(),
            image_b64: "abc123".to_string(),
        },
        TargetCandidate::Coordinates { x: 100.0, y: 200.0 },
        TargetCandidate::CdpElement {
            name: "Submit".to_string(),
            role: Some("button".to_string()),
            href: None,
            parent_role: None,
            parent_name: None,
        },
    ];

    for candidate in &candidates {
        let json = serde_json::to_string(candidate).expect("serialize");
        let _deserialized: TargetCandidate = serde_json::from_str(&json).expect("deserialize");
    }
}

#[test]
fn test_annotations_default() {
    let annotations = WalkthroughAnnotations::default();
    assert!(annotations.deleted_node_ids.is_empty());
    assert!(annotations.renamed_nodes.is_empty());
    assert!(annotations.target_overrides.is_empty());
    assert!(annotations.variable_promotions.is_empty());
}

#[test]
fn test_storage_create_and_save_session() {
    use super::super::storage::WalkthroughStorage;

    let dir = std::env::temp_dir()
        .join("clickweave_test_wt")
        .join(Uuid::new_v4().to_string());
    let storage = WalkthroughStorage::new(&dir);

    let session = WalkthroughSessionMeta {
        id: Uuid::new_v4(),
        project_id: Uuid::new_v4(),
        started_at: 1_700_000_000_000,
        ended_at: None,
        status: WalkthroughStatus::Recording,
        warnings: vec![],
    };

    let session_dir = storage
        .create_session_dir(&session)
        .expect("create session dir");
    assert!(session_dir.exists());
    assert!(session_dir.join("artifacts").exists());

    storage
        .save_session(&session_dir, &session)
        .expect("save session");
    assert!(session_dir.join("session.json").exists());

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn test_storage_append_event() {
    use super::super::storage::WalkthroughStorage;

    let dir = std::env::temp_dir()
        .join("clickweave_test_wt")
        .join(Uuid::new_v4().to_string());
    let storage = WalkthroughStorage::new(&dir);

    let session = WalkthroughSessionMeta {
        id: Uuid::new_v4(),
        project_id: Uuid::new_v4(),
        started_at: 1_700_000_000_000,
        ended_at: None,
        status: WalkthroughStatus::Recording,
        warnings: vec![],
    };

    let session_dir = storage
        .create_session_dir(&session)
        .expect("create session dir");

    let event = WalkthroughEvent {
        id: Uuid::new_v4(),
        timestamp: 1_700_000_000_100,
        kind: WalkthroughEventKind::AppFocused {
            app_name: "Calculator".to_string(),
            pid: 1234,
            window_title: Some("Calculator".to_string()),
            app_kind: AppKind::Native,
        },
    };

    storage
        .append_event(&session_dir, &event)
        .expect("append event");

    let content =
        std::fs::read_to_string(session_dir.join("events.jsonl")).expect("read events.jsonl");
    assert!(content.contains("Calculator"));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn test_storage_read_events_roundtrip() {
    use super::super::storage::WalkthroughStorage;

    let dir = std::env::temp_dir()
        .join("clickweave_test_wt")
        .join(Uuid::new_v4().to_string());
    let storage = WalkthroughStorage::new(&dir);
    let session = WalkthroughSessionMeta::new(Uuid::new_v4());
    let session_dir = storage.create_session_dir(&session).expect("create dir");

    let ev1 = WalkthroughEvent {
        id: Uuid::new_v4(),
        timestamp: 1000,
        kind: WalkthroughEventKind::AppFocused {
            app_name: "Calculator".into(),
            pid: 100,
            window_title: None,
            app_kind: AppKind::Native,
        },
    };
    let ev2 = WalkthroughEvent {
        id: Uuid::new_v4(),
        timestamp: 2000,
        kind: WalkthroughEventKind::KeyPressed {
            key: "return".into(),
            modifiers: vec![],
        },
    };
    storage.append_event(&session_dir, &ev1).expect("append");
    storage.append_event(&session_dir, &ev2).expect("append");

    let events = storage.read_events(&session_dir).expect("read events");
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].id, ev1.id);
    assert_eq!(events[1].id, ev2.id);

    let _ = std::fs::remove_dir_all(&dir);
}

mod normalize_tests {
    use super::super::super::test_helpers::make_event;
    use super::*;

    #[test]
    fn test_first_app_focus_becomes_launch_app() {
        let events = vec![make_event(
            1000,
            WalkthroughEventKind::AppFocused {
                app_name: "Calculator".into(),
                pid: 100,
                window_title: Some("Calculator".into()),
                app_kind: AppKind::Native,
            },
        )];
        let (actions, _warnings) = normalize_events(&events);
        assert_eq!(actions.len(), 1);
        assert!(matches!(
            &actions[0].kind,
            WalkthroughActionKind::LaunchApp { app_name, .. } if app_name == "Calculator"
        ));
        assert_eq!(actions[0].confidence, ActionConfidence::High);
    }

    #[test]
    fn test_repeated_focus_same_app_collapsed() {
        let events = vec![
            make_event(
                1000,
                WalkthroughEventKind::AppFocused {
                    app_name: "Calculator".into(),
                    pid: 100,
                    window_title: None,
                    app_kind: AppKind::Native,
                },
            ),
            make_event(
                1100,
                WalkthroughEventKind::AppFocused {
                    app_name: "Calculator".into(),
                    pid: 100,
                    window_title: None,
                    app_kind: AppKind::Native,
                },
            ),
        ];
        let (actions, _) = normalize_events(&events);
        assert_eq!(actions.len(), 1);
    }

    #[test]
    fn test_refocus_previously_seen_app_becomes_focus_window() {
        let events = vec![
            make_event(
                1000,
                WalkthroughEventKind::AppFocused {
                    app_name: "Calculator".into(),
                    pid: 100,
                    window_title: None,
                    app_kind: AppKind::Native,
                },
            ),
            make_event(
                2000,
                WalkthroughEventKind::AppFocused {
                    app_name: "Notes".into(),
                    pid: 200,
                    window_title: None,
                    app_kind: AppKind::Native,
                },
            ),
            make_event(
                3000,
                WalkthroughEventKind::AppFocused {
                    app_name: "Calculator".into(),
                    pid: 100,
                    window_title: None,
                    app_kind: AppKind::Native,
                },
            ),
        ];
        let (actions, _) = normalize_events(&events);
        assert_eq!(actions.len(), 3);
        assert!(matches!(
            &actions[0].kind,
            WalkthroughActionKind::LaunchApp { .. }
        ));
        assert!(matches!(
            &actions[1].kind,
            WalkthroughActionKind::LaunchApp { .. }
        ));
        assert!(matches!(
            &actions[2].kind,
            WalkthroughActionKind::FocusWindow { .. }
        ));
    }

    #[test]
    fn test_key_pressed_becomes_press_key() {
        let events = vec![make_event(
            1000,
            WalkthroughEventKind::KeyPressed {
                key: "return".into(),
                modifiers: vec![],
            },
        )];
        let (actions, _) = normalize_events(&events);
        assert_eq!(actions.len(), 1);
        assert!(matches!(
            &actions[0].kind,
            WalkthroughActionKind::PressKey { key, .. } if key == "return"
        ));
    }

    #[test]
    fn test_scroll_preserved() {
        let events = vec![make_event(
            1000,
            WalkthroughEventKind::Scrolled {
                delta_y: -5.0,
                x: Some(100.0),
                y: Some(200.0),
            },
        )];
        let (actions, _) = normalize_events(&events);
        assert_eq!(actions.len(), 1);
        assert!(matches!(
            &actions[0].kind,
            WalkthroughActionKind::Scroll { delta_y } if *delta_y == -5.0
        ));
    }

    #[test]
    fn test_paused_resumed_stopped_events_skipped() {
        let events = vec![
            make_event(1000, WalkthroughEventKind::Paused),
            make_event(2000, WalkthroughEventKind::Resumed),
            make_event(3000, WalkthroughEventKind::Stopped),
        ];
        let (actions, _) = normalize_events(&events);
        assert!(actions.is_empty());
    }

    #[test]
    fn test_screenshot_events_skipped() {
        let events = vec![make_event(
            1000,
            WalkthroughEventKind::ScreenshotCaptured {
                path: "/tmp/shot.png".into(),
                kind: ScreenshotKind::AfterClick,
                meta: None,
                image_b64: None,
            },
        )];
        let (actions, _) = normalize_events(&events);
        assert!(actions.is_empty());
    }

    #[test]
    fn test_app_kind_propagated_to_actions() {
        let events = vec![make_event(
            1000,
            WalkthroughEventKind::AppFocused {
                app_name: "Discord".into(),
                pid: 100,
                window_title: None,
                app_kind: AppKind::ElectronApp,
            },
        )];
        let (actions, _) = normalize_events(&events);
        assert_eq!(actions.len(), 1);
        match &actions[0].kind {
            WalkthroughActionKind::LaunchApp { app_kind, .. } => {
                assert_eq!(*app_kind, AppKind::ElectronApp);
            }
            _ => panic!("expected LaunchApp"),
        }
    }

    #[test]
    fn test_close_button_click_becomes_window_control_via_label() {
        let events = vec![
            make_event(
                1000,
                WalkthroughEventKind::MouseClicked {
                    x: 14.0,
                    y: 12.0,
                    button: MouseButton::Left,
                    click_count: 1,
                    modifiers: vec![],
                },
            ),
            make_event(
                1000,
                WalkthroughEventKind::AccessibilityElementCaptured {
                    label: "close button".to_string(),
                    role: Some("AXButton".to_string()),
                    subrole: None,
                },
            ),
        ];
        let (actions, _) = normalize_events(&events);
        assert_eq!(actions.len(), 1);
        // Close becomes a Click with WindowControl target (not PressKey).
        assert!(matches!(
            &actions[0].kind,
            WalkthroughActionKind::Click { .. }
        ));
        assert!(matches!(
            actions[0].target_candidates.first(),
            Some(TargetCandidate::WindowControl {
                action: WindowControlAction::Close
            })
        ));
    }

    #[test]
    fn test_close_via_subrole_no_label() {
        // Electron apps: traffic light buttons have subrole but no label.
        let events = vec![
            make_event(
                1000,
                WalkthroughEventKind::MouseClicked {
                    x: 48.0,
                    y: 52.0,
                    button: MouseButton::Left,
                    click_count: 1,
                    modifiers: vec![],
                },
            ),
            make_event(
                1000,
                WalkthroughEventKind::AccessibilityElementCaptured {
                    label: String::new(),
                    role: Some("AXButton".to_string()),
                    subrole: Some("AXCloseButton".to_string()),
                },
            ),
        ];
        let (actions, _) = normalize_events(&events);
        assert_eq!(actions.len(), 1);
        assert!(matches!(
            &actions[0].kind,
            WalkthroughActionKind::Click { .. }
        ));
        assert!(matches!(
            actions[0].target_candidates.first(),
            Some(TargetCandidate::WindowControl {
                action: WindowControlAction::Close
            })
        ));
    }

    #[test]
    fn test_minimize_button_click_becomes_press_key() {
        let events = vec![
            make_event(
                1000,
                WalkthroughEventKind::MouseClicked {
                    x: 34.0,
                    y: 12.0,
                    button: MouseButton::Left,
                    click_count: 1,
                    modifiers: vec![],
                },
            ),
            make_event(
                1000,
                WalkthroughEventKind::AccessibilityElementCaptured {
                    label: String::new(),
                    role: Some("AXButton".to_string()),
                    subrole: Some("AXMinimizeButton".to_string()),
                },
            ),
        ];
        let (actions, _) = normalize_events(&events);
        assert_eq!(actions.len(), 1);
        assert!(matches!(
            &actions[0].kind,
            WalkthroughActionKind::PressKey { key, modifiers }
                if key == "m" && modifiers == &["command"]
        ));
    }

    #[test]
    fn test_zoom_button_click_becomes_window_relative_click() {
        let events = vec![
            make_event(
                1000,
                WalkthroughEventKind::MouseClicked {
                    x: 54.0,
                    y: 12.0,
                    button: MouseButton::Left,
                    click_count: 1,
                    modifiers: vec![],
                },
            ),
            make_event(
                1000,
                WalkthroughEventKind::AccessibilityElementCaptured {
                    label: String::new(),
                    role: Some("AXButton".to_string()),
                    subrole: Some("AXZoomButton".to_string()),
                },
            ),
        ];
        let (actions, _) = normalize_events(&events);
        assert_eq!(actions.len(), 1);
        // Zoom button emits a Click with WindowControl target (resolved at
        // execution time), NOT a PressKey — Ctrl+Cmd+F would toggle full
        // screen instead of zooming the window.
        assert!(matches!(
            &actions[0].kind,
            WalkthroughActionKind::Click { .. }
        ));
        assert!(matches!(
            actions[0].target_candidates.as_slice(),
            [TargetCandidate::WindowControl {
                action: WindowControlAction::Zoom
            }]
        ));
    }

    #[test]
    fn test_fullscreen_button_click_becomes_press_key() {
        let events = vec![
            make_event(
                1000,
                WalkthroughEventKind::MouseClicked {
                    x: 54.0,
                    y: 12.0,
                    button: MouseButton::Left,
                    click_count: 1,
                    modifiers: vec![],
                },
            ),
            make_event(
                1000,
                WalkthroughEventKind::AccessibilityElementCaptured {
                    label: String::new(),
                    role: Some("AXButton".to_string()),
                    subrole: Some("AXFullScreenButton".to_string()),
                },
            ),
        ];
        let (actions, _) = normalize_events(&events);
        assert_eq!(actions.len(), 1);
        assert!(matches!(
            &actions[0].kind,
            WalkthroughActionKind::PressKey { key, modifiers }
                if key == "f" && modifiers == &["command", "control"]
        ));
    }
}

