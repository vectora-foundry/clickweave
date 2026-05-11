use super::*;

pub(super) async fn stop_recording_and_persist_frames(
    mcp: &McpClient,
    session_dir: &std::path::Path,
) {
    let recording_timeout = tokio::time::Duration::from_secs(10);
    match tokio::time::timeout(recording_timeout, mcp.call_tool("stop_recording", None)).await {
        Ok(Ok(result)) if result.is_error != Some(true) => {
            let frames =
                crate::commands::walkthrough_enrichment::parse_recording_frames(&result.content);
            tracing::info!("Recording stopped, got {} frames", frames.len());
            let frames_path = session_dir.join("recording_frames.json");
            match serde_json::to_string_pretty(&frames) {
                Ok(json) => {
                    if let Err(e) = std::fs::write(&frames_path, json) {
                        tracing::warn!("Failed to write recording frames: {e}");
                    }
                }
                Err(e) => tracing::warn!("Failed to serialize recording frames: {e}"),
            }
        }
        Ok(Ok(_)) => {
            tracing::debug!("stop_recording returned error (may not have been active)");
        }
        Ok(Err(e)) => {
            tracing::debug!("stop_recording call failed: {e}");
        }
        Err(_) => {
            tracing::warn!("stop_recording timed out after {recording_timeout:?}");
        }
    }
}

pub(super) async fn persist_native_hover_events(
    mcp: &McpClient,
    app: &tauri::AppHandle,
    storage: &WalkthroughStorage,
    session_dir: &std::path::Path,
) {
    let hover_timeout = tokio::time::Duration::from_secs(5);
    match tokio::time::timeout(hover_timeout, mcp.call_tool("stop_hover_tracking", None)).await {
        Ok(Ok(result)) if result.is_error != Some(true) => {
            let raw_text: String = result.content.iter().filter_map(|c| c.as_text()).collect();
            match serde_json::from_str::<Vec<serde_json::Value>>(&raw_text) {
                Ok(events) => persist_native_hover_json(app, storage, session_dir, events),
                Err(e) => tracing::warn!("Failed to parse hover tracking response: {e}"),
            }
        }
        Ok(Ok(_)) => {
            tracing::debug!("stop_hover_tracking returned error (may not have been active)");
        }
        Ok(Err(e)) => {
            tracing::debug!("stop_hover_tracking call failed: {e}");
        }
        Err(_) => {
            tracing::warn!("stop_hover_tracking timed out after {hover_timeout:?}");
        }
    }
}

fn persist_native_hover_json(
    app: &tauri::AppHandle,
    storage: &WalkthroughStorage,
    session_dir: &std::path::Path,
    events: Vec<serde_json::Value>,
) {
    let mut count = 0u32;
    for ev in events {
        if ev.get("timeout").and_then(|v| v.as_bool()) == Some(true) {
            continue;
        }
        let hover_event = WalkthroughEvent {
            id: Uuid::new_v4(),
            timestamp: ev.get("timestamp_ms").and_then(|v| v.as_u64()).unwrap_or(0),
            kind: WalkthroughEventKind::HoverDetected {
                x: ev
                    .pointer("/cursor/x")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(0.0),
                y: ev
                    .pointer("/cursor/y")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(0.0),
                element_name: ev
                    .pointer("/element/name")
                    .and_then(|v| v.as_str())
                    .or_else(|| ev.pointer("/element/label").and_then(|v| v.as_str()))
                    .unwrap_or("")
                    .to_string(),
                element_role: ev
                    .pointer("/element/role")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string()),
                dwell_ms: ev.get("dwell_ms").and_then(|v| v.as_u64()).unwrap_or(0),
                app_name: ev
                    .pointer("/element/app_name")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string()),
            },
        };
        persist_and_emit(app, storage, session_dir, &hover_event);
        count += 1;
    }
    if count > 0 {
        tracing::info!("Persisted {count} hover events from native tracking");
    }
}

pub(super) async fn persist_cdp_hover_events(
    mcp: &McpClient,
    app: &tauri::AppHandle,
    storage: &WalkthroughStorage,
    session_dir: &std::path::Path,
    cdp_state: &HashMap<String, CdpAppState>,
) {
    for (app_name, app_state) in cdp_state {
        if !reconnect_cdp_for_hover_retrieval(mcp, app_name, app_state).await {
            continue;
        }
        let entries = match retrieve_cdp_hover_entries(mcp, app_name).await {
            Some(entries) => entries,
            None => continue,
        };
        persist_cdp_hover_entries(app, storage, session_dir, app_name, entries);
        let _ = mcp.call_tool("cdp_disconnect", None).await;
    }
}

async fn reconnect_cdp_for_hover_retrieval(
    mcp: &McpClient,
    app_name: &str,
    app_state: &CdpAppState,
) -> bool {
    match mcp
        .call_tool(
            "cdp_connect",
            Some(serde_json::json!({"port": app_state.port})),
        )
        .await
    {
        Err(e) => {
            tracing::debug!("CDP reconnect for hover retrieval failed for '{app_name}': {e}");
            false
        }
        Ok(r) if r.is_error == Some(true) => {
            tracing::debug!("CDP reconnect for hover retrieval rejected for '{app_name}'");
            false
        }
        Ok(_) => {
            if let Some(url) = app_state.selected_page_url.as_deref() {
                restore_selected_page(mcp, url).await;
            }
            true
        }
    }
}

async fn retrieve_cdp_hover_entries(
    mcp: &McpClient,
    app_name: &str,
) -> Option<Vec<serde_json::Value>> {
    let stop_args = serde_json::json!({ "function": CDP_STOP_HOVER_JS });
    let _ = mcp.call_tool("cdp_evaluate_script", Some(stop_args)).await;

    let retrieve_args = serde_json::json!({ "function": CDP_RETRIEVE_HOVERS_JS });
    let result = match tokio::time::timeout(
        CDP_SNAPSHOT_TIMEOUT,
        mcp.call_tool("cdp_evaluate_script", Some(retrieve_args)),
    )
    .await
    {
        Ok(Ok(r)) if r.is_error != Some(true) => r,
        Ok(Ok(_)) | Ok(Err(_)) | Err(_) => {
            tracing::debug!("CDP hover retrieve failed for '{app_name}'");
            return None;
        }
    };

    let raw: String = result.content.iter().filter_map(|c| c.as_text()).collect();
    serde_json::from_str(raw.trim()).ok()
}

fn persist_cdp_hover_entries(
    app: &tauri::AppHandle,
    storage: &WalkthroughStorage,
    session_dir: &std::path::Path,
    app_name: &str,
    entries: Vec<serde_json::Value>,
) {
    let mut count = 0u32;
    for entry in entries {
        let label = entry["textContent"]
            .as_str()
            .or_else(|| entry["ariaLabel"].as_str())
            .filter(|s| !s.is_empty());
        let Some(label) = label else { continue };

        let ts = entry["ts"].as_u64().unwrap_or(0);
        let dwell_ms = entry["dwellMs"].as_u64().unwrap_or(0);
        let hover_id = Uuid::new_v4();
        let hover_event = WalkthroughEvent {
            id: hover_id,
            timestamp: ts,
            kind: WalkthroughEventKind::HoverDetected {
                x: entry["x"].as_f64().unwrap_or(0.0),
                y: entry["y"].as_f64().unwrap_or(0.0),
                element_name: label.to_string(),
                element_role: entry["role"].as_str().map(|s| s.to_string()),
                dwell_ms,
                app_name: Some(app_name.to_string()),
            },
        };
        persist_and_emit(app, storage, session_dir, &hover_event);

        let cdp_event = WalkthroughEvent {
            id: Uuid::new_v4(),
            timestamp: ts,
            kind: WalkthroughEventKind::CdpHoverResolved {
                hover_event_id: hover_id,
                name: label.to_string(),
                role: entry["role"].as_str().map(|s| s.to_string()),
                href: entry["href"].as_str().map(|s| s.to_string()),
                parent_role: entry["parentRole"].as_str().map(|s| s.to_string()),
                parent_name: entry["parentName"].as_str().map(|s| s.to_string()),
            },
        };
        persist_and_emit(app, storage, session_dir, &cdp_event);
        count += 1;
    }
    if count > 0 {
        tracing::info!("Persisted {count} CDP hover events from '{app_name}'");
    }
}
