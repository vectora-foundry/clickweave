use super::*;

pub(super) async fn enrich_click_background(
    mcp: std::sync::Arc<McpClient>,
    vlm_backend: Option<std::sync::Arc<clickweave_llm::LlmClient>>,
    app: tauri::AppHandle,
    storage: WalkthroughStorage,
    session_dir: std::path::PathBuf,
    app_name: Option<String>,
    x: f64,
    y: f64,
    timestamp: u64,
    vlm_timeout: tokio::time::Duration,
    app_kind_cache: Arc<Mutex<HashMap<i32, AppKind>>>,
    target_pid: i32,
    #[cfg(any(target_os = "macos", target_os = "windows"))] prehover_screenshot: Option<
        Arc<CursorRegionCapture>,
    >,
) {
    // Run enrichment without checking the cancel token — we want MCP calls
    // to complete even after Stop is pressed so every click gets a screenshot.
    // The drain timeout in the event loop bounds total shutdown time.
    let enrichment_events =
        enrich_click(&mcp, &session_dir, x, y, app_name.as_deref(), timestamp).await;

    for ev in &enrichment_events {
        persist_and_emit(&app, &storage, &session_dir, ev);
    }

    let summary = summarize_click_enrichment(&enrichment_events);
    maybe_reclassify_empty_ax_app(
        &app,
        &storage,
        &session_dir,
        &app_kind_cache,
        target_pid,
        app_name.as_deref(),
        timestamp,
        summary.has_actionable_ax,
    );

    // Both crop and VLM need a screenshot. Bail early if we don't have one.
    let (Some(screenshot_path), Some(screenshot_meta)) =
        (summary.screenshot_path, summary.screenshot_meta)
    else {
        return;
    };

    let crop_fut = persist_click_crop(
        app.clone(),
        storage.clone(),
        session_dir.clone(),
        screenshot_path.clone(),
        screenshot_meta,
        x,
        y,
        timestamp,
        #[cfg(any(target_os = "macos", target_os = "windows"))]
        prehover_screenshot,
    );
    let vlm_fut = persist_vlm_click_label(
        &app,
        &storage,
        &session_dir,
        vlm_backend,
        app_name.as_deref(),
        &screenshot_path,
        screenshot_meta,
        summary.ax_label_data.as_ref(),
        summary.has_actionable_ax,
        x,
        y,
        timestamp,
        vlm_timeout,
    );

    tokio::join!(crop_fut, vlm_fut);
}

fn summarize_click_enrichment(events: &[WalkthroughEvent]) -> ClickEnrichmentSummary {
    let mut summary = ClickEnrichmentSummary {
        screenshot_path: None,
        screenshot_meta: None,
        ax_label_data: None,
        has_actionable_ax: false,
    };

    for ev in events {
        match &ev.kind {
            WalkthroughEventKind::ScreenshotCaptured { path, meta, .. } => {
                summary.screenshot_path = Some(path.clone());
                summary.screenshot_meta = *meta;
            }
            WalkthroughEventKind::AccessibilityElementCaptured { label, role, .. } => {
                // Empty labels still need VLM fallback even if the AX role is actionable.
                summary.has_actionable_ax = !label.is_empty()
                    && clickweave_core::walkthrough::is_actionable_ax_role(role.as_deref());
                summary.ax_label_data = Some((label.clone(), role.clone()));
            }
            _ => {}
        }
    }

    summary
}

#[allow(clippy::too_many_arguments)]
fn maybe_reclassify_empty_ax_app(
    app: &tauri::AppHandle,
    storage: &WalkthroughStorage,
    session_dir: &std::path::Path,
    app_kind_cache: &Arc<Mutex<HashMap<i32, AppKind>>>,
    target_pid: i32,
    app_name: Option<&str>,
    timestamp: u64,
    has_actionable_ax: bool,
) {
    if has_actionable_ax {
        return;
    }
    let current_kind = app_kind_cache.lock().unwrap().get(&target_pid).copied();
    if current_kind != Some(AppKind::Native) {
        return;
    }

    let rechecked = classify_app_by_pid(target_pid);
    if rechecked == AppKind::Native {
        return;
    }

    tracing::info!(
        "Reactive detection: PID {} reclassified as {:?} (empty AX triggered recheck)",
        target_pid,
        rechecked,
    );
    app_kind_cache.lock().unwrap().insert(target_pid, rechecked);

    let updated_focus = WalkthroughEvent {
        id: Uuid::new_v4(),
        timestamp,
        kind: WalkthroughEventKind::AppFocused {
            app_name: app_name.unwrap_or_default().to_string(),
            pid: target_pid,
            window_title: None,
            app_kind: rechecked,
        },
    };
    persist_and_emit(app, storage, session_dir, &updated_focus);
}

#[allow(clippy::too_many_arguments)]
async fn persist_click_crop(
    app: tauri::AppHandle,
    storage: WalkthroughStorage,
    session_dir: std::path::PathBuf,
    screenshot_path: String,
    screenshot_meta: ScreenshotMeta,
    x: f64,
    y: f64,
    timestamp: u64,
    #[cfg(any(target_os = "macos", target_os = "windows"))] prehover_screenshot: Option<
        Arc<CursorRegionCapture>,
    >,
) {
    use crate::commands::walkthrough_enrichment::crop_click_region;

    let artifacts_dir = session_dir.join("artifacts");

    #[cfg(any(target_os = "macos", target_os = "windows"))]
    if let Some(shot) = prehover_screenshot {
        tracing::debug!("Using cursor region capture for click crop");
        let artifacts_for_capture = artifacts_dir.clone();
        let crop_result = tokio::task::spawn_blocking(move || {
            encode_cursor_region_crop(shot, artifacts_for_capture, timestamp)
        })
        .await;
        if let Ok(Some((crop_b64, crop_path))) = crop_result {
            persist_click_crop_event(&app, &storage, &session_dir, crop_b64, crop_path, timestamp);
            return;
        }
    }

    tracing::debug!("Falling back to MCP screenshot for crop");
    let bytes = match tokio::fs::read(&screenshot_path).await {
        Ok(b) => b,
        Err(_) => return,
    };
    let (px, py) = screenshot_meta.screen_to_pixel(x, y);
    let scale = screenshot_meta.scale;
    let crop_result = tokio::task::spawn_blocking(move || {
        let img = image::load_from_memory(&bytes).ok()?;
        crop_click_region(&img, px, py, scale).map(|(jpeg, b64)| {
            let filename = format!("crop_{timestamp}.jpg");
            let path = artifacts_dir.join(&filename);
            let _ = std::fs::write(&path, &jpeg);
            (b64, path)
        })
    })
    .await;
    if let Ok(Some((crop_b64, crop_path))) = crop_result {
        persist_click_crop_event(&app, &storage, &session_dir, crop_b64, crop_path, timestamp);
    }
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
fn encode_cursor_region_crop(
    shot: Arc<CursorRegionCapture>,
    artifacts_dir: std::path::PathBuf,
    timestamp: u64,
) -> Option<(String, std::path::PathBuf)> {
    use base64::Engine;

    let img = image::RgbaImage::from_raw(shot.width, shot.height, shot.rgba_bytes.clone())?;
    let dynamic = image::DynamicImage::ImageRgba8(img);
    let mut jpeg_buf = std::io::Cursor::new(Vec::new());
    dynamic
        .write_to(&mut jpeg_buf, image::ImageFormat::Jpeg)
        .ok()?;
    let jpeg_bytes = jpeg_buf.into_inner();
    let b64 = base64::engine::general_purpose::STANDARD.encode(&jpeg_bytes);
    let filename = format!("crop_{timestamp}.jpg");
    let path = artifacts_dir.join(&filename);
    let _ = std::fs::write(&path, &jpeg_bytes);
    Some((b64, path))
}

fn persist_click_crop_event(
    app: &tauri::AppHandle,
    storage: &WalkthroughStorage,
    session_dir: &std::path::Path,
    b64: String,
    path: std::path::PathBuf,
    timestamp: u64,
) {
    let ev = WalkthroughEvent {
        id: Uuid::new_v4(),
        timestamp,
        kind: WalkthroughEventKind::ScreenshotCaptured {
            path: path.to_string_lossy().to_string(),
            kind: ScreenshotKind::ClickCrop,
            meta: None,
            image_b64: Some(b64),
        },
    };
    persist_and_emit(app, storage, session_dir, &ev);
}

#[allow(clippy::too_many_arguments)]
async fn persist_vlm_click_label(
    app: &tauri::AppHandle,
    storage: &WalkthroughStorage,
    session_dir: &std::path::Path,
    vlm_backend: Option<Arc<clickweave_llm::LlmClient>>,
    app_name: Option<&str>,
    screenshot_path: &str,
    screenshot_meta: ScreenshotMeta,
    ax_label_data: Option<&(String, Option<String>)>,
    has_actionable_ax: bool,
    x: f64,
    y: f64,
    timestamp: u64,
    vlm_timeout: tokio::time::Duration,
) {
    if has_actionable_ax {
        return;
    }
    let Some(backend) = vlm_backend else {
        return;
    };
    let ax_ref = ax_label_data.map(|(label, role)| (label.as_str(), role.as_deref()));
    let Some(req) = prepare_vlm_click_request(
        screenshot_path,
        x,
        y,
        screenshot_meta,
        ax_ref,
        None,
        app_name,
    ) else {
        return;
    };

    let vlm_result = tokio::time::timeout(
        vlm_timeout,
        execute_vlm_click_request(backend.as_ref(), &req),
    )
    .await;

    match vlm_result {
        Ok(Some(label)) => {
            tracing::info!("VLM resolved click at ts={timestamp} -> \"{label}\"");
            let vlm_event = WalkthroughEvent {
                id: Uuid::new_v4(),
                timestamp,
                kind: WalkthroughEventKind::VlmLabelResolved { label },
            };
            persist_and_emit(app, storage, session_dir, &vlm_event);
        }
        Ok(None) => {}
        Err(_) => {
            tracing::warn!("VLM timed out for click at ts={timestamp}");
        }
    }
}
