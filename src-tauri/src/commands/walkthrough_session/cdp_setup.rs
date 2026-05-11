use super::*;

async fn existing_debug_port(app_name: &str) -> Option<u16> {
    let output = tokio::process::Command::new("pgrep")
        .args(["-x", app_name])
        .output()
        .await
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let pids = String::from_utf8_lossy(&output.stdout);
    for pid_str in pids.split_whitespace() {
        let pid: u32 = pid_str.parse().ok()?;
        let args_output = tokio::process::Command::new("ps")
            .args(["-p", &pid.to_string(), "-o", "args="])
            .output()
            .await
            .ok()?;
        let args = String::from_utf8_lossy(&args_output.stdout);
        if let Some(flag) = args
            .split_whitespace()
            .find(|a| a.starts_with("--remote-debugging-port="))
            && let Some(port_str) = flag.strip_prefix("--remote-debugging-port=")
            && let Ok(port) = port_str.parse::<u16>()
        {
            return Some(port);
        }
    }
    None
}

pub(super) use clickweave_core::cdp::rand_ephemeral_port;

/// Set up CDP connections for user-selected apps.
///
/// For each app: quit the running instance, relaunch with
/// `--remote-debugging-port`, connect via `cdp_connect`, inject
/// listeners, and disconnect. Returns a map of app_name → CDP port.
pub(super) async fn setup_cdp_apps(
    cdp_apps: &[CdpAppConfig],
    mcp: &McpClient,
    app: &tauri::AppHandle,
    cancel: &mut tokio::sync::watch::Receiver<bool>,
    hover_dwell_ms: u64,
) -> HashMap<String, CdpAppState> {
    let mut state: HashMap<String, CdpAppState> = HashMap::new();

    if !mcp.has_tool("cdp_connect") {
        tracing::warn!(
            "MCP server does not support CDP tools (cdp_connect not available). \
             Skipping CDP setup for {} app(s).",
            cdp_apps.len()
        );
        return state;
    }

    for cdp_app in cdp_apps {
        if *cancel.borrow() {
            break;
        }

        let Some(port) = prepare_cdp_recording_port(cdp_app, mcp, app).await else {
            continue;
        };

        emit_cdp_progress(app, &cdp_app.name, CdpSetupStatus::Connecting);
        match connect_cdp_for_setup(mcp, port, &cdp_app.name, cancel).await {
            CdpConnectOutcome::Ready => {}
            CdpConnectOutcome::Cancelled => break,
            CdpConnectOutcome::Failed(reason) => {
                tracing::warn!("CDP connect failed for '{}': {}", cdp_app.name, reason);
                emit_cdp_progress(app, &cdp_app.name, CdpSetupStatus::Failed { reason });
                continue;
            }
        }

        match install_cdp_recording_listeners(mcp, &cdp_app.name, hover_dwell_ms).await {
            Ok(selected_page_url) => {
                emit_cdp_progress(app, &cdp_app.name, CdpSetupStatus::Ready);
                state.insert(
                    cdp_app.name.clone(),
                    CdpAppState {
                        port,
                        selected_page_url,
                    },
                );
            }
            Err(reason) => emit_cdp_progress(app, &cdp_app.name, CdpSetupStatus::Failed { reason }),
        }
    }

    state
}

async fn prepare_cdp_recording_port(
    cdp_app: &CdpAppConfig,
    mcp: &McpClient,
    app: &tauri::AppHandle,
) -> Option<u16> {
    if let Some(port) = existing_debug_port(&cdp_app.name).await {
        tracing::info!(
            "'{}' already running with --remote-debugging-port={}, reusing",
            cdp_app.name,
            port
        );
        emit_cdp_progress(app, &cdp_app.name, CdpSetupStatus::Connecting);
        return Some(port);
    }

    let port = rand_ephemeral_port();
    if cdp_app.binary_path.is_some() {
        emit_cdp_progress(app, &cdp_app.name, CdpSetupStatus::Launching);
    } else {
        emit_cdp_progress(app, &cdp_app.name, CdpSetupStatus::Restarting);
    }

    quit_existing_cdp_app(mcp, &cdp_app.name).await;
    if !wait_for_app_exit(mcp, &cdp_app.name).await {
        force_quit_cdp_app(mcp, &cdp_app.name).await;
    }
    if !launch_cdp_app(mcp, cdp_app, port, app).await {
        return None;
    }

    tokio::time::sleep(std::time::Duration::from_secs(3)).await;
    Some(port)
}

async fn quit_existing_cdp_app(mcp: &McpClient, app_name: &str) {
    let quit_args = serde_json::json!({ "app_name": app_name });
    match mcp.call_tool("quit_app", Some(quit_args)).await {
        Ok(r) if r.is_error == Some(true) => {
            tracing::debug!(
                "quit_app for '{}' returned error (may not be running)",
                app_name
            );
        }
        Err(e) => {
            tracing::debug!("quit_app for '{}' failed: {e}", app_name);
        }
        _ => {}
    }
}

async fn wait_for_app_exit(mcp: &McpClient, app_name: &str) -> bool {
    let poll_args = serde_json::json!({ "app_name": app_name, "user_apps_only": true });
    for _ in 0..20 {
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        if let Ok(r) = mcp.call_tool("list_apps", Some(poll_args.clone())).await {
            let text = r
                .content
                .iter()
                .filter_map(|c| c.as_text())
                .collect::<String>();
            if text.trim() == "[]" {
                return true;
            }
        }
    }
    false
}

async fn force_quit_cdp_app(mcp: &McpClient, app_name: &str) {
    tracing::warn!("'{}' did not quit within 10s, force-killing", app_name);
    let force_args = serde_json::json!({ "app_name": app_name, "force": true });
    let _ = mcp.call_tool("quit_app", Some(force_args)).await;
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
}

async fn launch_cdp_app(
    mcp: &McpClient,
    cdp_app: &CdpAppConfig,
    port: u16,
    app: &tauri::AppHandle,
) -> bool {
    let launch_args = if let Some(ref binary_path) = cdp_app.binary_path {
        serde_json::json!({
            "app_name": binary_path,
            "args": [format!("--remote-debugging-port={}", port)],
        })
    } else {
        serde_json::json!({
            "app_name": &cdp_app.name,
            "args": [format!("--remote-debugging-port={}", port)],
        })
    };

    match mcp.call_tool("launch_app", Some(launch_args)).await {
        Err(e) => {
            tracing::warn!("Failed to launch '{}' with CDP: {}", cdp_app.name, e);
            emit_cdp_progress(
                app,
                &cdp_app.name,
                CdpSetupStatus::Failed {
                    reason: e.to_string(),
                },
            );
            false
        }
        Ok(r) if r.is_error == Some(true) => {
            let reason = r
                .content
                .iter()
                .filter_map(|c| c.as_text())
                .collect::<Vec<_>>()
                .join("; ");
            tracing::warn!("launch_app for '{}' returned error: {reason}", cdp_app.name);
            emit_cdp_progress(app, &cdp_app.name, CdpSetupStatus::Failed { reason });
            false
        }
        _ => true,
    }
}

async fn connect_cdp_for_setup(
    mcp: &McpClient,
    port: u16,
    app_name: &str,
    cancel: &mut tokio::sync::watch::Receiver<bool>,
) -> CdpConnectOutcome {
    tokio::select! {
        biased;
        _ = cancel.changed() => {
            tracing::info!("CDP setup cancelled during connect for '{}'", app_name);
            CdpConnectOutcome::Cancelled
        }
        result = poll_cdp_ready(mcp, port, 10) => match result {
            Ok(()) => {
                tracing::info!("CDP connected to '{}' (port {})", app_name, port);
                CdpConnectOutcome::Ready
            }
            Err(reason) => CdpConnectOutcome::Failed(reason),
        },
    }
}

async fn install_cdp_recording_listeners(
    mcp: &McpClient,
    app_name: &str,
    hover_dwell_ms: u64,
) -> Result<Option<String>, String> {
    let inject_ok = inject_cdp_click_listener(mcp, app_name).await;
    if inject_ok {
        inject_cdp_hover_listener(mcp, app_name, hover_dwell_ms).await;
    }

    let selected_page_url = current_selected_page_url(mcp).await;
    let _ = mcp.call_tool("cdp_disconnect", None).await;

    if inject_ok {
        Ok(selected_page_url)
    } else {
        Err("Click listener injection failed".to_string())
    }
}

async fn inject_cdp_click_listener(mcp: &McpClient, app_name: &str) -> bool {
    let inject_args = serde_json::json!({ "function": CDP_CLICK_LISTENER_JS });
    match mcp
        .call_tool("cdp_evaluate_script", Some(inject_args))
        .await
    {
        Ok(r) if r.is_error != Some(true) => {
            tracing::info!("Injected click listener into '{}'", app_name);
            true
        }
        Ok(r) => {
            let err: String = r.content.iter().filter_map(|c| c.as_text()).collect();
            tracing::warn!(
                "CDP click listener injection rejected for '{}': {err}",
                app_name
            );
            false
        }
        Err(e) => {
            tracing::warn!("Failed to inject click listener into '{}': {e}", app_name);
            false
        }
    }
}

async fn inject_cdp_hover_listener(mcp: &McpClient, app_name: &str, hover_dwell_ms: u64) {
    let hover_js = CDP_HOVER_LISTENER_JS.replace("__CW_MIN_DWELL__", &hover_dwell_ms.to_string());
    let hover_args = serde_json::json!({ "function": hover_js });
    match mcp.call_tool("cdp_evaluate_script", Some(hover_args)).await {
        Ok(r) if r.is_error != Some(true) => {
            tracing::info!("Injected hover listener into '{}'", app_name);
        }
        Ok(r) => {
            let err: String = r.content.iter().filter_map(|c| c.as_text()).collect();
            tracing::warn!(
                "CDP hover listener injection rejected for '{}': {err}",
                app_name
            );
        }
        Err(e) => {
            tracing::warn!("Failed to inject hover listener into '{}': {e}", app_name);
        }
    }
}

/// Poll `cdp_connect` + `cdp_list_pages` until a page is available.
async fn poll_cdp_ready(mcp: &McpClient, port: u16, timeout_secs: u64) -> Result<(), String> {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(timeout_secs);

    loop {
        // Try connecting to the CDP port.
        match mcp
            .call_tool("cdp_connect", Some(serde_json::json!({"port": port})))
            .await
        {
            Ok(r) if r.is_error != Some(true) => {
                // Connection succeeded — cdp_connect auto-selects the first page.
                return Ok(());
            }
            Ok(r) => {
                let text: String = r
                    .content
                    .iter()
                    .filter_map(|c| c.as_text())
                    .collect::<Vec<_>>()
                    .join("\n");
                tracing::debug!("cdp_connect error for port {port}: {text}");
            }
            Err(e) => {
                tracing::debug!("cdp_connect call failed for port {port}: {e}");
            }
        }

        if tokio::time::Instant::now() >= deadline {
            return Err(format!(
                "Timed out waiting for CDP on port {port} to be ready ({timeout_secs}s)",
            ));
        }

        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }
}

/// Fetch the URL of the currently-selected CDP page, or `None` if the
/// selected page cannot be identified. Used to remember which tab was active
/// at listener-injection time so reconnects can restore it.
async fn current_selected_page_url(mcp: &McpClient) -> Option<String> {
    let result = mcp
        .call_tool("cdp_list_pages", Some(serde_json::json!({})))
        .await
        .ok()?;
    if result.is_error == Some(true) {
        return None;
    }
    let text: String = result.content.iter().filter_map(|c| c.as_text()).collect();
    pick_current_selected_url(&parse_cdp_page_list(&text))
}

/// Restore the previously-selected CDP page by matching URL. If no page
/// matches (tab closed, same origin unreachable), log at debug and leave
/// whatever `cdp_connect` auto-selected — a wrong tab is preferable to
/// halting retrieval.
pub(super) async fn restore_selected_page(mcp: &McpClient, target_url: &str) {
    let list_result = match mcp
        .call_tool("cdp_list_pages", Some(serde_json::json!({})))
        .await
    {
        Ok(r) if r.is_error != Some(true) => r,
        _ => {
            tracing::debug!("Walkthrough CDP restore: cdp_list_pages failed or errored");
            return;
        }
    };
    let text: String = list_result
        .content
        .iter()
        .filter_map(|c| c.as_text())
        .collect();
    let pages = parse_cdp_page_list(&text);
    let Some(target_index) = pick_page_index_for_url(&pages, target_url) else {
        tracing::debug!(
            "Walkthrough CDP restore: no page matched remembered URL {target_url}; \
             falling back to auto-selected tab"
        );
        return;
    };

    // Skip the call when the auto-selected tab already matches.
    if pages
        .iter()
        .find(|p| p.index == target_index)
        .is_some_and(|p| p.selected)
    {
        return;
    }

    match mcp
        .call_tool(
            "cdp_select_page",
            Some(serde_json::json!({ "page_idx": target_index })),
        )
        .await
    {
        Ok(r) if r.is_error != Some(true) => {
            tracing::debug!("Walkthrough CDP restore: selected page [{target_index}] {target_url}");
        }
        Ok(r) => {
            let err: String = r.content.iter().filter_map(|c| c.as_text()).collect();
            tracing::debug!("Walkthrough CDP restore: cdp_select_page rejected: {err}");
        }
        Err(e) => {
            tracing::debug!("Walkthrough CDP restore: cdp_select_page call failed: {e}");
        }
    }
}

pub(super) fn emit_cdp_progress(app: &tauri::AppHandle, app_name: &str, status: CdpSetupStatus) {
    let _ = app.emit(
        "walkthrough://cdp-setup",
        CdpSetupProgress {
            app_name: app_name.to_string(),
            status,
        },
    );
}
