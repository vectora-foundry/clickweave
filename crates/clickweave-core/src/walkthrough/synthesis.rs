use uuid::Uuid;

use crate::MouseButton;

use super::event_coalescing::{
    KEY_COALESCE_GAP_MS, SCROLL_COALESCE_GAP_MS, TEXT_IDLE_GAP_MS, flush_text,
};
use super::event_interpretation::{WindowControl, shortcut_display_name};
use super::target_resolution::{
    CdpElementData, ClickEnrichment, build_target_candidates, score_confidence,
};
use super::types::{
    ActionConfidence, ScreenshotKind, ScreenshotMeta, TargetCandidate, WalkthroughAction,
    WalkthroughActionKind, WalkthroughEvent, WalkthroughEventKind,
};

// Re-export OCR_PROXIMITY_PX so external callers (if any) still find it here.
pub use super::target_resolution::OCR_PROXIMITY_PX;

// --- Event normalization ---

/// Normalize raw walkthrough events into semantic actions.
///
/// Returns `(actions, warnings)`. Pure function — no I/O.
pub fn normalize_events(events: &[WalkthroughEvent]) -> (Vec<WalkthroughAction>, Vec<String>) {
    // Sort by timestamp so each click is followed by its enrichment events.
    // Background enrichment tasks append events out-of-order (after later
    // clicks), but reuse the original click's timestamp. Stable sort keeps
    // the click before its enrichment within the same timestamp.
    let mut sorted = events.to_vec();
    sorted.sort_by_key(|e| e.timestamp);
    let events = &sorted;

    let mut actions: Vec<WalkthroughAction> = Vec::new();
    let warnings: Vec<String> = Vec::new();
    let mut seen_apps: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut last_app: Option<String> = None;
    let mut text_buffer: Vec<(Uuid, u64, String)> = Vec::new();
    let mut last_scroll_ts: u64 = 0;
    let mut last_key_ts: u64 = 0;
    let mut i = 0;

    while i < events.len() {
        let event = &events[i];
        i += 1;
        match &event.kind {
            WalkthroughEventKind::AppFocused {
                app_name,
                window_title,
                app_kind,
                ..
            } => {
                flush_text(&mut text_buffer, &mut actions, &last_app);

                // Collapse repeated focus on same app, but update app_kind
                // if it changed (e.g. reactive Electron reclassification).
                if last_app.as_ref() == Some(app_name) {
                    // Search backward for the most recent focus/launch action
                    // for this app — it may not be the very last action (clicks
                    // can appear between the original focus and the correction).
                    for prev in actions.iter_mut().rev() {
                        let (prev_name, prev_kind) = match &mut prev.kind {
                            WalkthroughActionKind::LaunchApp {
                                app_name: name,
                                app_kind: kind,
                            } => (name as &str, kind),
                            WalkthroughActionKind::FocusWindow {
                                app_name: name,
                                app_kind: kind,
                                ..
                            } => (name as &str, kind),
                            _ => continue,
                        };
                        if prev_name == app_name && *prev_kind != *app_kind {
                            *prev_kind = *app_kind;
                            break;
                        }
                    }
                    continue;
                }

                let is_new = seen_apps.insert(app_name.clone());
                let kind = if is_new {
                    WalkthroughActionKind::LaunchApp {
                        app_name: app_name.clone(),
                        app_kind: *app_kind,
                    }
                } else {
                    WalkthroughActionKind::FocusWindow {
                        app_name: app_name.clone(),
                        window_title: window_title.clone(),
                        app_kind: *app_kind,
                    }
                };

                let mut action =
                    WalkthroughAction::new(kind, Some(app_name.clone()), vec![event.id]);
                action.window_title = window_title.clone();
                actions.push(action);
                last_app = Some(app_name.clone());
            }

            WalkthroughEventKind::TextCommitted { text } => {
                // Check for idle gap — break text group.
                if let Some((_, last_ts, _)) = text_buffer.last()
                    && event.timestamp - last_ts > TEXT_IDLE_GAP_MS
                {
                    flush_text(&mut text_buffer, &mut actions, &last_app);
                }
                text_buffer.push((event.id, event.timestamp, text.clone()));
            }

            WalkthroughEventKind::MouseClicked {
                x,
                y,
                button,
                click_count,
                ..
            } => {
                flush_text(&mut text_buffer, &mut actions, &last_app);

                // Lookahead: collect enrichment events (screenshot, OCR, accessibility)
                // that follow this click before the next action event.
                let mut screenshot_path: Option<String> = None;
                let mut screenshot_meta: Option<ScreenshotMeta> = None;
                let mut ocr_annotations = None;
                let mut ax_label: Option<(String, Option<String>, Option<String>)> = None;
                let mut vlm_label: Option<String> = None;
                let mut crop_candidate: Option<(String, String)> = None;
                let mut cdp_resolved: Option<CdpElementData> = None;
                let mut peek = i;
                while peek < events.len() {
                    match &events[peek].kind {
                        WalkthroughEventKind::ScreenshotCaptured {
                            path,
                            kind: ScreenshotKind::ClickCrop,
                            image_b64: Some(b64),
                            ..
                        } => {
                            crop_candidate = Some((path.clone(), b64.clone()));
                        }
                        WalkthroughEventKind::ScreenshotCaptured { path, meta, .. } => {
                            screenshot_path = Some(path.clone());
                            screenshot_meta = *meta;
                        }
                        WalkthroughEventKind::OcrCaptured { annotations, .. } => {
                            ocr_annotations = Some(annotations);
                        }
                        WalkthroughEventKind::AccessibilityElementCaptured {
                            label,
                            role,
                            subrole,
                        } => {
                            ax_label = Some((label.clone(), role.clone(), subrole.clone()));
                        }
                        WalkthroughEventKind::VlmLabelResolved { label } => {
                            vlm_label = Some(label.clone());
                        }
                        WalkthroughEventKind::CdpClickResolved {
                            name,
                            role,
                            href,
                            parent_role,
                            parent_name,
                            ..
                        } => {
                            cdp_resolved = Some(CdpElementData {
                                name: name.clone(),
                                role: role.clone(),
                                href: href.clone(),
                                parent_role: parent_role.clone(),
                                parent_name: parent_name.clone(),
                            });
                        }
                        // Stop at the next action event.
                        _ => break,
                    }
                    peek += 1;
                }
                // Advance past consumed enrichment events.
                i = peek;

                // Window control buttons (close, minimize, maximize/zoom):
                // - Close/Zoom → window-relative click (no reliable shortcut)
                // - Minimize → Cmd+M, Maximize (full screen) → Ctrl+Cmd+F
                if let Some((ref label, ref role, ref subrole)) = ax_label
                    && let Some(wc) = WindowControl::from_accessibility(
                        label,
                        role.as_deref(),
                        subrole.as_deref(),
                    )
                {
                    if wc.shortcut().is_none() {
                        // Emit a Click action with a WindowControl target candidate.
                        // The executor resolves this to window-relative coordinates.
                        let mut action = WalkthroughAction::new(
                            WalkthroughActionKind::Click {
                                x: *x,
                                y: *y,
                                button: MouseButton::Left,
                                click_count: 1,
                            },
                            last_app.clone(),
                            vec![event.id],
                        );
                        action.target_candidates = vec![TargetCandidate::WindowControl {
                            action: wc.to_action(),
                        }];
                        action.confidence = ActionConfidence::High;
                        actions.push(action);
                    } else if let Some((key, modifiers)) = wc.shortcut() {
                        actions.push(WalkthroughAction::new(
                            WalkthroughActionKind::PressKey {
                                key: key.to_string(),
                                modifiers,
                            },
                            last_app.clone(),
                            vec![event.id],
                        ));
                    }
                    continue;
                }

                // Build target candidates and score confidence via extracted functions.
                let ax_for_candidates = ax_label.map(|(label, role, _subrole)| (label, role));

                let enrichment = ClickEnrichment {
                    ax_label: ax_for_candidates,
                    vlm_label,
                    ocr_annotations: ocr_annotations.as_ref().map(|a| *a),
                    crop_candidate,
                    cdp_resolved,
                    click_x: *x,
                    click_y: *y,
                };

                let candidates = build_target_candidates(&enrichment);
                let confidence = score_confidence(&candidates);

                let mut click_warnings = Vec::new();
                if confidence == ActionConfidence::Low {
                    click_warnings.push(format!(
                        "No text target found for click at ({x:.0}, {y:.0}) — using coordinates"
                    ));
                }

                let mut action = WalkthroughAction::new(
                    WalkthroughActionKind::Click {
                        x: *x,
                        y: *y,
                        button: *button,
                        click_count: *click_count,
                    },
                    last_app.clone(),
                    vec![event.id],
                );
                action.target_candidates = candidates;
                action.confidence = confidence;
                action.warnings = click_warnings;
                action.screenshot_meta = screenshot_meta;
                if let Some(path) = screenshot_path {
                    action.artifact_paths.push(path);
                }
                actions.push(action);
            }

            WalkthroughEventKind::KeyPressed { key, modifiers } => {
                flush_text(&mut text_buffer, &mut actions, &last_app);

                // Coalesce with previous identical PressKey if recent.
                let coalesced = if let Some(prev) = actions.last_mut() {
                    if let WalkthroughActionKind::PressKey {
                        key: ref pk,
                        modifiers: ref pm,
                    } = prev.kind
                    {
                        if pk == key
                            && pm == modifiers
                            && event.timestamp - last_key_ts <= KEY_COALESCE_GAP_MS
                        {
                            prev.source_event_ids.push(event.id);
                            true
                        } else {
                            false
                        }
                    } else {
                        false
                    }
                } else {
                    false
                };

                if !coalesced {
                    actions.push(WalkthroughAction::new(
                        WalkthroughActionKind::PressKey {
                            key: key.clone(),
                            modifiers: modifiers.clone(),
                        },
                        last_app.clone(),
                        vec![event.id],
                    ));
                }
                last_key_ts = event.timestamp;
            }

            WalkthroughEventKind::Scrolled { delta_y, .. } => {
                flush_text(&mut text_buffer, &mut actions, &last_app);

                // Coalesce with previous scroll if recent.
                let coalesced = if let Some(prev) = actions.last_mut() {
                    if let WalkthroughActionKind::Scroll {
                        delta_y: ref mut prev_dy,
                    } = prev.kind
                    {
                        if event.timestamp - last_scroll_ts <= SCROLL_COALESCE_GAP_MS {
                            *prev_dy += delta_y;
                            prev.source_event_ids.push(event.id);
                            true
                        } else {
                            false
                        }
                    } else {
                        false
                    }
                } else {
                    false
                };

                if !coalesced {
                    actions.push(WalkthroughAction::new(
                        WalkthroughActionKind::Scroll { delta_y: *delta_y },
                        last_app.clone(),
                        vec![event.id],
                    ));
                }
                last_scroll_ts = event.timestamp;
            }

            // Enrichment events are consumed by the MouseClicked lookahead above,
            // so standalone occurrences are skipped.
            WalkthroughEventKind::OcrCaptured { .. }
            | WalkthroughEventKind::ScreenshotCaptured { .. }
            | WalkthroughEventKind::AccessibilityElementCaptured { .. }
            | WalkthroughEventKind::VlmLabelResolved { .. } => {}

            // CDP click resolved events are consumed in the click peek loop above.
            WalkthroughEventKind::CdpClickResolved { .. } => {}

            // Hover events and their CDP enrichment are processed separately.
            WalkthroughEventKind::HoverDetected { .. } => {}
            WalkthroughEventKind::CdpHoverResolved { .. } => {}

            // Skip non-action events.
            WalkthroughEventKind::Paused
            | WalkthroughEventKind::Resumed
            | WalkthroughEventKind::Stopped => {}
        }
    }

    // Flush remaining text buffer.
    flush_text(&mut text_buffer, &mut actions, &last_app);

    // Per-action warnings are stored on each action and rendered inline in the
    // review UI. Only top-level (non-action-specific) warnings are returned here
    // to avoid double-counting in the warning badge and global warning strip.

    (actions, warnings)
}

// --- Draft synthesis ---

/// Vertical spacing between auto-positioned nodes (pixels in canvas coords).
const NODE_Y_SPACING: f32 = 100.0;
const NODE_X_POSITION: f32 = 250.0;

/// Synthesize a linear workflow draft from normalized walkthrough actions.
///
/// Pure function — no I/O. Produces a valid `Workflow` with linear edges.
pub fn synthesize_draft(
    actions: &[WalkthroughAction],
    workflow_id: Uuid,
    workflow_name: &str,
) -> crate::Workflow {
    use crate::{
        CdpClickParams, CdpHoverParams, ClickParams, ClickTarget, Edge, FocusMethod,
        FocusWindowParams, HoverParams, Node, NodeType, Position, PressKeyParams, ScrollParams,
        TypeTextParams, Workflow,
    };

    let mut workflow = Workflow {
        id: workflow_id,
        name: workflow_name.to_string(),
        nodes: Vec::new(),
        edges: Vec::new(),
        groups: Vec::new(),
        next_id_counters: std::collections::HashMap::new(),
    };

    let mut node_index = 0usize;
    for action in actions {
        // Skip unconfirmed candidates (e.g. hover suggestions the user hasn't kept).
        if action.candidate {
            continue;
        }
        let position = Position {
            x: NODE_X_POSITION,
            y: (node_index as f32) * NODE_Y_SPACING,
        };
        node_index += 1;

        let (node_type, name) = match &action.kind {
            WalkthroughActionKind::LaunchApp { app_name, app_kind } => (
                NodeType::FocusWindow(FocusWindowParams {
                    method: FocusMethod::AppName,
                    value: Some(app_name.clone()),
                    bring_to_front: true,
                    app_kind: *app_kind,
                    chrome_profile_id: None,
                    ..Default::default()
                }),
                format!("Launch {app_name}"),
            ),

            WalkthroughActionKind::FocusWindow {
                app_name,
                window_title,
                app_kind,
            } => (
                NodeType::FocusWindow(FocusWindowParams {
                    method: FocusMethod::AppName,
                    value: Some(app_name.clone()),
                    bring_to_front: true,
                    app_kind: *app_kind,
                    chrome_profile_id: None,
                    ..Default::default()
                }),
                match window_title {
                    Some(t) => format!("Focus '{t}'"),
                    None => format!("Focus {app_name}"),
                },
            ),

            WalkthroughActionKind::Click {
                x,
                y,
                button,
                click_count,
            } => {
                // Window control target — highest priority, resolved at execution time.
                if let Some(wc_action) = action.target_candidates.iter().find_map(|c| match c {
                    TargetCandidate::WindowControl { action } => Some(*action),
                    _ => None,
                }) {
                    let name = wc_action.display_name().to_string();
                    let params = ClickParams {
                        target: Some(ClickTarget::WindowControl { action: wc_action }),
                        button: *button,
                        click_count: *click_count,
                        ..Default::default()
                    };
                    (NodeType::Click(params), name)
                } else {
                    // Check for CDP element candidate first (structured target).
                    let cdp_candidate = action.target_candidates.iter().find_map(|c| match c {
                        TargetCandidate::CdpElement { name, .. } => Some(name),
                        _ => None,
                    });

                    // Use the best text target candidate.
                    let best_target = action
                        .target_candidates
                        .iter()
                        .find_map(|c| c.preferred_label().map(|s| s.to_string()));

                    if let Some(cdp_name) = cdp_candidate {
                        (
                            NodeType::CdpClick(CdpClickParams {
                                uid: cdp_name.clone(),
                                ..Default::default()
                            }),
                            format!("Click '{cdp_name}'"),
                        )
                    } else if let Some(ref target) = best_target {
                        (
                            NodeType::Click(ClickParams {
                                target: Some(ClickTarget::Text {
                                    text: target.clone(),
                                }),
                                button: *button,
                                click_count: *click_count,
                                ..Default::default()
                            }),
                            format!("Click '{target}'"),
                        )
                    } else {
                        (
                            NodeType::Click(ClickParams {
                                target: Some(ClickTarget::Coordinates { x: *x, y: *y }),
                                button: *button,
                                click_count: *click_count,
                                ..Default::default()
                            }),
                            format!("Click ({x:.0}, {y:.0})"),
                        )
                    }
                }
            }

            WalkthroughActionKind::TypeText { text } => {
                let display = if text.chars().count() > 20 {
                    let truncated: String = text.chars().take(20).collect();
                    format!("Type '{truncated}'...")
                } else {
                    format!("Type '{text}'")
                };
                (
                    NodeType::TypeText(TypeTextParams {
                        text: text.clone(),
                        ..Default::default()
                    }),
                    display,
                )
            }

            WalkthroughActionKind::PressKey { key, modifiers } => {
                let name = WindowControl::from_shortcut(key, modifiers)
                    .map(|wc| wc.display_name().to_string())
                    .or_else(|| shortcut_display_name(key, modifiers))
                    .unwrap_or_else(|| {
                        if modifiers.is_empty() {
                            format!("Press {key}")
                        } else {
                            format!("Press {}+{key}", modifiers.join("+"))
                        }
                    });
                (
                    NodeType::PressKey(PressKeyParams {
                        key: key.clone(),
                        modifiers: modifiers.clone(),
                        ..Default::default()
                    }),
                    name,
                )
            }

            WalkthroughActionKind::Scroll { delta_y } => (
                NodeType::Scroll(ScrollParams {
                    delta_y: *delta_y as i32,
                    x: None,
                    y: None,
                    ..Default::default()
                }),
                format!("Scroll {}", if *delta_y < 0.0 { "up" } else { "down" }),
            ),

            WalkthroughActionKind::Hover { x, y, dwell_ms } => {
                // Same target resolution logic as Click: CDP > text > coordinates
                let cdp_candidate = action.target_candidates.iter().find_map(|c| match c {
                    TargetCandidate::CdpElement { name, .. } => Some(name),
                    _ => None,
                });

                let best_target = action
                    .target_candidates
                    .iter()
                    .find_map(|c| c.preferred_label().map(|s| s.to_string()));

                let (node_type_out, name) = if let Some(cdp_name) = cdp_candidate {
                    (
                        NodeType::CdpHover(CdpHoverParams {
                            uid: cdp_name.clone(),
                            ..Default::default()
                        }),
                        format!("Hover '{cdp_name}'"),
                    )
                } else if let Some(ref target) = best_target {
                    (
                        NodeType::Hover(HoverParams {
                            target: Some(ClickTarget::Text {
                                text: target.clone(),
                            }),
                            dwell_ms: *dwell_ms,
                            ..Default::default()
                        }),
                        format!("Hover '{target}'"),
                    )
                } else {
                    (
                        NodeType::Hover(HoverParams {
                            target: Some(ClickTarget::Coordinates { x: *x, y: *y }),
                            dwell_ms: *dwell_ms,
                            ..Default::default()
                        }),
                        format!("Hover ({x:.0}, {y:.0})"),
                    )
                };
                (node_type_out, name)
            }
        };

        let auto_id = crate::auto_id::assign_auto_id(&node_type, &mut workflow.next_id_counters);
        let node = Node::new(node_type, position, name, auto_id);
        workflow.nodes.push(node);
    }

    // Wire linear edges.
    for i in 0..workflow.nodes.len().saturating_sub(1) {
        let from = workflow.nodes[i].id;
        let to = workflow.nodes[i + 1].id;
        workflow.edges.push(Edge {
            from,
            to,
            output: None,
        });
    }

    workflow
}

#[cfg(test)]
mod tests {
    use super::super::types::*;
    use super::*;
    use crate::{MouseButton, WindowControlAction};

    #[test]
    fn test_walkthrough_status_default_is_idle() {
        assert_eq!(WalkthroughStatus::default(), WalkthroughStatus::Idle);
    }

    #[test]
    fn test_session_serialization_skips_events_and_actions() {
        let session = WalkthroughSession {
            id: Uuid::new_v4(),
            workflow_id: Uuid::new_v4(),
            started_at: 1_700_000_000_000,
            ended_at: None,
            status: WalkthroughStatus::Recording,
            events: vec![WalkthroughEvent {
                id: Uuid::new_v4(),
                timestamp: 1_700_000_000_100,
                kind: WalkthroughEventKind::Paused,
            }],
            actions: vec![],
            warnings: vec![],
        };

        let json = serde_json::to_string(&session).expect("serialize");
        assert!(!json.contains("Paused"));

        let deserialized: WalkthroughSession = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(deserialized.id, session.id);
        assert_eq!(deserialized.status, WalkthroughStatus::Recording);
        assert!(deserialized.events.is_empty());
        assert!(deserialized.actions.is_empty());
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
        let json =
            r#"{"type":"AppFocused","app_name":"Calculator","pid":1234,"window_title":null}"#;
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

        let session = WalkthroughSession {
            id: Uuid::new_v4(),
            workflow_id: Uuid::new_v4(),
            started_at: 1_700_000_000_000,
            ended_at: None,
            status: WalkthroughStatus::Recording,
            events: vec![],
            actions: vec![],
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

        let session = WalkthroughSession {
            id: Uuid::new_v4(),
            workflow_id: Uuid::new_v4(),
            started_at: 1_700_000_000_000,
            ended_at: None,
            status: WalkthroughStatus::Recording,
            events: vec![],
            actions: vec![],
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
        let session = WalkthroughSession::new(Uuid::new_v4());
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

    #[test]
    fn test_build_action_node_map_1_to_1() {
        let actions = vec![
            WalkthroughAction {
                id: Uuid::new_v4(),
                kind: WalkthroughActionKind::Click {
                    x: 0.0,
                    y: 0.0,
                    button: MouseButton::Left,
                    click_count: 1,
                },
                app_name: None,
                window_title: None,
                target_candidates: vec![],
                artifact_paths: vec![],
                source_event_ids: vec![],
                confidence: ActionConfidence::High,
                warnings: vec![],
                screenshot_meta: None,
                candidate: false,
            },
            WalkthroughAction {
                id: Uuid::new_v4(),
                kind: WalkthroughActionKind::TypeText {
                    text: "hello".into(),
                },
                app_name: None,
                window_title: None,
                target_candidates: vec![],
                artifact_paths: vec![],
                source_event_ids: vec![],
                confidence: ActionConfidence::High,
                warnings: vec![],
                screenshot_meta: None,
                candidate: false,
            },
        ];
        let draft = synthesize_draft(&actions, Uuid::new_v4(), "test");
        let map = build_action_node_map(&actions, &draft);

        assert_eq!(map.len(), 2);
        assert_eq!(map[0].action_id, actions[0].id);
        assert_eq!(map[0].node_id, draft.nodes[0].id);
        assert_eq!(map[1].action_id, actions[1].id);
        assert_eq!(map[1].node_id, draft.nodes[1].id);
    }

    #[test]
    fn test_build_action_node_map_empty() {
        let map = build_action_node_map(&[], &crate::Workflow::default());
        assert!(map.is_empty());
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

    mod synthesis_tests {
        use super::*;
        use crate::{FocusMethod, NodeType};

        fn make_action(kind: WalkthroughActionKind) -> WalkthroughAction {
            WalkthroughAction {
                id: Uuid::new_v4(),
                kind,
                app_name: None,
                window_title: None,
                target_candidates: vec![],
                artifact_paths: vec![],
                source_event_ids: vec![],
                confidence: ActionConfidence::High,
                warnings: vec![],
                screenshot_meta: None,
                candidate: false,
            }
        }

        #[test]
        fn test_empty_actions_produces_empty_workflow() {
            let wf = synthesize_draft(&[], Uuid::new_v4(), "Test");
            assert!(wf.nodes.is_empty());
            assert!(wf.edges.is_empty());
        }

        #[test]
        fn test_launch_app_becomes_focus_window_node() {
            let actions = vec![make_action(WalkthroughActionKind::LaunchApp {
                app_name: "Calculator".into(),
                app_kind: AppKind::Native,
            })];
            let wf = synthesize_draft(&actions, Uuid::new_v4(), "Test");
            assert_eq!(wf.nodes.len(), 1);
            assert!(matches!(
                &wf.nodes[0].node_type,
                NodeType::FocusWindow(p) if p.method == FocusMethod::AppName && p.value.as_deref() == Some("Calculator")
            ));
            assert_eq!(wf.nodes[0].name, "Launch Calculator");
        }

        #[test]
        fn test_app_kind_propagated_to_focus_window_node() {
            let actions = vec![make_action(WalkthroughActionKind::LaunchApp {
                app_name: "Discord".into(),
                app_kind: AppKind::ElectronApp,
            })];
            let wf = synthesize_draft(&actions, Uuid::new_v4(), "Test");
            assert_eq!(wf.nodes.len(), 1);
            match &wf.nodes[0].node_type {
                NodeType::FocusWindow(p) => {
                    assert_eq!(p.app_kind, AppKind::ElectronApp);
                }
                _ => panic!("expected FocusWindow"),
            }
        }

        #[test]
        fn test_click_with_ocr_target_uses_target_field() {
            let mut action = make_action(WalkthroughActionKind::Click {
                x: 100.0,
                y: 200.0,
                button: MouseButton::Left,
                click_count: 1,
            });
            action.target_candidates = vec![
                TargetCandidate::OcrText {
                    text: "Submit".into(),
                },
                TargetCandidate::Coordinates { x: 100.0, y: 200.0 },
            ];
            let wf = synthesize_draft(&[action], Uuid::new_v4(), "Test");
            assert_eq!(wf.nodes.len(), 1);
            assert!(matches!(
                &wf.nodes[0].node_type,
                NodeType::Click(p) if p.target.as_ref().map(|t| t.text()) == Some("Submit")
            ));
            assert_eq!(wf.nodes[0].name, "Click 'Submit'");
        }

        #[test]
        fn test_click_coordinates_only() {
            let action = make_action(WalkthroughActionKind::Click {
                x: 100.0,
                y: 200.0,
                button: MouseButton::Left,
                click_count: 1,
            });
            let wf = synthesize_draft(&[action], Uuid::new_v4(), "Test");
            assert!(matches!(
                &wf.nodes[0].node_type,
                NodeType::Click(p) if matches!(&p.target, Some(crate::ClickTarget::Coordinates { x, .. }) if (*x - 100.0).abs() < f64::EPSILON)
            ));
        }

        #[test]
        fn synthesize_draft_cdp_element_produces_cdp_click() {
            let mut action = make_action(WalkthroughActionKind::Click {
                x: 100.0,
                y: 200.0,
                button: MouseButton::Left,
                click_count: 1,
            });
            action.target_candidates = vec![
                TargetCandidate::CdpElement {
                    name: "Friends".into(),
                    role: Some("link".into()),
                    href: Some("https://discord.com/friends".into()),
                    parent_role: None,
                    parent_name: None,
                },
                TargetCandidate::Coordinates { x: 100.0, y: 200.0 },
            ];
            let launch = make_action(WalkthroughActionKind::LaunchApp {
                app_name: "Discord".into(),
                app_kind: AppKind::ElectronApp,
            });
            let draft = synthesize_draft(&[launch, action], Uuid::new_v4(), "test");
            let click_node = &draft.nodes[1];
            match &click_node.node_type {
                NodeType::CdpClick(p) => {
                    assert_eq!(p.uid, "Friends");
                }
                other => panic!("expected CdpClick, got {:?}", other),
            }
        }

        #[test]
        fn test_linear_edges_between_nodes() {
            let actions = vec![
                make_action(WalkthroughActionKind::LaunchApp {
                    app_name: "App".into(),
                    app_kind: AppKind::Native,
                }),
                make_action(WalkthroughActionKind::TypeText {
                    text: "hello".into(),
                }),
                make_action(WalkthroughActionKind::PressKey {
                    key: "return".into(),
                    modifiers: vec![],
                }),
            ];
            let wf = synthesize_draft(&actions, Uuid::new_v4(), "Test");
            assert_eq!(wf.nodes.len(), 3);
            assert_eq!(wf.edges.len(), 2);
            assert_eq!(wf.edges[0].from, wf.nodes[0].id);
            assert_eq!(wf.edges[0].to, wf.nodes[1].id);
            assert_eq!(wf.edges[1].from, wf.nodes[1].id);
            assert_eq!(wf.edges[1].to, wf.nodes[2].id);
        }

        #[test]
        fn test_nodes_auto_positioned_vertically() {
            let actions = vec![
                make_action(WalkthroughActionKind::TypeText { text: "a".into() }),
                make_action(WalkthroughActionKind::TypeText { text: "b".into() }),
            ];
            let wf = synthesize_draft(&actions, Uuid::new_v4(), "Test");
            assert!(wf.nodes[1].position.y > wf.nodes[0].position.y);
        }

        #[test]
        fn test_synthesize_hover_action() {
            let action = WalkthroughAction {
                id: Uuid::new_v4(),
                kind: WalkthroughActionKind::Hover {
                    x: 100.0,
                    y: 200.0,
                    dwell_ms: 800,
                },
                app_name: Some("Calculator".into()),
                window_title: None,
                target_candidates: vec![TargetCandidate::AccessibilityLabel {
                    label: "Edit".into(),
                    role: Some("AXMenuItem".into()),
                }],
                artifact_paths: vec![],
                source_event_ids: vec![],
                confidence: ActionConfidence::High,
                warnings: vec![],
                screenshot_meta: None,
                candidate: false,
            };
            let wf = synthesize_draft(&[action], Uuid::new_v4(), "test");
            assert_eq!(wf.nodes.len(), 1);
            match &wf.nodes[0].node_type {
                crate::NodeType::Hover(p) => {
                    assert!(p.target.is_some());
                    assert_eq!(p.target.as_ref().unwrap().text(), "Edit");
                    assert_eq!(p.dwell_ms, 800);
                }
                other => panic!("Expected Hover, got {:?}", other),
            }
        }

        #[test]
        fn test_synthesize_skips_candidate_actions() {
            let actions = vec![
                WalkthroughAction {
                    id: Uuid::new_v4(),
                    kind: WalkthroughActionKind::Hover {
                        x: 100.0,
                        y: 200.0,
                        dwell_ms: 800,
                    },
                    app_name: None,
                    window_title: None,
                    target_candidates: vec![],
                    artifact_paths: vec![],
                    source_event_ids: vec![],
                    confidence: ActionConfidence::High,
                    warnings: vec![],
                    screenshot_meta: None,
                    candidate: true, // should be skipped
                },
                WalkthroughAction {
                    id: Uuid::new_v4(),
                    kind: WalkthroughActionKind::Click {
                        x: 50.0,
                        y: 50.0,
                        button: MouseButton::Left,
                        click_count: 1,
                    },
                    app_name: None,
                    window_title: None,
                    target_candidates: vec![],
                    artifact_paths: vec![],
                    source_event_ids: vec![],
                    confidence: ActionConfidence::High,
                    warnings: vec![],
                    screenshot_meta: None,
                    candidate: false,
                },
            ];
            let wf = synthesize_draft(&actions, Uuid::new_v4(), "test");
            assert_eq!(wf.nodes.len(), 1); // only the click, not the candidate hover
            assert!(matches!(wf.nodes[0].node_type, crate::NodeType::Click(_)));
        }

        #[test]
        fn test_scroll_delta_cast_to_i32() {
            let action = make_action(WalkthroughActionKind::Scroll { delta_y: -5.7 });
            let wf = synthesize_draft(&[action], Uuid::new_v4(), "Test");
            assert!(matches!(&wf.nodes[0].node_type, NodeType::Scroll(p) if p.delta_y == -5));
        }

        #[test]
        fn test_window_control_synthesizes_semantic_node_name() {
            // Cmd+W is "Close tab", not "Close window".
            let action = make_action(WalkthroughActionKind::PressKey {
                key: "w".to_string(),
                modifiers: vec!["command".to_string()],
            });
            let wf = synthesize_draft(&[action], Uuid::new_v4(), "Test");
            assert_eq!(wf.nodes[0].name, "Close tab");

            let action = make_action(WalkthroughActionKind::PressKey {
                key: "m".to_string(),
                modifiers: vec!["command".to_string()],
            });
            let wf = synthesize_draft(&[action], Uuid::new_v4(), "Test");
            assert_eq!(wf.nodes[0].name, "Minimize window");

            let action = make_action(WalkthroughActionKind::PressKey {
                key: "f".to_string(),
                modifiers: vec!["command".to_string(), "control".to_string()],
            });
            let wf = synthesize_draft(&[action], Uuid::new_v4(), "Test");
            assert_eq!(wf.nodes[0].name, "Maximize window");
        }

        #[test]
        fn test_close_window_control_synthesizes_click_node() {
            // Close buttons produce Click actions with WindowControl target candidate.
            let mut action = make_action(WalkthroughActionKind::Click {
                x: 14.0,
                y: 14.0,
                button: MouseButton::Left,
                click_count: 1,
            });
            action.target_candidates = vec![TargetCandidate::WindowControl {
                action: WindowControlAction::Close,
            }];
            let wf = synthesize_draft(&[action], Uuid::new_v4(), "Test");
            assert_eq!(wf.nodes[0].name, "Close window");
            assert!(matches!(
                &wf.nodes[0].node_type,
                NodeType::Click(p) if matches!(
                    &p.target,
                    Some(crate::ClickTarget::WindowControl { action: WindowControlAction::Close })
                )
            ));
        }

        #[test]
        fn test_regular_press_key_name_unchanged() {
            let action = make_action(WalkthroughActionKind::PressKey {
                key: "return".to_string(),
                modifiers: vec![],
            });
            let wf = synthesize_draft(&[action], Uuid::new_v4(), "Test");
            assert_eq!(wf.nodes[0].name, "Press return");
        }
    }
}
