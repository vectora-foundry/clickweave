use base64::Engine;
use clickweave_core::walkthrough::{
    ScreenshotKind, ScreenshotMeta, WalkthroughAction, WalkthroughEvent, WalkthroughEventKind,
};
use clickweave_mcp::McpRouter;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::walkthrough::VLM_CALL_TIMEOUT;

/// A single frame from continuous screen recording (returned by `stop_recording`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct RecordedFrame {
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

/// Parse the `stop_recording` MCP response into a sorted list of frames.
pub(super) fn parse_recording_frames(
    content: &[clickweave_mcp::ToolContent],
) -> Vec<RecordedFrame> {
    let raw_text: String = content.iter().filter_map(|c| c.as_text()).collect();
    let mut frames: Vec<RecordedFrame> = match serde_json::from_str(&raw_text) {
        Ok(f) => f,
        Err(e) => {
            tracing::warn!("Failed to parse recording frames: {e}");
            return Vec::new();
        }
    };
    frames.sort_by_key(|f| f.timestamp_ms);
    frames
}

/// Maximum length of a VLM-resolved label to accept. Longer responses
/// are likely full sentences rather than a concise element name.
const VLM_LABEL_MAX_LEN: usize = 80;

/// Half-size of the click crop in screen points (32pt radius → 64pt square →
/// 128px on Retina). On macOS this re-exports the platform constant; on other
/// platforms it's defined inline.
#[cfg(target_os = "macos")]
use crate::platform::macos::CURSOR_REGION_HALF_PT as CROP_HALF_SIZE_PTS;
#[cfg(not(target_os = "macos"))]
const CROP_HALF_SIZE_PTS: f64 = 32.0;

/// Enrich a click event with accessibility data and a screenshot with OCR.
///
/// Returns accessibility, screenshot, and OCR events if successful.
pub(super) async fn enrich_click(
    mcp: &McpRouter,
    session_dir: &std::path::Path,
    x: f64,
    y: f64,
    app_name: Option<&str>,
    timestamp: u64,
) -> Vec<WalkthroughEvent> {
    let mut events = Vec::new();

    // Build args for both calls.
    let app_name_val = app_name.map(|n| serde_json::Value::String(n.to_string()));
    let mut ax_args = serde_json::json!({ "x": x, "y": y });
    let mut screenshot_args = serde_json::json!({
        "mode": "window",
        "include_ocr": false,
    });
    if let Some(val) = &app_name_val {
        ax_args["app_name"] = val.clone();
        screenshot_args["app_name"] = val.clone();
    }

    // Fire both MCP calls in parallel. No per-call timeout — calls
    // serialize through io_lock so timeouts would fire while waiting
    // in the queue, not during actual execution. The background task
    // lifetime is bounded by the drain in the event loop.
    let (ax_result, screenshot_result) = tokio::join!(
        mcp.call_tool("element_at_point", Some(ax_args)),
        mcp.call_tool("take_screenshot", Some(screenshot_args)),
    );

    // Process accessibility result.
    match ax_result {
        Err(e) => {
            tracing::info!("Accessibility enrichment failed at ({x:.0}, {y:.0}): {e}");
        }
        Ok(result) => {
            if let Some(ax) = parse_accessibility_result(&result.content) {
                tracing::info!(
                    "Accessibility enrichment: label={:?} role={:?} subrole={:?} at ({x:.0}, {y:.0})",
                    ax.label,
                    ax.role,
                    ax.subrole,
                );
                events.push(WalkthroughEvent {
                    id: Uuid::new_v4(),
                    timestamp,
                    kind: WalkthroughEventKind::AccessibilityElementCaptured {
                        label: ax.label,
                        role: ax.role,
                        subrole: ax.subrole,
                    },
                });
            } else {
                let raw: Vec<String> = result
                    .content
                    .iter()
                    .filter_map(|c| c.as_text().map(|s| s.to_string()))
                    .collect();
                tracing::info!(
                    "Accessibility enrichment: no label parsed at ({x:.0}, {y:.0}), raw={raw:?}"
                );
            }
        }
    }

    // Process screenshot result.
    match screenshot_result {
        Err(e) => {
            tracing::info!("Screenshot enrichment failed at ({x:.0}, {y:.0}): {e}");
        }
        Ok(result) => {
            let screenshot_meta = parse_screenshot_metadata(&result.content);

            for content in &result.content {
                if let clickweave_mcp::ToolContent::Image { data, .. } = content {
                    let filename = format!("click_{timestamp}.png");
                    let artifact_path = session_dir.join("artifacts").join(&filename);
                    if let Ok(image_bytes) = base64::engine::general_purpose::STANDARD.decode(data)
                    {
                        let _ = std::fs::write(&artifact_path, &image_bytes);
                        events.push(WalkthroughEvent {
                            id: Uuid::new_v4(),
                            timestamp,
                            kind: WalkthroughEventKind::ScreenshotCaptured {
                                path: artifact_path.to_string_lossy().to_string(),
                                kind: ScreenshotKind::AfterClick,
                                meta: screenshot_meta,
                                image_b64: None,
                            },
                        });
                    }
                }
            }
        }
    }

    events
}

/// Find the first JSON object in MCP tool response content.
pub(super) fn find_json_in_content(
    content: &[clickweave_mcp::ToolContent],
) -> Option<serde_json::Value> {
    content.iter().find_map(|item| {
        item.as_text()
            .and_then(|text| serde_json::from_str::<serde_json::Value>(text).ok())
    })
}

/// Parsed accessibility data from `element_at_point`.
pub(super) struct AccessibilityData {
    pub label: String,
    pub role: Option<String>,
    pub subrole: Option<String>,
}

/// Parse the `element_at_point` MCP response into accessibility data.
///
/// Picks the best display text from the response fields:
/// `name` (AXTitle) > `value` (AXValue) > `label` (AXDescription).
///
/// Returns `None` only if no display text AND no subrole are present.
/// Window control buttons (close/minimize/zoom) may lack text labels
/// but always have a subrole set by the macOS window server.
pub(super) fn parse_accessibility_result(
    content: &[clickweave_mcp::ToolContent],
) -> Option<AccessibilityData> {
    let obj = find_json_in_content(content)?;
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

/// Parse screenshot metadata (origin, scale) from the MCP take_screenshot response.
pub(super) fn parse_screenshot_metadata(
    content: &[clickweave_mcp::ToolContent],
) -> Option<ScreenshotMeta> {
    let obj = find_json_in_content(content)?;
    Some(ScreenshotMeta {
        origin_x: obj["screenshot_origin_x"].as_f64()?,
        origin_y: obj["screenshot_origin_y"].as_f64()?,
        scale: obj["screenshot_scale"].as_f64()?,
    })
}

/// Data needed to fire a VLM request for a single click.
pub(super) struct VlmClickRequest {
    pub(super) image_b64: String,
    pub(super) prompt: String,
}

/// Prepare a VLM request for a single click: read screenshot, mark crosshair,
/// build prompt with context hints. Returns `None` if prerequisites are missing.
pub(super) fn prepare_vlm_click_request(
    screenshot_path: &str,
    click_x: f64,
    click_y: f64,
    meta: ScreenshotMeta,
    ax_label: Option<(&str, Option<&str>)>,
    ocr_text: Option<&str>,
    app_name: Option<&str>,
) -> Option<VlmClickRequest> {
    let image_bytes = match std::fs::read(screenshot_path) {
        Ok(bytes) => bytes,
        Err(e) => {
            tracing::warn!("VLM: failed to read {screenshot_path}: {e}");
            return None;
        }
    };

    // Compute click position in image pixel coordinates.
    let (px, py) = meta.screen_to_pixel(click_x, click_y);

    let image_b64 = mark_click_point(&image_bytes, px, py)?;

    // Build context-aware prompt with hints from captured data.
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

    Some(VlmClickRequest { image_b64, prompt })
}

/// Execute a VLM request and return the resolved label, or `None` on failure.
///
/// Retries once if the model exhausts its token budget on reasoning.
pub(super) async fn execute_vlm_click_request(
    backend: &clickweave_llm::LlmClient,
    request: &VlmClickRequest,
) -> Option<String> {
    let make_messages = || {
        vec![clickweave_llm::Message::user_with_images(
            request.prompt.clone(),
            vec![(request.image_b64.clone(), "image/jpeg".to_string())],
        )]
    };

    let result = clickweave_llm::ChatBackend::chat(backend, make_messages(), None).await;

    // Retry once if the model exhausted the token budget on reasoning.
    let needs_retry = match &result {
        Ok(resp) => resp.choices.first().is_some_and(|c| {
            c.finish_reason.as_deref() == Some("length")
                && c.message.content_text().is_none_or(|t| t.trim().is_empty())
        }),
        Err(_) => false,
    };

    let final_result = if needs_retry {
        clickweave_llm::ChatBackend::chat(backend, make_messages(), None).await
    } else {
        result
    };

    match final_result {
        Ok(response) => response
            .choices
            .first()
            .and_then(|c| c.message.content_text())
            .map(|label| label.trim().trim_matches('"').to_string())
            .filter(|label| !label.is_empty() && label.len() <= VLM_LABEL_MAX_LEN),
        Err(_) => None,
    }
}

/// Find the frames immediately before and after the given timestamp.
///
/// Returns `(before, after)` where:
/// - `before` is the last frame with `timestamp_ms < timestamp`
/// - `after` is the first frame with `timestamp_ms >= timestamp`
///
/// Frames must be sorted by `timestamp_ms` (guaranteed by `parse_recording_frames`).
/// Uses binary search for O(log n) lookup.
fn find_surrounding_frames(
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
pub(super) fn attach_recording_frames(
    actions: &mut [WalkthroughAction],
    frames: &[RecordedFrame],
    events: &[clickweave_core::walkthrough::WalkthroughEvent],
) {
    use clickweave_core::walkthrough::WalkthroughActionKind;

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

/// Use a VLM to identify click/hover targets (in parallel).
///
/// For each Click or Hover action that lacks an actionable AX label or VLM label,
/// draws a crosshair on the screenshot and sends it to the VLM asking what UI
/// element is at that point. For hovers with before/after recording frames,
/// uses the before frame (element unobscured by hover effects).
pub(super) async fn resolve_click_targets_with_vlm(
    actions: &mut [WalkthroughAction],
    planner_cfg: &super::types::EndpointConfig,
) {
    use clickweave_core::walkthrough::{TargetCandidate, WalkthroughActionKind};

    if planner_cfg.is_empty() {
        return;
    }

    struct VlmInput {
        action_idx: usize,
        screenshot_path: String,
        x: f64,
        y: f64,
        meta: ScreenshotMeta,
        ax_label: Option<(String, Option<String>)>,
        ocr_text: Option<String>,
        app_name: Option<String>,
    }

    let mut inputs: Vec<VlmInput> = Vec::new();

    for (idx, action) in actions.iter().enumerate() {
        let (x, y) = match &action.kind {
            WalkthroughActionKind::Click { x, y, .. } => (*x, *y),
            WalkthroughActionKind::Hover { x, y, .. } => (*x, *y),
            _ => continue,
        };

        if action
            .target_candidates
            .iter()
            .any(|c| c.is_actionable_ax_label())
        {
            continue;
        }
        if action
            .target_candidates
            .iter()
            .any(|c| matches!(c, TargetCandidate::VlmLabel { .. }))
        {
            continue;
        }

        // For hovers with before/after frames, use artifact_paths[0] (the
        // before frame) so the element is unobscured by hover effects.
        // For clicks, artifact_paths[0] is the per-click screenshot.
        let screenshot_path = match action.artifact_paths.first() {
            Some(p) => p.clone(),
            None => continue,
        };
        let meta = match &action.screenshot_meta {
            Some(m) => *m,
            None => continue,
        };

        let ax_label = action.target_candidates.iter().find_map(|c| match c {
            TargetCandidate::AccessibilityLabel { label, role } => {
                Some((label.clone(), role.clone()))
            }
            _ => None,
        });
        let ocr_text = action.target_candidates.iter().find_map(|c| match c {
            TargetCandidate::OcrText { text } => Some(text.clone()),
            _ => None,
        });

        inputs.push(VlmInput {
            action_idx: idx,
            screenshot_path,
            x,
            y,
            meta,
            ax_label,
            ocr_text,
            app_name: action.app_name.clone(),
        });
    }

    if inputs.is_empty() {
        return;
    }

    tracing::info!(
        "VLM: resolving {} click/hover targets in parallel",
        inputs.len()
    );

    let llm_config = planner_cfg
        .clone()
        .into_llm_config(Some(0.1))
        .with_max_tokens(2048)
        .with_thinking(false);
    let backend = std::sync::Arc::new(clickweave_llm::LlmClient::new(llm_config));

    let mut join_set = tokio::task::JoinSet::new();

    for input in inputs {
        let backend = backend.clone();

        join_set.spawn(async move {
            let req = tokio::task::spawn_blocking(move || {
                let ax_ref = input
                    .ax_label
                    .as_ref()
                    .map(|(l, r)| (l.as_str(), r.as_deref()));
                prepare_vlm_click_request(
                    &input.screenshot_path,
                    input.x,
                    input.y,
                    input.meta,
                    ax_ref,
                    input.ocr_text.as_deref(),
                    input.app_name.as_deref(),
                )
            })
            .await
            .ok()
            .flatten();

            let Some(req) = req else {
                return (input.action_idx, None);
            };

            let label = match tokio::time::timeout(
                VLM_CALL_TIMEOUT,
                execute_vlm_click_request(backend.as_ref(), &req),
            )
            .await
            {
                Ok(label) => label,
                Err(_) => {
                    tracing::warn!("Post-hoc VLM timed out for action {}", input.action_idx);
                    None
                }
            };
            (input.action_idx, label)
        });
    }

    // Collect results and apply to actions.
    while let Some(join_result) = join_set.join_next().await {
        let (action_idx, label) = match join_result {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!("VLM task panicked: {e}");
                continue;
            }
        };

        if let Some(label) = label {
            let (x, y) = match &actions[action_idx].kind {
                WalkthroughActionKind::Click { x, y, .. } => (*x, *y),
                WalkthroughActionKind::Hover { x, y, .. } => (*x, *y),
                _ => continue,
            };
            tracing::info!("VLM resolved target at ({x:.0}, {y:.0}) → \"{label}\"");
            let action = &mut actions[action_idx];
            let insert_pos = action
                .target_candidates
                .iter()
                .position(|c| {
                    !matches!(c, TargetCandidate::CdpElement { .. }) && !c.is_actionable_ax_label()
                })
                .unwrap_or(action.target_candidates.len());
            action
                .target_candidates
                .insert(insert_pos, TargetCandidate::VlmLabel { label });
        }
    }
}

/// Crop a region around the click point from a screenshot and encode as JPEG.
///
/// `img` — decoded screenshot (raw RGBA from pre-hover buffer or PNG from disk).
/// `(px, py)` — click position in **image-pixel** coordinates.
/// `scale` — display scale factor (e.g. 2.0 for Retina).
///
/// Returns `(jpeg_bytes_for_disk, base64_jpeg)`, or `None` on failure.
pub(super) fn crop_click_region(
    img: &image::DynamicImage,
    px: f64,
    py: f64,
    scale: f64,
) -> Option<(Vec<u8>, String)> {
    let (img_w, img_h) = (img.width(), img.height());

    let half_px = (CROP_HALF_SIZE_PTS * scale).round() as u32;
    let cx = (px.round() as u32).min(img_w.saturating_sub(1));
    let cy = (py.round() as u32).min(img_h.saturating_sub(1));

    let x0 = cx.saturating_sub(half_px);
    let y0 = cy.saturating_sub(half_px);
    let x1 = (cx + half_px).min(img_w);
    let y1 = (cy + half_px).min(img_h);
    let crop_w = x1 - x0;
    let crop_h = y1 - y0;
    if crop_w == 0 || crop_h == 0 {
        return None;
    }

    let cropped = img.crop_imm(x0, y0, crop_w, crop_h);

    // Single JPEG encode: save bytes to disk and base64-encode for events.
    let mut jpeg_buf = std::io::Cursor::new(Vec::new());
    cropped
        .write_to(&mut jpeg_buf, image::ImageFormat::Jpeg)
        .ok()?;
    let jpeg_bytes = jpeg_buf.into_inner();
    let b64 = base64::engine::general_purpose::STANDARD.encode(&jpeg_bytes);

    Some((jpeg_bytes, b64))
}

/// Draw a red crosshair with black outline on an RGBA image at pixel coordinates.
///
/// The crosshair is a gap-centered cross with 4 arms. Dimensions scale with
/// image size so the crosshair stays visible at any resolution.
fn draw_crosshair(rgba: &mut image::RgbaImage, px: f64, py: f64) {
    let (img_w, img_h) = (rgba.width(), rgba.height());

    let longest = img_w.max(img_h) as f64;
    let scale = (longest / clickweave_llm::DEFAULT_MAX_DIMENSION as f64).max(1.0);
    let half_thickness = (2.0 * scale).round() as i64;
    let arm_length = (20.0 * scale).round() as i64;
    let gap = (4.0 * scale).round() as i64;

    let cx = (px as u32).min(img_w.saturating_sub(1)) as i64;
    let cy = (py as u32).min(img_h.saturating_sub(1)) as i64;

    let outline = image::Rgba([0, 0, 0, 200]);
    let fill = image::Rgba([255, 0, 0, 255]);

    let draw_rect =
        |img: &mut image::RgbaImage, x0: i64, y0: i64, x1: i64, y1: i64, color: image::Rgba<u8>| {
            let x_lo = x0.max(0) as u32;
            let y_lo = y0.max(0) as u32;
            let x_hi = (x1.min(img_w as i64 - 1)).max(0) as u32;
            let y_hi = (y1.min(img_h as i64 - 1)).max(0) as u32;
            for y in y_lo..=y_hi {
                for x in x_lo..=x_hi {
                    img.put_pixel(x, y, color);
                }
            }
        };

    // Two passes: black outline (1px larger all around), then red fill.
    for (color, expand) in [(outline, 1i64), (fill, 0i64)] {
        draw_rect(
            rgba,
            cx - arm_length - expand,
            cy - half_thickness - expand,
            cx - gap + expand,
            cy + half_thickness + expand,
            color,
        );
        draw_rect(
            rgba,
            cx + gap - expand,
            cy - half_thickness - expand,
            cx + arm_length + expand,
            cy + half_thickness + expand,
            color,
        );
        draw_rect(
            rgba,
            cx - half_thickness - expand,
            cy - arm_length - expand,
            cx + half_thickness + expand,
            cy - gap + expand,
            color,
        );
        draw_rect(
            rgba,
            cx - half_thickness - expand,
            cy + gap - expand,
            cx + half_thickness + expand,
            cy + arm_length + expand,
            color,
        );
    }
}

/// Downscale the full window screenshot and draw a red crosshair at the click point.
///
/// Draws a red crosshair at `(px, py)` in image-pixel coordinates, then
/// downscales + JPEG-encodes via the shared VLM image prep utility.
/// Returns `None` if the image can't be decoded.
pub(super) fn mark_click_point(png_bytes: &[u8], px: f64, py: f64) -> Option<String> {
    let img = image::load_from_memory(png_bytes).ok()?;
    let mut rgba = img.into_rgba8();

    draw_crosshair(&mut rgba, px, py);

    let (b64, _mime) = clickweave_llm::prepare_dynimage_for_vlm(
        image::DynamicImage::ImageRgba8(rgba),
        clickweave_llm::DEFAULT_MAX_DIMENSION,
    );
    Some(b64)
}

/// Generate crosshair-marked screenshots for hover actions (parallel, async).
///
/// For each Hover action that has recording frame(s) (from
/// `attach_recording_frames`), loads each frame, draws a crosshair at the
/// hover position, and saves the result as JPEG. Updates the action's
/// `artifact_paths` to point to the new files.
///
/// When two frames are present (before/after hover start), both get
/// crosshairs. The before frame is used by VLM for element identification
/// (element unobscured by hover effects); both frames appear in the review
/// panel so the user can see the hover's visual effect.
///
/// Image decoding, crosshair drawing, and JPEG encoding run on the blocking
/// pool in parallel so they don't block the async runtime or serialize.
pub(super) async fn generate_hover_screenshots(
    actions: &mut [WalkthroughAction],
    session_dir: &std::path::Path,
) {
    use clickweave_core::walkthrough::WalkthroughActionKind;

    let artifacts_dir = session_dir.join("artifacts");

    struct HoverFrameInput {
        action_idx: usize,
        /// Index within `artifact_paths` (0 = before or single, 1 = after).
        path_idx: usize,
        source_path: String,
        hover_x: f64,
        hover_y: f64,
        meta: ScreenshotMeta,
        output_path: std::path::PathBuf,
    }

    let mut inputs = Vec::new();
    for (idx, action) in actions.iter().enumerate() {
        let (hover_x, hover_y) = match &action.kind {
            WalkthroughActionKind::Hover { x, y, .. } => (*x, *y),
            _ => continue,
        };
        if action.artifact_paths.is_empty() {
            continue;
        }
        let meta = match action.screenshot_meta {
            Some(m) => m,
            None => continue,
        };
        let id_simple = action.id.as_simple();
        for (path_idx, source_path) in action.artifact_paths.iter().enumerate() {
            let suffix = if action.artifact_paths.len() == 2 {
                if path_idx == 0 { "before" } else { "after" }
            } else {
                "hover"
            };
            let filename = format!("hover_{id_simple}_{suffix}.jpg");
            inputs.push(HoverFrameInput {
                action_idx: idx,
                path_idx,
                source_path: source_path.clone(),
                hover_x,
                hover_y,
                meta,
                output_path: artifacts_dir.join(filename),
            });
        }
    }

    if inputs.is_empty() {
        return;
    }

    // Process all frames in parallel on the blocking pool.
    let mut join_set = tokio::task::JoinSet::new();
    for input in inputs {
        join_set.spawn_blocking(move || {
            let img_bytes = std::fs::read(&input.source_path).ok()?;
            let img = image::load_from_memory(&img_bytes).ok()?;
            let (px, py) = input.meta.screen_to_pixel(input.hover_x, input.hover_y);
            let mut rgba = img.into_rgba8();
            draw_crosshair(&mut rgba, px, py);

            let dynamic = image::DynamicImage::ImageRgba8(rgba);
            let mut buf = std::io::Cursor::new(Vec::new());
            dynamic.write_to(&mut buf, image::ImageFormat::Jpeg).ok()?;
            std::fs::write(&input.output_path, buf.into_inner()).ok()?;
            Some((input.action_idx, input.path_idx, input.output_path))
        });
    }

    while let Some(result) = join_set.join_next().await {
        if let Ok(Some((action_idx, path_idx, path))) = result {
            actions[action_idx].artifact_paths[path_idx] = path.to_string_lossy().to_string();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clickweave_core::walkthrough::{
        ActionConfidence, WalkthroughAction, WalkthroughActionKind, WalkthroughEvent,
        WalkthroughEventKind,
    };
    use uuid::Uuid;

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
        // partition_point(< 2000) → idx=1, before=frame(1000), after=frame(2000)
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
                button: clickweave_core::MouseButton::Left,
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
        // Hover ts=3000 (arrival time). Frames: 1000, 2000, 3000, 4000.
        // Before start(3000): frame(2000). After start(3000): frame(3000).
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

        // Click already has a screenshot — should be unchanged.
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
        // Hover ts=8000, all frames before that.
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
        // Hover ts=500, all frames after that.
        let events = vec![hover_event(hover_id, 500)];
        let frames = vec![frame(1000), frame(2000)];
        let mut actions = vec![hover_action(hover_id)];

        attach_recording_frames(&mut actions, &frames, &events);

        assert_eq!(actions[0].artifact_paths.len(), 1);
        assert_eq!(actions[0].artifact_paths[0], "/frames/frame_1000.png");
    }

    #[test]
    fn attach_recording_frames_native_and_cdp_both_use_timestamp_directly() {
        // Both native and CDP hovers use the event timestamp as hover start.
        // Native: MCP fires transition event with timestamp_ms = arrival time.
        // CDP: JS listener stores ts = Date.now() at element enter.
        let native_id = Uuid::new_v4();
        let cdp_id = Uuid::new_v4();
        let events = vec![hover_event(native_id, 3000), cdp_hover_event(cdp_id, 3000)];
        let frames = vec![frame(1000), frame(2000), frame(3000), frame(4000)];
        let mut native_actions = vec![hover_action(native_id)];
        let mut cdp_actions = vec![hover_action(cdp_id)];

        attach_recording_frames(&mut native_actions, &frames, &events);
        attach_recording_frames(&mut cdp_actions, &frames, &events);

        // Both should get the same frame pair around ts=3000.
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
        // Hover on TestApp at ts=3000. Frames from two apps interleaved.
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

        // Should pick TestApp frames: before=2000, after=3000 (not OtherApp's 2500/3500).
        assert_eq!(actions[0].artifact_paths.len(), 2);
        assert_eq!(actions[0].artifact_paths[0], "/frames/frame_2000.png");
        assert_eq!(actions[0].artifact_paths[1], "/frames/frame_3000.png");
    }

    #[test]
    fn attach_recording_frames_falls_back_to_all_frames_when_no_app_match() {
        let hover_id = Uuid::new_v4();
        // Hover has app_name=None (can happen for native hovers without focus resolution).
        let events = vec![{
            let mut e = hover_event(hover_id, 3000);
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
            a.app_name = None; // No app resolved
            a
        }];

        attach_recording_frames(&mut actions, &frames, &events);

        // Falls back to all frames since no app name to filter on.
        assert_eq!(actions[0].artifact_paths.len(), 2);
        assert_eq!(actions[0].artifact_paths[0], "/frames/frame_2000.png");
        assert_eq!(actions[0].artifact_paths[1], "/frames/frame_4000.png");
    }
}
