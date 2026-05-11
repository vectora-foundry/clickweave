use super::*;

pub(super) async fn cdp_retrieve_click(
    mcp: &McpClient,
    port: u16,
    selected_page_url: Option<&str>,
    app: &tauri::AppHandle,
    storage: &WalkthroughStorage,
    session_dir: &std::path::Path,
    click_event_id: Uuid,
    click_timestamp: u64,
) {
    // Reconnect to this app's CDP port.
    match mcp
        .call_tool("cdp_connect", Some(serde_json::json!({"port": port})))
        .await
    {
        Err(e) => {
            tracing::debug!("CDP reconnect for click retrieve failed for {click_event_id}: {e}");
            return;
        }
        Ok(r) if r.is_error == Some(true) => {
            tracing::debug!("CDP reconnect for click retrieve rejected for {click_event_id}");
            return;
        }
        Ok(_) => {}
    }

    // Restore the tab the listener was injected into. `cdp_connect` auto-
    // selects the first non-extension page, which may not be the user's
    // working tab when multiple tabs are open.
    if let Some(url) = selected_page_url {
        restore_selected_page(mcp, url).await;
    }

    // Poll the click queue with retries.  The macOS event tap fires before the
    // click is delivered to the app, so the JS click event may not have pushed
    // to the queue yet on the first attempt.
    const POLL_DELAYS_MS: &[u64] = &[100, 200, 300, 400];
    let mut text = String::new();

    for (attempt, &delay_ms) in POLL_DELAYS_MS.iter().enumerate() {
        tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;

        let retrieve_args = serde_json::json!({ "function": CDP_RETRIEVE_CLICK_JS });
        let call_fut = mcp.call_tool("cdp_evaluate_script", Some(retrieve_args));
        let result = match tokio::time::timeout(CDP_SNAPSHOT_TIMEOUT, call_fut).await {
            Ok(Ok(r)) if r.is_error != Some(true) => r,
            Ok(Ok(r)) => {
                let err: String = r.content.iter().filter_map(|c| c.as_text()).collect();
                tracing::debug!("CDP click retrieve error for {click_event_id}: {err}");
                return;
            }
            Ok(Err(e)) => {
                tracing::debug!("CDP click retrieve failed for {click_event_id}: {e}");
                return;
            }
            Err(_) => {
                tracing::debug!("CDP click retrieve timed out for {click_event_id}");
                return;
            }
        };

        let raw_text: String = result.content.iter().filter_map(|c| c.as_text()).collect();
        text = raw_text.trim().to_string();
        if text != "null" && text != "undefined" && !text.is_empty() {
            break;
        }

        if attempt < POLL_DELAYS_MS.len() - 1 {
            tracing::debug!(
                "CDP click queue empty for {click_event_id} (attempt {}), retrying",
                attempt + 1
            );
        }
    }

    if text == "null" || text == "undefined" || text.is_empty() {
        tracing::debug!("CDP click queue empty after all retries for {click_event_id}");

        // Check listener health and re-inject if lost (single MCP call).
        let check_args = serde_json::json!({ "function": CDP_CHECK_AND_REINJECT_JS });
        match mcp.call_tool("cdp_evaluate_script", Some(check_args)).await {
            Ok(r) => {
                let raw: String = r.content.iter().filter_map(|c| c.as_text()).collect();
                let status = raw.trim();
                if status.contains("reinjected") {
                    tracing::info!("CDP click listener lost after navigation, re-injected");
                }
            }
            Err(e) => tracing::warn!("CDP click listener health check failed: {e}"),
        }
        return;
    }

    // Parse the JSON result from evaluate_script.
    let parsed: serde_json::Value = match serde_json::from_str(&text) {
        Ok(v) => v,
        Err(e) => {
            tracing::debug!("CDP click data parse failed for {click_event_id}: {e}");
            return;
        }
    };

    // Delegate element name/role extraction to the library crate.
    let Some((name, role, href, parent_role, parent_name)) = parse_cdp_click_data(&parsed) else {
        tracing::debug!("CDP click data empty for {click_event_id}");
        return;
    };

    // Log fallback usage for debugging.
    let has_text_name = parsed["ariaLabel"]
        .as_str()
        .filter(|s| !s.is_empty())
        .or_else(|| parsed["textContent"].as_str().filter(|s| !s.is_empty()))
        .or_else(|| parsed["value"].as_str().filter(|s| !s.is_empty()))
        .or_else(|| parsed["title"].as_str().filter(|s| !s.is_empty()))
        .is_some();
    if !has_text_name {
        tracing::debug!("CDP click has no text name for {click_event_id}, using fallback: {name}");
    }

    tracing::info!(
        "CDP resolved click {click_event_id} → name={:?} role={:?}",
        name,
        role
    );

    let event = WalkthroughEvent {
        id: Uuid::new_v4(),
        timestamp: click_timestamp,
        kind: WalkthroughEventKind::CdpClickResolved {
            name,
            role,
            href,
            parent_role,
            parent_name,
            click_event_id,
        },
    };
    persist_and_emit(app, storage, session_dir, &event);
}

// ---------------------------------------------------------------------------
// MCP helpers
// ---------------------------------------------------------------------------
