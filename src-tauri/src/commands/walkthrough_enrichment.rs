use base64::Engine;
use clickweave_core::walkthrough::{
    ScreenshotKind, ScreenshotMeta, WalkthroughAction, WalkthroughEvent, WalkthroughEventKind,
};
use clickweave_mcp::McpRouter;
use uuid::Uuid;

use super::walkthrough::VLM_CALL_TIMEOUT;

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
                    "Accessibility enrichment: label={:?} role={:?} at ({x:.0}, {y:.0})",
                    ax.0,
                    ax.1
                );
                events.push(WalkthroughEvent {
                    id: Uuid::new_v4(),
                    timestamp,
                    kind: WalkthroughEventKind::AccessibilityElementCaptured {
                        label: ax.0,
                        role: ax.1,
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

/// Parse the `element_at_point` MCP response into `(label, role)`.
///
/// Picks the best display text from the response fields:
/// `name` (AXTitle) > `value` (AXValue) > `label` (AXDescription).
pub(super) fn parse_accessibility_result(
    content: &[clickweave_mcp::ToolContent],
) -> Option<(String, Option<String>)> {
    let obj = find_json_in_content(content)?;
    let label = obj["name"]
        .as_str()
        .or_else(|| obj["value"].as_str())
        .or_else(|| obj["label"].as_str())
        .filter(|s| !s.is_empty())?;
    let role = obj["role"].as_str().map(|s| s.to_string());
    Some((label.to_string(), role))
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

/// Use a VLM to identify click targets for all click actions (in parallel).
///
/// For each Click action that has a screenshot artifact and screenshot metadata,
/// draws a crosshair on the screenshot and sends it to the VLM asking what UI
/// element was clicked. Image prep and VLM calls all run concurrently.
pub(super) async fn resolve_click_targets_with_vlm(
    actions: &mut [WalkthroughAction],
    planner_cfg: &super::types::EndpointConfig,
) {
    use clickweave_core::walkthrough::{TargetCandidate, WalkthroughActionKind};

    if planner_cfg.is_empty() {
        return;
    }

    // Collect the data needed per eligible click. Image prep (PNG decode +
    // crosshair draw + JPEG encode) moves inside each spawned task so all
    // clicks are prepared concurrently instead of sequentially.
    struct ClickInput {
        action_idx: usize,
        screenshot_path: String,
        click_x: f64,
        click_y: f64,
        meta: ScreenshotMeta,
        ax_label: Option<(String, Option<String>)>,
        ocr_text: Option<String>,
        app_name: Option<String>,
    }

    let mut inputs: Vec<ClickInput> = Vec::new();

    for (idx, action) in actions.iter().enumerate() {
        let (click_x, click_y) = match &action.kind {
            WalkthroughActionKind::Click { x, y, .. } => (*x, *y),
            _ => continue,
        };

        // Skip clicks that already have a specific accessibility label.
        if action
            .target_candidates
            .iter()
            .any(|c| c.is_actionable_ax_label())
        {
            continue;
        }

        // Skip clicks that already have a VLM label (resolved during recording).
        if action
            .target_candidates
            .iter()
            .any(|c| matches!(c, TargetCandidate::VlmLabel { .. }))
        {
            continue;
        }

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

        inputs.push(ClickInput {
            action_idx: idx,
            screenshot_path,
            click_x,
            click_y,
            meta,
            ax_label,
            ocr_text,
            app_name: action.app_name.clone(),
        });
    }

    if inputs.is_empty() {
        return;
    }

    tracing::info!("VLM: resolving {} click targets in parallel", inputs.len());

    let llm_config = planner_cfg
        .clone()
        .into_llm_config(Some(0.1))
        .with_max_tokens(2048)
        .with_thinking(false);
    let backend = std::sync::Arc::new(clickweave_llm::LlmClient::new(llm_config));

    // Fire all tasks in parallel — each task prepares its own image on the
    // blocking pool (PNG decode + crosshair draw + JPEG encode) and then
    // sends the VLM request (async HTTP).
    let mut join_set = tokio::task::JoinSet::new();

    for input in inputs {
        let backend = backend.clone();

        join_set.spawn(async move {
            // Image prep is CPU-heavy (PNG decode + draw + JPEG encode) plus
            // blocking file I/O — run on the blocking pool.
            let req = tokio::task::spawn_blocking(move || {
                let ax_ref = input
                    .ax_label
                    .as_ref()
                    .map(|(l, r)| (l.as_str(), r.as_deref()));
                prepare_vlm_click_request(
                    &input.screenshot_path,
                    input.click_x,
                    input.click_y,
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
            let (click_x, click_y) = match &actions[action_idx].kind {
                WalkthroughActionKind::Click { x, y, .. } => (*x, *y),
                _ => continue,
            };
            tracing::info!("VLM resolved click at ({click_x:.0}, {click_y:.0}) → \"{label}\"");
            let action = &mut actions[action_idx];
            // Insert VLM label after actionable AX labels but before
            // non-actionable AX labels, OCR, and coordinates.
            let insert_pos = action
                .target_candidates
                .iter()
                .position(|c| !c.is_actionable_ax_label())
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

/// Downscale the full window screenshot and draw a red crosshair at the click point.
///
/// Draws a red crosshair at `(px, py)` in image-pixel coordinates, then
/// downscales + JPEG-encodes via the shared VLM image prep utility.
/// Returns `None` if the image can't be decoded.
pub(super) fn mark_click_point(png_bytes: &[u8], px: f64, py: f64) -> Option<String> {
    let img = image::load_from_memory(png_bytes).ok()?;
    let (img_w, img_h) = (img.width(), img.height());

    // Scale crosshair dimensions so it remains visible after VLM downscaling.
    // A 3152px Retina screenshot downscales ~0.4x to 1280px; a 1px line would
    // become sub-pixel and vanish in Triangle filter + JPEG compression.
    let longest = img_w.max(img_h) as f64;
    let scale = (longest / clickweave_llm::DEFAULT_MAX_DIMENSION as f64).max(1.0);
    let half_thickness = (2.0 * scale).round() as i64;
    let arm_length = (20.0 * scale).round() as i64;
    let gap = (4.0 * scale).round() as i64;

    let mut rgba = img.into_rgba8();
    let cx = (px as u32).min(img_w.saturating_sub(1)) as i64;
    let cy = (py as u32).min(img_h.saturating_sub(1)) as i64;

    let outline = image::Rgba([0, 0, 0, 200]);
    let fill = image::Rgba([255, 0, 0, 255]);

    // Draw a filled rectangle, clamped to image bounds.
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

    // Draw 4 arms (left, right, top, bottom) in two passes:
    // first a black outline (1px larger all around), then red fill on top.
    for (color, expand) in [(outline, 1i64), (fill, 0i64)] {
        // Left arm
        draw_rect(
            &mut rgba,
            cx - arm_length - expand,
            cy - half_thickness - expand,
            cx - gap + expand,
            cy + half_thickness + expand,
            color,
        );
        // Right arm
        draw_rect(
            &mut rgba,
            cx + gap - expand,
            cy - half_thickness - expand,
            cx + arm_length + expand,
            cy + half_thickness + expand,
            color,
        );
        // Top arm
        draw_rect(
            &mut rgba,
            cx - half_thickness - expand,
            cy - arm_length - expand,
            cx + half_thickness + expand,
            cy - gap + expand,
            color,
        );
        // Bottom arm
        draw_rect(
            &mut rgba,
            cx - half_thickness - expand,
            cy + gap - expand,
            cx + half_thickness + expand,
            cy + arm_length + expand,
            color,
        );
    }

    let (b64, _mime) = clickweave_llm::prepare_dynimage_for_vlm(
        image::DynamicImage::ImageRgba8(rgba),
        clickweave_llm::DEFAULT_MAX_DIMENSION,
    );
    Some(b64)
}
