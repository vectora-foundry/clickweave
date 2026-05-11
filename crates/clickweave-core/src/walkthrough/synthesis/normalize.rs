use super::*;

// --- Event normalization ---

/// Normalize raw walkthrough events into semantic actions.
///
/// Returns `(actions, warnings)`. Pure function — no I/O.
pub fn normalize_events(events: &[WalkthroughEvent]) -> (Vec<WalkthroughAction>, Vec<String>) {
    let sorted = sorted_events(events);
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
                    update_existing_focus_kind(&mut actions, app_name, *app_kind);
                    continue;
                }

                let is_new = seen_apps.insert(app_name.clone());
                actions.push(focus_action(
                    event.id,
                    app_name,
                    window_title,
                    *app_kind,
                    is_new,
                ));
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

                let (lookahead, peek) = collect_click_lookahead(events, i);
                // Advance past consumed enrichment events.
                i = peek;

                // Window control buttons (close, minimize, maximize/zoom):
                // - Close/Zoom → window-relative click (no reliable shortcut)
                // - Minimize → Cmd+M, Maximize (full screen) → Ctrl+Cmd+F
                if let Some(action) =
                    window_control_action(event.id, *x, *y, lookahead.ax_label.as_ref(), &last_app)
                {
                    actions.push(action);
                    continue;
                }

                actions.push(click_action_from_lookahead(
                    event.id,
                    *x,
                    *y,
                    *button,
                    *click_count,
                    last_app.clone(),
                    lookahead,
                ));
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

fn sorted_events(events: &[WalkthroughEvent]) -> Vec<WalkthroughEvent> {
    // Sort by timestamp so each click is followed by its enrichment events.
    // Background enrichment tasks append events out-of-order (after later
    // clicks), but reuse the original click's timestamp. Stable sort keeps
    // the click before its enrichment within the same timestamp.
    let mut sorted = events.to_vec();
    sorted.sort_by_key(|e| e.timestamp);
    sorted
}

fn update_existing_focus_kind(
    actions: &mut [WalkthroughAction],
    app_name: &str,
    app_kind: crate::AppKind,
) {
    // Search backward for the most recent focus/launch action for this app.
    // It may not be the very last action because clicks can sit between the
    // original focus and a later reactive app-kind correction.
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
        if prev_name == app_name && *prev_kind != app_kind {
            *prev_kind = app_kind;
            break;
        }
    }
}

fn focus_action(
    event_id: Uuid,
    app_name: &str,
    window_title: &Option<String>,
    app_kind: crate::AppKind,
    is_new: bool,
) -> WalkthroughAction {
    let kind = if is_new {
        WalkthroughActionKind::LaunchApp {
            app_name: app_name.to_string(),
            app_kind,
        }
    } else {
        WalkthroughActionKind::FocusWindow {
            app_name: app_name.to_string(),
            window_title: window_title.clone(),
            app_kind,
        }
    };

    let mut action = WalkthroughAction::new(kind, Some(app_name.to_string()), vec![event_id]);
    action.window_title = window_title.clone();
    action
}

#[derive(Default)]
struct ClickLookahead<'a> {
    screenshot_path: Option<String>,
    screenshot_meta: Option<ScreenshotMeta>,
    ocr_annotations: Option<&'a Vec<super::super::types::OcrAnnotation>>,
    ax_label: Option<(String, Option<String>, Option<String>)>,
    vlm_label: Option<String>,
    crop_candidate: Option<(String, String)>,
    cdp_resolved: Option<CdpElementData>,
}

fn collect_click_lookahead<'a>(
    events: &'a [WalkthroughEvent],
    start: usize,
) -> (ClickLookahead<'a>, usize) {
    let mut lookahead = ClickLookahead::default();
    let mut peek = start;
    while peek < events.len() {
        match &events[peek].kind {
            WalkthroughEventKind::ScreenshotCaptured {
                path,
                kind: ScreenshotKind::ClickCrop,
                image_b64: Some(b64),
                ..
            } => {
                lookahead.crop_candidate = Some((path.clone(), b64.clone()));
            }
            WalkthroughEventKind::ScreenshotCaptured { path, meta, .. } => {
                lookahead.screenshot_path = Some(path.clone());
                lookahead.screenshot_meta = *meta;
            }
            WalkthroughEventKind::OcrCaptured { annotations, .. } => {
                lookahead.ocr_annotations = Some(annotations);
            }
            WalkthroughEventKind::AccessibilityElementCaptured {
                label,
                role,
                subrole,
            } => {
                lookahead.ax_label = Some((label.clone(), role.clone(), subrole.clone()));
            }
            WalkthroughEventKind::VlmLabelResolved { label } => {
                lookahead.vlm_label = Some(label.clone());
            }
            WalkthroughEventKind::CdpClickResolved {
                name,
                role,
                href,
                parent_role,
                parent_name,
                ..
            } => {
                lookahead.cdp_resolved = Some(CdpElementData {
                    name: name.clone(),
                    role: role.clone(),
                    href: href.clone(),
                    parent_role: parent_role.clone(),
                    parent_name: parent_name.clone(),
                });
            }
            _ => break,
        }
        peek += 1;
    }
    (lookahead, peek)
}

fn window_control_action(
    event_id: Uuid,
    x: f64,
    y: f64,
    ax_label: Option<&(String, Option<String>, Option<String>)>,
    last_app: &Option<String>,
) -> Option<WalkthroughAction> {
    let (label, role, subrole) = ax_label?;
    let wc = WindowControl::from_accessibility(label, role.as_deref(), subrole.as_deref())?;

    if let Some((key, modifiers)) = wc.shortcut() {
        return Some(WalkthroughAction::new(
            WalkthroughActionKind::PressKey {
                key: key.to_string(),
                modifiers,
            },
            last_app.clone(),
            vec![event_id],
        ));
    }

    // Emit a Click action with a WindowControl target candidate. The executor
    // resolves this to window-relative coordinates.
    let mut action = WalkthroughAction::new(
        WalkthroughActionKind::Click {
            x,
            y,
            button: MouseButton::Left,
            click_count: 1,
        },
        last_app.clone(),
        vec![event_id],
    );
    action.target_candidates = vec![TargetCandidate::WindowControl {
        action: wc.to_action(),
    }];
    action.confidence = ActionConfidence::High;
    Some(action)
}

fn click_action_from_lookahead(
    event_id: Uuid,
    x: f64,
    y: f64,
    button: MouseButton,
    click_count: u32,
    last_app: Option<String>,
    lookahead: ClickLookahead<'_>,
) -> WalkthroughAction {
    let ClickLookahead {
        screenshot_path,
        screenshot_meta,
        ocr_annotations,
        ax_label,
        vlm_label,
        crop_candidate,
        cdp_resolved,
    } = lookahead;

    let ax_for_candidates = ax_label
        .as_ref()
        .map(|(label, role, _)| (label.clone(), role.clone()));
    let ax_dispatch_resolved = ax_dispatch_candidate(&ax_label, cdp_resolved.is_some());
    let enrichment = ClickEnrichment {
        ax_label: ax_for_candidates,
        vlm_label,
        ocr_annotations,
        crop_candidate,
        cdp_resolved,
        ax_resolved: ax_dispatch_resolved,
        click_x: x,
        click_y: y,
    };

    let candidates = build_target_candidates(&enrichment);
    let confidence = score_confidence(&candidates);

    let mut action = WalkthroughAction::new(
        WalkthroughActionKind::Click {
            x,
            y,
            button,
            click_count,
        },
        last_app,
        vec![event_id],
    );
    action.target_candidates = candidates;
    action.confidence = confidence;
    if confidence == ActionConfidence::Low {
        action.warnings.push(format!(
            "No text target found for click at ({x:.0}, {y:.0}) — using coordinates"
        ));
    }
    action.screenshot_meta = screenshot_meta;
    if let Some(path) = screenshot_path {
        action.artifact_paths.push(path);
    }
    action
}

fn ax_dispatch_candidate(
    ax_label: &Option<(String, Option<String>, Option<String>)>,
    has_cdp_resolution: bool,
) -> Option<AxElementData> {
    // Promote the ax_label to an AX dispatch descriptor when the role is
    // actionable AND CDP is not in play. A JS-resolved element is a strong
    // signal that the target belongs to a web view, not a native AX tree.
    if has_cdp_resolution {
        return None;
    }
    let Some((label, Some(role), _)) = ax_label else {
        return None;
    };
    if label.is_empty() || !super::super::types::is_actionable_ax_role(Some(role.as_str())) {
        return None;
    }
    Some(AxElementData {
        role: role.clone(),
        name: label.clone(),
        parent_name: None,
    })
}
