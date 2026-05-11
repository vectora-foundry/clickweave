use super::*;

async fn initialize_capture_services(
    app: &tauri::AppHandle,
    event_rx: &mut tokio::sync::mpsc::UnboundedReceiver<CaptureEvent>,
    mcp_binary_path: &str,
    supervisor: Option<crate::commands::types::EndpointConfig>,
    session_dir: &std::path::Path,
    cancel: &mut tokio::sync::watch::Receiver<bool>,
    cdp_apps: &[CdpAppConfig],
    hover_dwell_ms: u64,
) -> CaptureServices {
    let mcp_raw = spawn_mcp(mcp_binary_path).await;
    let cdp_state =
        initialize_cdp_capture(app, event_rx, cancel, cdp_apps, &mcp_raw, hover_dwell_ms).await;
    let mcp = mcp_raw.map(Arc::new);
    let vlm_backend = supervisor
        .filter(|s| !s.is_empty())
        .map(|s| Arc::new(clickweave_llm::LlmClient::new(vlm_capture_config(s))));

    if let Some(ref mcp) = mcp {
        start_native_hover_tracking(mcp).await;
        start_continuous_recording(mcp, session_dir).await;
    }

    CaptureServices {
        mcp,
        cdp_state,
        vlm_backend,
    }
}

async fn initialize_cdp_capture(
    app: &tauri::AppHandle,
    event_rx: &mut tokio::sync::mpsc::UnboundedReceiver<CaptureEvent>,
    cancel: &mut tokio::sync::watch::Receiver<bool>,
    cdp_apps: &[CdpAppConfig],
    mcp_raw: &Option<McpClient>,
    hover_dwell_ms: u64,
) -> HashMap<String, CdpAppState> {
    let cdp_state = if cdp_apps.is_empty() {
        HashMap::new()
    } else if let Some(mcp) = mcp_raw {
        setup_cdp_apps(cdp_apps, mcp, app, cancel, hover_dwell_ms).await
    } else {
        tracing::warn!("No MCP server available for CDP setup");
        for cdp_app in cdp_apps {
            emit_cdp_progress(
                app,
                &cdp_app.name,
                CdpSetupStatus::Failed {
                    reason: "MCP server unavailable".to_string(),
                },
            );
        }
        HashMap::new()
    };

    if !cdp_apps.is_empty() {
        emit_cdp_progress(app, "", CdpSetupStatus::Done);
        while event_rx.try_recv().is_ok() {}
    }
    cdp_state
}

fn vlm_capture_config(
    supervisor: crate::commands::types::EndpointConfig,
) -> clickweave_llm::LlmConfig {
    supervisor
        .into_llm_config(Some(0.0))
        .with_max_tokens(2048)
        .with_thinking(false)
}

async fn start_native_hover_tracking(mcp: &McpClient) {
    let hover_args = serde_json::json!({
        "min_dwell_ms": 100,
        "poll_interval_ms": 100,
        "max_duration_ms": 600_000,
    });
    if let Err(e) = mcp
        .call_tool("start_hover_tracking", Some(hover_args))
        .await
    {
        tracing::warn!("Failed to start hover tracking: {e}");
    }
}

async fn start_continuous_recording(mcp: &McpClient, session_dir: &std::path::Path) {
    let artifacts_dir = session_dir.join("artifacts");
    let recording_args = serde_json::json!({
        "output_dir": artifacts_dir.to_string_lossy(),
        "max_duration_ms": 600_000,
    });
    match mcp.call_tool("start_recording", Some(recording_args)).await {
        Ok(r) if r.is_error != Some(true) => {
            tracing::info!("Continuous recording started")
        }
        Ok(r) => {
            let msg: String = r.content.iter().filter_map(|c| c.as_text()).collect();
            tracing::warn!("start_recording returned error (non-fatal): {msg}");
        }
        Err(e) => tracing::warn!("Failed to start recording (non-fatal): {e}"),
    }
}

/// Process captured events: enrich with MCP data, persist, and emit to frontend.
///
/// Click enrichment (screenshot + accessibility + VLM) runs in background tasks
/// so the event loop never blocks on MCP calls and captures every click.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn process_capture_events(
    app: tauri::AppHandle,
    mut event_rx: tokio::sync::mpsc::UnboundedReceiver<CaptureEvent>,
    mcp_binary_path: String,
    supervisor: Option<crate::commands::types::EndpointConfig>,
    storage: WalkthroughStorage,
    session_dir: std::path::PathBuf,
    mut cancel: tokio::sync::watch::Receiver<bool>,
    cdp_apps: Vec<CdpAppConfig>,
    hover_dwell_ms: u64,
) {
    let CaptureServices {
        mcp,
        cdp_state,
        vlm_backend,
    } = initialize_capture_services(
        &app,
        &mut event_rx,
        &mcp_binary_path,
        supervisor,
        &session_dir,
        &mut cancel,
        &cdp_apps,
        hover_dwell_ms,
    )
    .await;

    // Background tasks for click enrichment and VLM resolution.
    // Each task persists and emits its own events; the event loop
    // only needs to drain completions to detect errors.
    let mut bg_tasks: tokio::task::JoinSet<()> = tokio::task::JoinSet::new();

    // Sequential CDP click retrieval channel. System clicks arrive in order
    // and the JS listener pushes in order, so a single consumer drains the
    // entries in FIFO order.
    let (cdp_tx, cdp_rx) = tokio::sync::mpsc::unbounded_channel::<CdpClickRequest>();

    // Screenshot buffer: a small (64pt / 128px on Retina) region around the
    // cursor, captured every 100ms. Used as the crop source for clicks —
    // always reflects what the user sees before hover effects from the click.
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    let screenshot_buffer: ScreenshotBuffer = Arc::new(RwLock::new(None));

    // Spawn a background task that continuously captures the region under the
    // cursor. Aborted when the event loop exits.
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    let cursor_poll_handle = spawn_cursor_polling(screenshot_buffer.clone());

    // Cache PID → app info to avoid repeated lookups.
    let mut app_cache: HashMap<i32, CachedApp> = HashMap::new();
    let app_kind_cache: Arc<Mutex<HashMap<i32, AppKind>>> = Arc::new(Mutex::new(HashMap::new()));
    let mut focus_state = CaptureFocusState::default();

    if let Some(ref mcp) = mcp {
        populate_app_cache(mcp, &mut app_cache).await;
    }

    // Spawn the sequential CDP click consumer.  It processes requests in FIFO
    // order so each shift() retrieves the entry that matches the system click.
    let cdp_consumer_handle = spawn_cdp_click_consumer(
        mcp.clone(),
        app.clone(),
        storage.clone(),
        session_dir.clone(),
        cdp_rx,
    );

    while let Some(capture) = next_capture_event(&mut event_rx, &mut cancel, &mut bg_tasks).await {
        // Detect app focus changes.
        if handle_capture_focus_change(
            &capture,
            &mcp,
            &mut app_cache,
            &app_kind_cache,
            &mut focus_state,
            &app,
            &storage,
            &session_dir,
        )
        .await
        {
            continue;
        }

        // Skip events while our own app is focused.
        if focus_state.self_focused {
            continue;
        }

        // Translate the capture event into a walkthrough event.
        let wt_event = match capture.kind {
            CaptureEventKind::MouseClick {
                x,
                y,
                button,
                click_count,
                modifiers,
            } => {
                let click_event = WalkthroughEvent {
                    id: Uuid::new_v4(),
                    timestamp: capture.timestamp,
                    kind: WalkthroughEventKind::MouseClicked {
                        x,
                        y,
                        button,
                        click_count,
                        modifiers,
                    },
                };

                // Persist the click event immediately so it's never lost.
                persist_and_emit(&app, &storage, &session_dir, &click_event);

                spawn_click_enrichment(
                    &mcp,
                    &vlm_backend,
                    &cdp_state,
                    &cdp_tx,
                    &mut bg_tasks,
                    &app_cache,
                    &app_kind_cache,
                    &app,
                    &storage,
                    &session_dir,
                    &click_event,
                    capture.target_pid,
                    capture.timestamp,
                    x,
                    y,
                    #[cfg(any(target_os = "macos", target_os = "windows"))]
                    &screenshot_buffer,
                );

                continue;
            }

            CaptureEventKind::KeyDown {
                key_name,
                characters,
                modifiers,
            } => {
                // If the key produces printable text and has no command/control
                // modifiers, emit TextCommitted instead of KeyPressed.
                let has_command_modifiers =
                    modifiers.iter().any(|m| m == "command" || m == "control");
                let is_printable = !has_command_modifiers
                    && characters
                        .as_ref()
                        .is_some_and(|t| !t.is_empty() && t.chars().all(|c| !c.is_control()));

                let kind = if is_printable {
                    WalkthroughEventKind::TextCommitted {
                        text: characters.unwrap(),
                    }
                } else {
                    WalkthroughEventKind::KeyPressed {
                        key: key_name,
                        modifiers,
                    }
                };

                WalkthroughEvent {
                    id: Uuid::new_v4(),
                    timestamp: capture.timestamp,
                    kind,
                }
            }

            CaptureEventKind::ScrollWheel { delta_y, x, y } => WalkthroughEvent {
                id: Uuid::new_v4(),
                timestamp: capture.timestamp,
                kind: WalkthroughEventKind::Scrolled {
                    delta_y,
                    x: Some(x),
                    y: Some(y),
                },
            },
        };

        persist_and_emit(&app, &storage, &session_dir, &wt_event);
    }

    drop(cdp_tx);
    drain_capture_tasks(cdp_consumer_handle, bg_tasks).await;

    // Stop the cursor region polling task.
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    cursor_poll_handle.abort();

    if let Some(ref mcp) = mcp {
        stop_recording_and_persist_frames(mcp, &session_dir).await;
        persist_native_hover_events(mcp, &app, &storage, &session_dir).await;
        persist_cdp_hover_events(mcp, &app, &storage, &session_dir, &cdp_state).await;
    }

    tracing::info!("Walkthrough capture event loop ended");
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
fn spawn_cursor_polling(screenshot_buffer: ScreenshotBuffer) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
            let buf = screenshot_buffer.clone();
            let _ = tokio::task::spawn_blocking(move || {
                let (cx, cy) = crate::platform::get_cursor_position();
                if let Some(shot) = crate::platform::capture_cursor_region(cx, cy)
                    && let Ok(mut guard) = buf.write()
                {
                    *guard = Some(Arc::new(shot));
                }
            })
            .await;
        }
    })
}

fn spawn_cdp_click_consumer(
    mcp: Option<Arc<McpClient>>,
    app: tauri::AppHandle,
    storage: WalkthroughStorage,
    session_dir: std::path::PathBuf,
    mut rx: tokio::sync::mpsc::UnboundedReceiver<CdpClickRequest>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        while let Some(req) = rx.recv().await {
            if let Some(ref mcp) = mcp {
                cdp_retrieve_click(
                    mcp,
                    req.port,
                    req.selected_page_url.as_deref(),
                    &app,
                    &storage,
                    &session_dir,
                    req.click_event_id,
                    req.click_timestamp,
                )
                .await;
            }
        }
    })
}

async fn next_capture_event(
    event_rx: &mut tokio::sync::mpsc::UnboundedReceiver<CaptureEvent>,
    cancel: &mut tokio::sync::watch::Receiver<bool>,
    bg_tasks: &mut tokio::task::JoinSet<()>,
) -> Option<CaptureEvent> {
    loop {
        tokio::select! {
            biased;
            _ = cancel.changed() => return None,
            Some(result) = bg_tasks.join_next() => {
                if let Err(e) = result {
                    tracing::warn!("Background enrichment task panicked: {e}");
                }
            }
            msg = event_rx.recv() => return msg,
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn handle_capture_focus_change(
    capture: &CaptureEvent,
    mcp: &Option<Arc<McpClient>>,
    app_cache: &mut HashMap<i32, CachedApp>,
    app_kind_cache: &Arc<Mutex<HashMap<i32, AppKind>>>,
    focus_state: &mut CaptureFocusState,
    app: &tauri::AppHandle,
    storage: &WalkthroughStorage,
    session_dir: &std::path::Path,
) -> bool {
    if capture.target_pid == 0 || capture.target_pid == focus_state.last_pid {
        return false;
    }

    let app_name = resolve_app_name(capture.target_pid, mcp, app_cache).await;

    // Skip events targeting our own app (recording bar clicks, etc.).
    // We track focus but don't emit the AppFocused event for ourselves.
    if app_name == SELF_APP_NAME {
        focus_state.last_pid = capture.target_pid;
        focus_state.self_focused = true;
        return true;
    }

    let app_kind = app_kind_for_capture(capture.target_pid, &app_name, app_cache, app_kind_cache);
    let focus_event = WalkthroughEvent {
        id: Uuid::new_v4(),
        timestamp: capture.timestamp,
        kind: WalkthroughEventKind::AppFocused {
            app_name: app_name.clone(),
            pid: capture.target_pid,
            window_title: None,
            app_kind,
        },
    };
    persist_and_emit(app, storage, session_dir, &focus_event);
    focus_state.last_pid = capture.target_pid;
    focus_state.self_focused = false;
    false
}

fn app_kind_for_capture(
    target_pid: i32,
    app_name: &str,
    app_cache: &HashMap<i32, CachedApp>,
    app_kind_cache: &Arc<Mutex<HashMap<i32, AppKind>>>,
) -> AppKind {
    let mut cache = app_kind_cache.lock().unwrap();
    if let Some(&cached_kind) = cache.get(&target_pid) {
        return cached_kind;
    }

    let bundle_id = app_cache
        .get(&target_pid)
        .and_then(|c| c.bundle_id.as_deref());
    let bundle_path = bundle_path_from_pid(target_pid);
    let kind = classify_app(bundle_id, bundle_path.as_deref());
    if kind != AppKind::Native {
        tracing::info!(
            "App '{}' (PID {}) classified as {:?}",
            app_name,
            target_pid,
            kind,
        );
    }
    cache.insert(target_pid, kind);
    kind
}

#[allow(clippy::too_many_arguments)]
fn spawn_click_enrichment(
    mcp: &Option<Arc<McpClient>>,
    vlm_backend: &Option<Arc<clickweave_llm::LlmClient>>,
    cdp_state: &HashMap<String, CdpAppState>,
    cdp_tx: &tokio::sync::mpsc::UnboundedSender<CdpClickRequest>,
    bg_tasks: &mut tokio::task::JoinSet<()>,
    app_cache: &HashMap<i32, CachedApp>,
    app_kind_cache: &Arc<Mutex<HashMap<i32, AppKind>>>,
    app: &tauri::AppHandle,
    storage: &WalkthroughStorage,
    session_dir: &std::path::Path,
    click_event: &WalkthroughEvent,
    target_pid: i32,
    timestamp: u64,
    x: f64,
    y: f64,
    #[cfg(any(target_os = "macos", target_os = "windows"))] screenshot_buffer: &ScreenshotBuffer,
) {
    let Some(mcp_arc) = mcp else {
        return;
    };

    let task_app_name = app_cache.get(&target_pid).map(|c| c.name.clone());
    queue_cdp_click_resolution(
        cdp_state,
        cdp_tx,
        task_app_name.as_deref(),
        click_event.id,
        timestamp,
    );

    let task_mcp = mcp_arc.clone();
    let task_vlm = vlm_backend.clone();
    let task_app = app.clone();
    let task_storage = storage.clone();
    let task_dir = session_dir.to_path_buf();
    let task_kind_cache = app_kind_cache.clone();
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    let task_prehover = screenshot_buffer.read().ok().and_then(|g| g.clone());

    bg_tasks.spawn(async move {
        enrich_click_background(
            task_mcp,
            task_vlm,
            task_app,
            task_storage,
            task_dir,
            task_app_name,
            x,
            y,
            timestamp,
            VLM_CALL_TIMEOUT,
            task_kind_cache,
            target_pid,
            #[cfg(any(target_os = "macos", target_os = "windows"))]
            task_prehover,
        )
        .await;
    });
}

fn queue_cdp_click_resolution(
    cdp_state: &HashMap<String, CdpAppState>,
    cdp_tx: &tokio::sync::mpsc::UnboundedSender<CdpClickRequest>,
    app_name: Option<&str>,
    click_event_id: Uuid,
    click_timestamp: u64,
) {
    let Some(app_state) = app_name.and_then(|name| cdp_state.get(name)) else {
        return;
    };

    let _ = cdp_tx.send(CdpClickRequest {
        port: app_state.port,
        selected_page_url: app_state.selected_page_url.clone(),
        click_event_id,
        click_timestamp,
    });
}

async fn drain_capture_tasks(
    cdp_consumer_handle: tokio::task::JoinHandle<()>,
    mut bg_tasks: tokio::task::JoinSet<()>,
) {
    let drain_deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(30);
    if tokio::time::timeout_at(drain_deadline, cdp_consumer_handle)
        .await
        .is_err()
    {
        tracing::warn!("CDP consumer drain timeout reached");
    }

    loop {
        match tokio::time::timeout_at(drain_deadline, bg_tasks.join_next()).await {
            Ok(Some(Ok(()))) => {}
            Ok(Some(Err(e))) => tracing::warn!("Enrichment task panicked: {e}"),
            Ok(None) => break,
            Err(_) => {
                let remaining = bg_tasks.len();
                tracing::warn!("Drain timeout reached, aborting {remaining} enrichment task(s)");
                bg_tasks.abort_all();
                break;
            }
        }
    }
}
