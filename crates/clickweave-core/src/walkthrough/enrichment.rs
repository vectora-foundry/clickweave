use serde::{Deserialize, Serialize};

use super::types::{ScreenshotMeta, WalkthroughAction, WalkthroughActionKind, WalkthroughEvent};

/// A single frame from continuous screen recording (returned by `stop_recording`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecordedFrame {
    pub timestamp_ms: u64,
    pub path: String,
    pub app_name: String,
    pub window_id: u32,
    pub origin_x: f64,
    pub origin_y: f64,
    pub scale: f64,
    pub pixel_width: u32,
    pub pixel_height: u32,
}

/// Parsed accessibility data from `element_at_point`.
pub struct AccessibilityData {
    pub label: String,
    pub role: Option<String>,
    pub subrole: Option<String>,
}

/// Parse accessibility data from a JSON value (the result of `element_at_point`).
///
/// Picks the best display text from the response fields:
/// `name` (AXTitle) > `value` (AXValue) > `label` (AXDescription).
///
/// Returns `None` only if no display text AND no subrole are present.
/// Window control buttons (close/minimize/zoom) may lack text labels
/// but always have a subrole set by the macOS window server.
pub fn parse_accessibility_json(obj: &serde_json::Value) -> Option<AccessibilityData> {
    let label = obj["name"]
        .as_str()
        .or_else(|| obj["value"].as_str())
        .or_else(|| obj["label"].as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());
    let role = obj["role"].as_str().map(|s| s.to_string());
    let subrole = obj["subrole"].as_str().map(|s| s.to_string());

    if label.is_some() || subrole.is_some() {
        Some(AccessibilityData {
            label: label.unwrap_or_default(),
            role,
            subrole,
        })
    } else {
        None
    }
}

/// Parse screenshot metadata (origin, scale) from a JSON value.
pub fn parse_screenshot_metadata_json(obj: &serde_json::Value) -> Option<ScreenshotMeta> {
    Some(ScreenshotMeta {
        origin_x: obj["screenshot_origin_x"].as_f64()?,
        origin_y: obj["screenshot_origin_y"].as_f64()?,
        scale: obj["screenshot_scale"].as_f64()?,
    })
}

/// Find the frames immediately before and after the given timestamp.
///
/// Returns `(before, after)` where:
/// - `before` is the last frame with `timestamp_ms < timestamp`
/// - `after` is the first frame with `timestamp_ms >= timestamp`
///
/// Frames must be sorted by `timestamp_ms` (guaranteed by `parse_recording_frames`).
/// Uses binary search for O(log n) lookup.
pub fn find_surrounding_frames(
    frames: &[RecordedFrame],
    timestamp_ms: u64,
) -> (Option<&RecordedFrame>, Option<&RecordedFrame>) {
    if frames.is_empty() {
        return (None, None);
    }
    let idx = frames.partition_point(|f| f.timestamp_ms < timestamp_ms);
    let before = if idx > 0 {
        Some(&frames[idx - 1])
    } else {
        None
    };
    let after = frames.get(idx);
    (before, after)
}

/// Attach before/after recording frames to hover actions.
///
/// For each Hover action, computes the hover start time (`timestamp - dwell_ms`)
/// and finds the frames immediately before and after that point. The before
/// frame (element unobscured) is used by VLM for target identification; both
/// frames appear in the review panel so the user can see the hover's visual
/// effect (tooltips, highlights, etc.).
///
/// `artifact_paths` is set to `[before_path, after_path]` when both exist,
/// or a single path when only one is available. Click actions are skipped.
pub fn attach_recording_frames(
    actions: &mut [WalkthroughAction],
    frames: &[RecordedFrame],
    events: &[WalkthroughEvent],
) {
    if frames.is_empty() {
        return;
    }

    for action in actions.iter_mut() {
        if !matches!(action.kind, WalkthroughActionKind::Hover { .. }) {
            continue;
        }
        if !action.artifact_paths.is_empty() {
            continue;
        }

        // The event timestamp is when the hover started (cursor arrived at
        // the element) for both native and CDP hovers:
        // - Native: MCP fires a transition event with timestamp_ms = arrival time
        // - CDP: JS listener stores ts = Date.now() at element enter
        let hover_start_ts = action
            .source_event_ids
            .first()
            .and_then(|id| events.iter().find(|e| e.id == *id))
            .map(|e| e.timestamp)
            .unwrap_or(0);

        // Prefer frames from the same app (recording captures per-app
        // windows). Fall back to all frames if no app-specific match.
        let app_frames: Vec<RecordedFrame> = if let Some(app) = &action.app_name {
            frames
                .iter()
                .filter(|f| f.app_name == *app)
                .cloned()
                .collect()
        } else {
            vec![]
        };
        let search_frames = if app_frames.is_empty() {
            frames
        } else {
            &app_frames
        };
        let (before, after) = find_surrounding_frames(search_frames, hover_start_ts);

        // Use the before frame's metadata for coordinate mapping — VLM and
        // crosshair drawing operate on artifact_paths[0] (the before frame).
        // Fall back to the after frame if no before exists.
        let meta_frame = before.or(after);
        if let Some(f) = meta_frame
            && f.scale > 0.0
        {
            action.screenshot_meta = Some(ScreenshotMeta {
                origin_x: f.origin_x,
                origin_y: f.origin_y,
                scale: f.scale,
            });
        }

        match (before, after) {
            (Some(b), Some(a)) => {
                action.artifact_paths = vec![b.path.clone(), a.path.clone()];
            }
            (Some(b), None) => {
                action.artifact_paths = vec![b.path.clone()];
            }
            (None, Some(a)) => {
                action.artifact_paths = vec![a.path.clone()];
            }
            (None, None) => {}
        }
    }
}

/// Build a VLM prompt for identifying a click/hover target on a screenshot.
///
/// Returns the complete prompt string with context hints from accessibility data,
/// OCR text, and app name when available.
pub fn build_vlm_click_prompt(
    ax_label: Option<(&str, Option<&str>)>,
    ocr_text: Option<&str>,
    app_name: Option<&str>,
) -> String {
    let mut prompt = String::from(
        "This is a screenshot of an application window with a red \
         crosshair marking where the user clicked. What UI element is at \
         the crosshair?",
    );

    let mut hints = Vec::new();
    if let Some(app) = app_name {
        hints.push(format!("Application: {app}"));
    }
    if let Some((label, role)) = ax_label {
        let role_str = role.unwrap_or("unknown");
        hints.push(format!(
            "Accessibility element: \"{label}\" (role: {role_str})"
        ));
    }
    if let Some(text) = ocr_text {
        hints.push(format!("Nearby text (OCR): \"{text}\""));
    }
    if !hints.is_empty() {
        prompt.push_str("\n\nContext hints (may be incomplete):\n");
        for hint in &hints {
            prompt.push_str(&format!("- {hint}\n"));
        }
    }

    prompt.push_str(
        "\nReturn ONLY the text label or name of the element \
         (e.g., \"Send\", \"Note to Self\", \"Search\"). If there's no text \
         label, describe the element briefly (e.g., \"message input field\"). \
         Return just the label, nothing else.",
    );

    prompt
}

/// Maximum length of a VLM-resolved label to accept. Longer responses
/// are likely full sentences rather than a concise element name.
pub const VLM_LABEL_MAX_LEN: usize = 80;

/// Validate and clean a raw VLM response label.
///
/// Returns `Some(label)` if the label is non-empty and within the max length,
/// `None` otherwise.
pub fn clean_vlm_label(raw: &str) -> Option<String> {
    let label = raw.trim().trim_matches('"').to_string();
    if !label.is_empty() && label.len() <= VLM_LABEL_MAX_LEN {
        Some(label)
    } else {
        None
    }
}

/// Parse a CDP click element response JSON into its named fields.
///
/// Returns `(name, role, href, parent_role, parent_name)` or `None` if the
/// data cannot be parsed into a meaningful element name.
#[allow(clippy::type_complexity)]
pub fn parse_cdp_click_data(
    parsed: &serde_json::Value,
) -> Option<(
    String,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
)> {
    // Build name from ariaLabel, textContent, value, or title.
    let text_name = parsed["ariaLabel"]
        .as_str()
        .filter(|s| !s.is_empty())
        .or_else(|| parsed["textContent"].as_str().filter(|s| !s.is_empty()))
        .or_else(|| parsed["value"].as_str().filter(|s| !s.is_empty()))
        .or_else(|| parsed["title"].as_str().filter(|s| !s.is_empty()));

    // Synthesize a structural fallback when no text-based name is available.
    let name = match text_name {
        Some(n) => n.to_string(),
        None => {
            if let Some(id) = parsed["id"].as_str().filter(|s| !s.is_empty()) {
                format!("#{id}")
            } else {
                let tag = parsed["tagName"]
                    .as_str()
                    .unwrap_or("element")
                    .to_lowercase();
                match parsed["role"].as_str().filter(|s| !s.is_empty()) {
                    Some(role) => format!("{tag}[{role}]"),
                    None => tag,
                }
            }
        }
    };

    let role = parsed["role"].as_str().map(|s| s.to_string());
    let href = parsed["href"].as_str().map(|s| s.to_string());
    let parent_role = parsed["parentRole"].as_str().map(|s| s.to_string());
    let parent_name = parsed["parentName"].as_str().map(|s| s.to_string());

    Some((name, role, href, parent_role, parent_name))
}

/// Parse a CDP hover entry from the JS hover listener into walkthrough fields.
///
/// Returns `(label, role, href, parent_role, parent_name, ts, dwell_ms, x, y)`
/// or `None` if the entry lacks a usable label.
#[allow(clippy::type_complexity)]
pub fn parse_cdp_hover_entry(
    entry: &serde_json::Value,
) -> Option<(
    String,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    u64,
    u64,
    f64,
    f64,
)> {
    let label = entry["textContent"]
        .as_str()
        .or_else(|| entry["ariaLabel"].as_str())
        .filter(|s| !s.is_empty())?
        .to_string();

    let ts = entry["ts"].as_u64().unwrap_or(0);
    let dwell_ms = entry["dwellMs"].as_u64().unwrap_or(0);
    let x = entry["x"].as_f64().unwrap_or(0.0);
    let y = entry["y"].as_f64().unwrap_or(0.0);
    let role = entry["role"].as_str().map(|s| s.to_string());
    let href = entry["href"].as_str().map(|s| s.to_string());
    let parent_role = entry["parentRole"].as_str().map(|s| s.to_string());
    let parent_name = entry["parentName"].as_str().map(|s| s.to_string());

    Some((
        label,
        role,
        href,
        parent_role,
        parent_name,
        ts,
        dwell_ms,
        x,
        y,
    ))
}

/// Parse native hover tracking response entries from MCP.
///
/// Extracts hover fields from a JSON value. Returns `None` for timeout sentinel
/// entries or entries that can't be parsed.
#[allow(clippy::type_complexity)]
pub fn parse_native_hover_entry(
    ev: &serde_json::Value,
) -> Option<(f64, f64, String, Option<String>, u64, u64, Option<String>)> {
    // Skip timeout sentinel events.
    if ev.get("timeout").and_then(|v| v.as_bool()) == Some(true) {
        return None;
    }

    let x = ev
        .pointer("/cursor/x")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    let y = ev
        .pointer("/cursor/y")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    let element_name = ev
        .pointer("/element/name")
        .and_then(|v| v.as_str())
        .or_else(|| ev.pointer("/element/label").and_then(|v| v.as_str()))
        .unwrap_or("")
        .to_string();
    let element_role = ev
        .pointer("/element/role")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let dwell_ms = ev.get("dwell_ms").and_then(|v| v.as_u64()).unwrap_or(0);
    let timestamp_ms = ev.get("timestamp_ms").and_then(|v| v.as_u64()).unwrap_or(0);
    let app_name = ev
        .pointer("/element/app_name")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    Some((
        x,
        y,
        element_name,
        element_role,
        dwell_ms,
        timestamp_ms,
        app_name,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use clickweave_core_test_imports::*;

    // Bring in test-only re-exports of private types from the parent module
    mod clickweave_core_test_imports {
        pub use crate::MouseButton;
        pub use crate::walkthrough::types::{
            ActionConfidence, ScreenshotMeta, WalkthroughAction, WalkthroughActionKind,
            WalkthroughEvent, WalkthroughEventKind,
        };
        pub use uuid::Uuid;
    }

    fn frame(ts: u64) -> RecordedFrame {
        frame_for_app(ts, "TestApp")
    }

    fn frame_for_app(ts: u64, app: &str) -> RecordedFrame {
        RecordedFrame {
            timestamp_ms: ts,
            path: format!("/frames/frame_{ts}.png"),
            app_name: app.to_string(),
            window_id: 1,
            origin_x: 10.0,
            origin_y: 20.0,
            scale: 2.0,
            pixel_width: 1920,
            pixel_height: 1080,
        }
    }

    // --- find_surrounding_frames tests ---

    #[test]
    fn surrounding_frames_between_two() {
        let frames = vec![frame(1000), frame(2000), frame(3000)];
        let (before, after) = find_surrounding_frames(&frames, 1500);
        assert_eq!(before.unwrap().timestamp_ms, 1000);
        assert_eq!(after.unwrap().timestamp_ms, 2000);
    }

    #[test]
    fn surrounding_frames_exact_match_goes_to_after() {
        let frames = vec![frame(1000), frame(2000), frame(3000)];
        let (before, after) = find_surrounding_frames(&frames, 2000);
        assert_eq!(before.unwrap().timestamp_ms, 1000);
        assert_eq!(after.unwrap().timestamp_ms, 2000);
    }

    #[test]
    fn surrounding_frames_before_first() {
        let frames = vec![frame(1000), frame(2000)];
        let (before, after) = find_surrounding_frames(&frames, 500);
        assert!(before.is_none());
        assert_eq!(after.unwrap().timestamp_ms, 1000);
    }

    #[test]
    fn surrounding_frames_after_last() {
        let frames = vec![frame(1000), frame(2000)];
        let (before, after) = find_surrounding_frames(&frames, 5000);
        assert_eq!(before.unwrap().timestamp_ms, 2000);
        assert!(after.is_none());
    }

    #[test]
    fn surrounding_frames_empty() {
        let frames: Vec<RecordedFrame> = vec![];
        let (before, after) = find_surrounding_frames(&frames, 1000);
        assert!(before.is_none());
        assert!(after.is_none());
    }

    #[test]
    fn surrounding_frames_single_element_before() {
        let frames = vec![frame(1000)];
        let (before, after) = find_surrounding_frames(&frames, 2000);
        assert_eq!(before.unwrap().timestamp_ms, 1000);
        assert!(after.is_none());
    }

    #[test]
    fn surrounding_frames_single_element_after() {
        let frames = vec![frame(5000)];
        let (before, after) = find_surrounding_frames(&frames, 1000);
        assert!(before.is_none());
        assert_eq!(after.unwrap().timestamp_ms, 5000);
    }

    // --- attach_recording_frames tests ---

    fn hover_action(event_id: Uuid) -> WalkthroughAction {
        WalkthroughAction {
            id: Uuid::new_v4(),
            kind: WalkthroughActionKind::Hover {
                x: 100.0,
                y: 200.0,
                dwell_ms: 2000,
            },
            app_name: Some("TestApp".to_string()),
            window_title: None,
            target_candidates: vec![],
            artifact_paths: vec![],
            source_event_ids: vec![event_id],
            confidence: ActionConfidence::Medium,
            warnings: vec![],
            screenshot_meta: None,
            candidate: true,
        }
    }

    fn click_action(event_id: Uuid) -> WalkthroughAction {
        WalkthroughAction {
            id: Uuid::new_v4(),
            kind: WalkthroughActionKind::Click {
                x: 300.0,
                y: 400.0,
                button: MouseButton::Left,
                click_count: 1,
            },
            app_name: Some("TestApp".to_string()),
            window_title: None,
            target_candidates: vec![],
            artifact_paths: vec!["/screenshots/click.png".to_string()],
            source_event_ids: vec![event_id],
            confidence: ActionConfidence::High,
            warnings: vec![],
            screenshot_meta: Some(ScreenshotMeta {
                origin_x: 0.0,
                origin_y: 0.0,
                scale: 2.0,
            }),
            candidate: false,
        }
    }

    /// Native hover event: timestamp = exit time (cursor left).
    /// dwell_ms = 2000, so hover start = ts - 2000.
    fn hover_event(id: Uuid, ts: u64) -> WalkthroughEvent {
        WalkthroughEvent {
            id,
            timestamp: ts,
            kind: WalkthroughEventKind::HoverDetected {
                x: 100.0,
                y: 200.0,
                element_name: "Button".to_string(),
                element_role: Some("AXButton".to_string()),
                dwell_ms: 2000,
                app_name: None,
            },
        }
    }

    /// CDP hover event: timestamp = enter time (hover start).
    /// dwell_ms = 2000, but no subtraction needed for start time.
    fn cdp_hover_event(id: Uuid, ts: u64) -> WalkthroughEvent {
        WalkthroughEvent {
            id,
            timestamp: ts,
            kind: WalkthroughEventKind::HoverDetected {
                x: 100.0,
                y: 200.0,
                element_name: "Submit".to_string(),
                element_role: Some("button".to_string()),
                dwell_ms: 2000,
                app_name: Some("Chrome".to_string()),
            },
        }
    }

    #[test]
    fn attach_recording_frames_before_after_pair() {
        let hover_id = Uuid::new_v4();
        let events = vec![hover_event(hover_id, 3000)];
        let frames = vec![frame(1000), frame(2000), frame(3000), frame(4000)];
        let mut actions = vec![hover_action(hover_id)];

        attach_recording_frames(&mut actions, &frames, &events);

        assert_eq!(actions[0].artifact_paths.len(), 2);
        assert_eq!(actions[0].artifact_paths[0], "/frames/frame_2000.png");
        assert_eq!(actions[0].artifact_paths[1], "/frames/frame_3000.png");
        let meta = actions[0].screenshot_meta.unwrap();
        assert_eq!(meta.scale, 2.0);
    }

    #[test]
    fn attach_recording_frames_skips_clicks() {
        let click_id = Uuid::new_v4();
        let events = vec![hover_event(click_id, 5000)];
        let frames = vec![frame(1000), frame(2000), frame(3000)];
        let mut actions = vec![click_action(click_id)];

        attach_recording_frames(&mut actions, &frames, &events);

        assert_eq!(actions[0].artifact_paths.len(), 1);
        assert_eq!(actions[0].artifact_paths[0], "/screenshots/click.png");
    }

    #[test]
    fn attach_recording_frames_skips_hovers_with_existing_screenshot() {
        let hover_id = Uuid::new_v4();
        let events = vec![hover_event(hover_id, 5000)];
        let frames = vec![frame(1000), frame(2000)];
        let mut actions = vec![{
            let mut a = hover_action(hover_id);
            a.artifact_paths = vec!["/existing/screenshot.png".to_string()];
            a
        }];

        attach_recording_frames(&mut actions, &frames, &events);

        assert_eq!(actions[0].artifact_paths[0], "/existing/screenshot.png");
    }

    #[test]
    fn attach_recording_frames_empty_frames_is_noop() {
        let hover_id = Uuid::new_v4();
        let events = vec![hover_event(hover_id, 5000)];
        let mut actions = vec![hover_action(hover_id)];

        attach_recording_frames(&mut actions, &[], &events);

        assert!(actions[0].artifact_paths.is_empty());
    }

    #[test]
    fn attach_recording_frames_only_before_when_hover_starts_after_last_frame() {
        let hover_id = Uuid::new_v4();
        let events = vec![hover_event(hover_id, 8000)];
        let frames = vec![frame(1000), frame(2000)];
        let mut actions = vec![hover_action(hover_id)];

        attach_recording_frames(&mut actions, &frames, &events);

        assert_eq!(actions[0].artifact_paths.len(), 1);
        assert_eq!(actions[0].artifact_paths[0], "/frames/frame_2000.png");
    }

    #[test]
    fn attach_recording_frames_only_after_when_hover_starts_before_first_frame() {
        let hover_id = Uuid::new_v4();
        let events = vec![hover_event(hover_id, 500)];
        let frames = vec![frame(1000), frame(2000)];
        let mut actions = vec![hover_action(hover_id)];

        attach_recording_frames(&mut actions, &frames, &events);

        assert_eq!(actions[0].artifact_paths.len(), 1);
        assert_eq!(actions[0].artifact_paths[0], "/frames/frame_1000.png");
    }

    #[test]
    fn attach_recording_frames_native_and_cdp_both_use_timestamp_directly() {
        let native_id = Uuid::new_v4();
        let cdp_id = Uuid::new_v4();
        let events = vec![hover_event(native_id, 3000), cdp_hover_event(cdp_id, 3000)];
        let frames = vec![frame(1000), frame(2000), frame(3000), frame(4000)];
        let mut native_actions = vec![hover_action(native_id)];
        let mut cdp_actions = vec![hover_action(cdp_id)];

        attach_recording_frames(&mut native_actions, &frames, &events);
        attach_recording_frames(&mut cdp_actions, &frames, &events);

        assert_eq!(
            native_actions[0].artifact_paths,
            cdp_actions[0].artifact_paths
        );
        assert_eq!(
            native_actions[0].artifact_paths[0],
            "/frames/frame_2000.png"
        );
        assert_eq!(
            native_actions[0].artifact_paths[1],
            "/frames/frame_3000.png"
        );
    }

    #[test]
    fn attach_recording_frames_prefers_same_app_frames() {
        let hover_id = Uuid::new_v4();
        let events = vec![hover_event(hover_id, 3000)];
        let frames = vec![
            frame_for_app(1000, "OtherApp"),
            frame_for_app(2000, "TestApp"),
            frame_for_app(2500, "OtherApp"),
            frame_for_app(3000, "TestApp"),
            frame_for_app(3500, "OtherApp"),
        ];
        let mut actions = vec![hover_action(hover_id)];

        attach_recording_frames(&mut actions, &frames, &events);

        assert_eq!(actions[0].artifact_paths.len(), 2);
        assert_eq!(actions[0].artifact_paths[0], "/frames/frame_2000.png");
        assert_eq!(actions[0].artifact_paths[1], "/frames/frame_3000.png");
    }

    #[test]
    fn attach_recording_frames_falls_back_to_all_frames_when_no_app_match() {
        let hover_id = Uuid::new_v4();
        let events = vec![{
            let e = hover_event(hover_id, 3000);
            if let WalkthroughEventKind::HoverDetected { .. } = &e.kind {
                // hover_event already has app_name: None
            }
            e
        }];
        let frames = vec![
            frame_for_app(2000, "SomeApp"),
            frame_for_app(4000, "SomeApp"),
        ];
        let mut actions = vec![{
            let mut a = hover_action(hover_id);
            a.app_name = None;
            a
        }];

        attach_recording_frames(&mut actions, &frames, &events);

        assert_eq!(actions[0].artifact_paths.len(), 2);
        assert_eq!(actions[0].artifact_paths[0], "/frames/frame_2000.png");
        assert_eq!(actions[0].artifact_paths[1], "/frames/frame_4000.png");
    }

    // --- parse_accessibility_json tests ---

    #[test]
    fn parse_ax_name_preferred() {
        let obj = serde_json::json!({"name": "Submit", "value": "val", "role": "AXButton"});
        let ax = parse_accessibility_json(&obj).unwrap();
        assert_eq!(ax.label, "Submit");
        assert_eq!(ax.role.as_deref(), Some("AXButton"));
    }

    #[test]
    fn parse_ax_falls_back_to_value() {
        let obj = serde_json::json!({"value": "hello", "role": "AXTextField"});
        let ax = parse_accessibility_json(&obj).unwrap();
        assert_eq!(ax.label, "hello");
    }

    #[test]
    fn parse_ax_returns_none_when_no_data() {
        let obj = serde_json::json!({"role": "AXGroup"});
        assert!(parse_accessibility_json(&obj).is_none());
    }

    #[test]
    fn parse_ax_subrole_only() {
        let obj = serde_json::json!({"subrole": "AXCloseButton", "role": "AXButton"});
        let ax = parse_accessibility_json(&obj).unwrap();
        assert_eq!(ax.label, "");
        assert_eq!(ax.subrole.as_deref(), Some("AXCloseButton"));
    }

    // --- clean_vlm_label tests ---

    #[test]
    fn clean_vlm_strips_quotes() {
        assert_eq!(clean_vlm_label("\"Send\"").unwrap(), "Send");
    }

    #[test]
    fn clean_vlm_rejects_empty() {
        assert!(clean_vlm_label("").is_none());
        assert!(clean_vlm_label("  ").is_none());
    }

    #[test]
    fn clean_vlm_rejects_long() {
        let long = "a".repeat(VLM_LABEL_MAX_LEN + 1);
        assert!(clean_vlm_label(&long).is_none());
    }

    // --- parse_cdp_click_data tests ---

    #[test]
    fn parse_cdp_click_with_aria_label() {
        let data = serde_json::json!({
            "ariaLabel": "Submit",
            "role": "button",
            "tagName": "BUTTON"
        });
        let (name, role, _, _, _) = parse_cdp_click_data(&data).unwrap();
        assert_eq!(name, "Submit");
        assert_eq!(role.as_deref(), Some("button"));
    }

    #[test]
    fn parse_cdp_click_fallback_to_id() {
        let data = serde_json::json!({"id": "main-nav", "tagName": "DIV"});
        let (name, _, _, _, _) = parse_cdp_click_data(&data).unwrap();
        assert_eq!(name, "#main-nav");
    }

    #[test]
    fn parse_cdp_click_fallback_to_tag_role() {
        let data = serde_json::json!({"tagName": "DIV", "role": "navigation"});
        let (name, _, _, _, _) = parse_cdp_click_data(&data).unwrap();
        assert_eq!(name, "div[navigation]");
    }

    // --- parse_cdp_hover_entry tests ---

    #[test]
    fn parse_cdp_hover_valid() {
        let entry = serde_json::json!({
            "textContent": "Settings",
            "role": "button",
            "ts": 5000,
            "dwellMs": 1500,
            "x": 100.0,
            "y": 200.0,
        });
        let result = parse_cdp_hover_entry(&entry);
        assert!(result.is_some());
        let (label, role, _, _, _, ts, dwell, x, y) = result.unwrap();
        assert_eq!(label, "Settings");
        assert_eq!(role.as_deref(), Some("button"));
        assert_eq!(ts, 5000);
        assert_eq!(dwell, 1500);
        assert_eq!(x, 100.0);
        assert_eq!(y, 200.0);
    }

    #[test]
    fn parse_cdp_hover_no_label_returns_none() {
        let entry = serde_json::json!({"role": "button", "ts": 5000});
        assert!(parse_cdp_hover_entry(&entry).is_none());
    }

    // --- parse_native_hover_entry tests ---

    #[test]
    fn parse_native_hover_valid() {
        let ev = serde_json::json!({
            "cursor": {"x": 50.0, "y": 60.0},
            "element": {"name": "File", "role": "AXMenuBarItem"},
            "dwell_ms": 3000,
            "timestamp_ms": 12345,
        });
        let result = parse_native_hover_entry(&ev);
        assert!(result.is_some());
        let (x, y, name, role, dwell, ts, app) = result.unwrap();
        assert_eq!(x, 50.0);
        assert_eq!(y, 60.0);
        assert_eq!(name, "File");
        assert_eq!(role.as_deref(), Some("AXMenuBarItem"));
        assert_eq!(dwell, 3000);
        assert_eq!(ts, 12345);
        assert!(app.is_none());
    }

    #[test]
    fn parse_native_hover_timeout_sentinel() {
        let ev = serde_json::json!({"timeout": true});
        assert!(parse_native_hover_entry(&ev).is_none());
    }

    // --- parse_screenshot_metadata_json tests ---

    #[test]
    fn parse_screenshot_meta_valid() {
        let obj = serde_json::json!({
            "screenshot_origin_x": 10.0,
            "screenshot_origin_y": 20.0,
            "screenshot_scale": 2.0,
        });
        let meta = parse_screenshot_metadata_json(&obj).unwrap();
        assert_eq!(meta.origin_x, 10.0);
        assert_eq!(meta.origin_y, 20.0);
        assert_eq!(meta.scale, 2.0);
    }

    #[test]
    fn parse_screenshot_meta_missing_field() {
        let obj = serde_json::json!({"screenshot_origin_x": 10.0});
        assert!(parse_screenshot_metadata_json(&obj).is_none());
    }
}
