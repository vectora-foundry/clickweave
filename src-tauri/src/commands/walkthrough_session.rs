use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use clickweave_core::app_detection::{bundle_path_from_pid, classify_app, classify_app_by_pid};
use clickweave_core::walkthrough::{
    AppKind, ScreenshotKind, WalkthroughEvent, WalkthroughEventKind, WalkthroughSession,
    WalkthroughStatus, WalkthroughStorage,
};
use clickweave_mcp::McpRouter;
use tauri::{Emitter, Manager};
use uuid::Uuid;

use super::walkthrough::{
    CDP_SNAPSHOT_TIMEOUT, CdpAppConfig, CdpSetupProgress, CdpSetupStatus, RECORDING_BAR_LABEL,
    SELF_APP_NAME, VLM_CALL_TIMEOUT,
};
use super::walkthrough_enrichment::{
    enrich_click, execute_vlm_click_request, prepare_vlm_click_request,
};
use crate::platform::{CaptureCommand, CaptureEvent, CaptureEventKind};

#[cfg(target_os = "macos")]
use crate::platform::macos::{CursorRegionCapture, MacOSEventTap};

#[cfg(target_os = "macos")]
use std::sync::RwLock;

/// Shared buffer holding the most recent cursor region capture (64×64pt around
/// the cursor, polled every 100ms). Used as the click crop template — always
/// reflects the screen before hover effects from the click itself.
///
/// Inner `Arc` avoids cloning the pixel data when reading on click — only an
/// `Arc` pointer bump instead of a 64 KB memcpy.
#[cfg(target_os = "macos")]
type ScreenshotBuffer = Arc<RwLock<Option<Arc<CursorRegionCapture>>>>;

/// Cached info about a running app, populated from MCP's `list_apps` response.
pub(super) struct CachedApp {
    pub(super) name: String,
    pub(super) bundle_id: Option<String>,
}

/// Manages the walkthrough recording lifecycle.
pub struct WalkthroughHandle {
    pub session: Option<WalkthroughSession>,
    pub session_dir: Option<std::path::PathBuf>,
    pub(super) storage: Option<WalkthroughStorage>,
    pub(super) mcp_command: Option<String>,
    #[cfg(target_os = "macos")]
    pub(super) event_tap: Option<MacOSEventTap>,
    pub(super) processing_task: Option<tauri::async_runtime::JoinHandle<()>>,
    /// Cancellation signal for the processing loop.
    pub(super) cancel_tx: tokio::sync::watch::Sender<bool>,
}

impl Default for WalkthroughHandle {
    fn default() -> Self {
        let (cancel_tx, _) = tokio::sync::watch::channel(false);
        Self {
            session: None,
            session_dir: None,
            storage: None,
            mcp_command: None,
            #[cfg(target_os = "macos")]
            event_tap: None,
            processing_task: None,
            cancel_tx,
        }
    }
}

impl WalkthroughHandle {
    pub(super) fn ensure_status(
        &self,
        expected: &[WalkthroughStatus],
    ) -> Result<&WalkthroughSession, String> {
        let session = self
            .session
            .as_ref()
            .ok_or("No walkthrough session is active")?;
        if !expected.contains(&session.status) {
            return Err(format!(
                "Walkthrough is in {:?} state, expected one of {:?}",
                session.status, expected
            ));
        }
        Ok(session)
    }

    /// Stop the capture backend and return the processing task handle.
    ///
    /// Signals the cancellation token so the processing loop exits promptly
    /// (any in-flight MCP call is dropped via `select!`). The caller should
    /// `await` the returned handle for a clean shutdown.
    pub(super) fn stop_capture(&mut self) -> Option<tauri::async_runtime::JoinHandle<()>> {
        let _ = self.cancel_tx.send(true);

        #[cfg(target_os = "macos")]
        if let Some(tap) = self.event_tap.take() {
            tap.send_command(CaptureCommand::Stop);
            // Drop the tap handle — this joins the thread and closes the sender.
            drop(tap);
        }
        self.processing_task.take()
    }
}

// ---------------------------------------------------------------------------
// Async event processing loop
// ---------------------------------------------------------------------------

/// Process captured events: enrich with MCP data, persist, and emit to frontend.
///
/// Click enrichment (screenshot + accessibility + VLM) runs in background tasks
/// so the event loop never blocks on MCP calls and captures every click.
#[allow(clippy::too_many_arguments)]
pub(super) async fn process_capture_events(
    app: tauri::AppHandle,
    mut event_rx: tokio::sync::mpsc::UnboundedReceiver<CaptureEvent>,
    mcp_command: String,
    planner: Option<super::types::EndpointConfig>,
    storage: WalkthroughStorage,
    session_dir: std::path::PathBuf,
    mut cancel: tokio::sync::watch::Receiver<bool>,
    cdp_apps: Vec<CdpAppConfig>,
) {
    // Spawn the MCP server for enrichment (screenshots + OCR).
    let mut mcp_raw = spawn_mcp(&mcp_command).await;

    // Set up CDP servers for selected apps before wrapping in Arc
    // (spawn_server requires &mut).
    let cdp_state: HashMap<String, String> = if !cdp_apps.is_empty() {
        if let Some(ref mut mcp) = mcp_raw {
            setup_cdp_apps(&cdp_apps, mcp, &app, &mut cancel).await
        } else {
            tracing::warn!("No MCP server available for CDP setup");
            for cdp_app in &cdp_apps {
                emit_cdp_progress(
                    &app,
                    &cdp_app.name,
                    CdpSetupStatus::Failed {
                        reason: "MCP server unavailable".to_string(),
                    },
                );
            }
            HashMap::new()
        }
    } else {
        HashMap::new()
    };

    // Signal frontend that CDP setup is complete so the modal can close.
    if !cdp_apps.is_empty() {
        emit_cdp_progress(&app, "", CdpSetupStatus::Done);
    }

    // Drain any events captured during CDP setup (app restarts generate
    // focus/input events that are not user-initiated). Drain even if all
    // setups failed — the quit/relaunch attempt still produces events.
    if !cdp_apps.is_empty() {
        while event_rx.try_recv().is_ok() {}
    }

    // Wrap in Arc so background enrichment tasks can share it.
    let mcp: Option<std::sync::Arc<McpRouter>> = mcp_raw.map(std::sync::Arc::new);

    // Initialize VLM backend if planner config is available.
    let vlm_backend: Option<std::sync::Arc<clickweave_llm::LlmClient>> =
        planner.filter(|p| !p.is_empty()).map(|p| {
            let config = p
                .into_llm_config(Some(0.0))
                .with_max_tokens(2048)
                .with_thinking(false);
            std::sync::Arc::new(clickweave_llm::LlmClient::new(config))
        });

    // Background tasks for click enrichment and VLM resolution.
    // Each task persists and emits its own events; the event loop
    // only needs to drain completions to detect errors.
    let mut bg_tasks: tokio::task::JoinSet<()> = tokio::task::JoinSet::new();

    // Screenshot buffer: a small (64pt / 128px on Retina) region around the
    // cursor, captured every 100ms. Used as the crop source for clicks —
    // always reflects what the user sees before hover effects from the click.
    #[cfg(target_os = "macos")]
    let screenshot_buffer: ScreenshotBuffer = Arc::new(RwLock::new(None));

    // Spawn a background task that continuously captures the region under the
    // cursor. Aborted when the event loop exits.
    #[cfg(target_os = "macos")]
    let cursor_poll_handle = {
        let buf = screenshot_buffer.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
                let buf2 = buf.clone();
                let _ = tokio::task::spawn_blocking(move || {
                    let (cx, cy) = crate::platform::macos::get_cursor_position();
                    if let Some(shot) = crate::platform::macos::capture_cursor_region(cx, cy)
                        && let Ok(mut guard) = buf2.write()
                    {
                        *guard = Some(Arc::new(shot));
                    }
                })
                .await;
            }
        })
    };

    // Cache PID → app info to avoid repeated lookups.
    let mut app_cache: HashMap<i32, CachedApp> = HashMap::new();
    let app_kind_cache: Arc<Mutex<HashMap<i32, AppKind>>> = Arc::new(Mutex::new(HashMap::new()));
    let mut last_pid: i32 = 0;
    let mut self_focused = false;

    if let Some(ref mcp) = mcp {
        populate_app_cache(mcp, &mut app_cache).await;
    }

    'event_loop: loop {
        // Drain completed background tasks and wait for the next capture event.
        let capture = loop {
            tokio::select! {
                biased;
                _ = cancel.changed() => break 'event_loop,
                Some(result) = bg_tasks.join_next() => {
                    if let Err(e) = result {
                        tracing::warn!("Background enrichment task panicked: {e}");
                    }
                    continue;
                }
                msg = event_rx.recv() => match msg {
                    Some(c) => break c,
                    None => break 'event_loop,
                },
            }
        };
        // Detect app focus changes.
        if capture.target_pid != 0 && capture.target_pid != last_pid {
            let app_name = resolve_app_name(capture.target_pid, &mcp, &mut app_cache).await;

            // Skip events targeting our own app (recording bar clicks, etc.).
            // We track focus but don't emit the AppFocused event for ourselves.
            if app_name == SELF_APP_NAME {
                last_pid = capture.target_pid;
                self_focused = true;
                continue;
            }

            // Classify the app's UI framework (Chrome, Electron, or Native).
            let app_kind = {
                let mut cache = app_kind_cache.lock().unwrap();
                if let Some(&cached_kind) = cache.get(&capture.target_pid) {
                    cached_kind
                } else {
                    let bundle_id = app_cache
                        .get(&capture.target_pid)
                        .and_then(|c| c.bundle_id.as_deref());
                    let bundle_path = bundle_path_from_pid(capture.target_pid);
                    let kind = classify_app(bundle_id, bundle_path.as_deref());
                    if kind != AppKind::Native {
                        tracing::info!(
                            "App '{}' (PID {}) classified as {:?}",
                            app_name,
                            capture.target_pid,
                            kind,
                        );
                    }
                    cache.insert(capture.target_pid, kind);
                    kind
                }
            };

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
            persist_and_emit(&app, &storage, &session_dir, &focus_event);
            last_pid = capture.target_pid;
            self_focused = false;
        }

        // Skip events while our own app is focused.
        if self_focused {
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
                        cdp_element: None,
                    },
                };

                // Persist the click event immediately so it's never lost.
                persist_and_emit(&app, &storage, &session_dir, &click_event);

                // Spawn enrichment (screenshot + accessibility + VLM) as a
                // background task so the event loop stays responsive.
                // Only spawn enrichment if MCP is available.
                if let Some(ref mcp_arc) = mcp {
                    let task_mcp = mcp_arc.clone();
                    let task_vlm = vlm_backend.clone();
                    let task_app = app.clone();
                    let task_storage = storage.clone();
                    let task_dir = session_dir.clone();
                    let task_app_name = app_cache.get(&capture.target_pid).map(|c| c.name.clone());
                    let ts = capture.timestamp;
                    let task_kind_cache = app_kind_cache.clone();
                    let task_pid = capture.target_pid;
                    #[cfg(target_os = "macos")]
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
                            ts,
                            VLM_CALL_TIMEOUT,
                            task_kind_cache,
                            task_pid,
                            #[cfg(target_os = "macos")]
                            task_prehover,
                        )
                        .await;
                    });

                    // CDP snapshot (async, independent of AX/VLM enrichment).
                    let focused_app = app_cache.get(&capture.target_pid).map(|c| c.name.as_str());
                    if let Some(app_name) = focused_app {
                        if let Some(server_name) = cdp_state.get(app_name) {
                            tracing::debug!(
                                "CDP snapshot: dispatching for '{}' (server '{}')",
                                app_name,
                                server_name
                            );
                            let task_mcp = mcp_arc.clone();
                            let task_app = app.clone();
                            let task_storage = storage.clone();
                            let task_dir = session_dir.clone();
                            let server = server_name.clone();
                            let click_id = click_event.id;
                            let click_ts = capture.timestamp;

                            bg_tasks.spawn(async move {
                                cdp_snapshot_for_click(
                                    &task_mcp,
                                    &server,
                                    &task_app,
                                    &task_storage,
                                    &task_dir,
                                    click_id,
                                    click_ts,
                                )
                                .await;
                            });
                        } else {
                            tracing::debug!(
                                "CDP snapshot: no server for app '{}', cdp_state keys: {:?}",
                                app_name,
                                cdp_state.keys().collect::<Vec<_>>()
                            );
                        }
                    } else {
                        tracing::debug!(
                            "CDP snapshot: PID {} not in app_cache",
                            capture.target_pid
                        );
                    }
                }

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

    // Await in-flight enrichment tasks so their events are on disk before
    // stop_walkthrough reads them. Bounded by a total drain timeout so a
    // wedged MCP server can't block shutdown indefinitely.
    let drain_deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(30);
    loop {
        match tokio::time::timeout_at(drain_deadline, bg_tasks.join_next()).await {
            Ok(Some(Ok(()))) => {} // task completed successfully
            Ok(Some(Err(e))) => tracing::warn!("Enrichment task panicked: {e}"),
            Ok(None) => break, // all tasks finished
            Err(_) => {
                let remaining = bg_tasks.len();
                tracing::warn!("Drain timeout reached, aborting {remaining} enrichment task(s)");
                bg_tasks.abort_all();
                break;
            }
        }
    }

    // Stop the cursor region polling task.
    #[cfg(target_os = "macos")]
    cursor_poll_handle.abort();

    tracing::info!("Walkthrough capture event loop ended");
}

/// Get the recording bar window's bounds in logical screen coordinates.
///
/// Returns `(x, y, width, height)` if the window exists, or `None` if it has
/// already been closed.
pub(super) fn get_recording_bar_rect(app: &tauri::AppHandle) -> Option<(f64, f64, f64, f64)> {
    let win = app.get_webview_window(RECORDING_BAR_LABEL)?;
    let scale = win.scale_factor().ok()?;
    let pos = win.outer_position().ok()?;
    let size = win.outer_size().ok()?;
    Some((
        pos.x as f64 / scale,
        pos.y as f64 / scale,
        size.width as f64 / scale,
        size.height as f64 / scale,
    ))
}

/// Strip the last click event if it lands inside the recording bar window.
///
/// When the user clicks Stop, the event tap captures that click before shutting
/// down. This function removes that click and any events sharing its timestamp
/// (enrichment data for the stop-button click), preserving all other events
/// (e.g. VLM results for earlier clicks that were appended later).
pub(super) fn strip_recording_bar_click(
    events: &mut Vec<WalkthroughEvent>,
    bar_rect: (f64, f64, f64, f64),
) {
    let (bar_x, bar_y, bar_w, bar_h) = bar_rect;

    let last_click_pos = events
        .iter()
        .rposition(|e| matches!(&e.kind, WalkthroughEventKind::MouseClicked { .. }));

    if let Some(idx) = last_click_pos
        && let WalkthroughEventKind::MouseClicked { x, y, .. } = &events[idx].kind
        && *x >= bar_x
        && *x <= bar_x + bar_w
        && *y >= bar_y
        && *y <= bar_y + bar_h
    {
        let click_ts = events[idx].timestamp;
        events.retain(|e| e.timestamp != click_ts);
    }
}

pub(super) fn persist_and_emit(
    app: &tauri::AppHandle,
    storage: &WalkthroughStorage,
    session_dir: &std::path::Path,
    event: &WalkthroughEvent,
) {
    let _ = storage.append_event(session_dir, event);
    emit_event(app, event);
}

fn emit_event(app: &tauri::AppHandle, event: &WalkthroughEvent) {
    let _ = app.emit(
        "walkthrough://event",
        super::types::WalkthroughEventPayload {
            event: event.clone(),
        },
    );
}

// ---------------------------------------------------------------------------
// CDP helpers
// ---------------------------------------------------------------------------

/// Check if an app is already running with `--remote-debugging-port=<N>`.
/// Returns the port if found, so we can skip the quit/relaunch cycle.
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

/// Pick a random port in the ephemeral range (49152–65535).
pub(super) fn rand_ephemeral_port() -> u16 {
    use std::time::SystemTime;
    let seed = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos();
    let raw = seed.wrapping_mul(1664525).wrapping_add(1013904223);
    let range = 65535 - 49152;
    49152 + (raw % range) as u16
}

/// Build the McpServerConfig for a chrome-devtools-mcp connected to a specific port.
pub(super) fn cdp_server_config(server_name: &str, port: u16) -> clickweave_mcp::McpServerConfig {
    clickweave_mcp::McpServerConfig {
        name: server_name.to_string(),
        command: "npx".into(),
        args: vec![
            "-y".into(),
            "chrome-devtools-mcp".into(),
            format!("--browserUrl=http://127.0.0.1:{}", port),
        ],
    }
}

/// Set up CDP servers for user-selected apps.
///
/// For each app: quit the running instance, relaunch with
/// `--remote-debugging-port`, spawn a chrome-devtools-mcp server, and
/// poll until ready. Returns a map of app_name → CDP server name.
async fn setup_cdp_apps(
    cdp_apps: &[CdpAppConfig],
    mcp: &mut McpRouter,
    app: &tauri::AppHandle,
    cancel: &mut tokio::sync::watch::Receiver<bool>,
) -> HashMap<String, String> {
    use clickweave_core::cdp::cdp_server_name;

    let mut state: HashMap<String, String> = HashMap::new();

    for cdp_app in cdp_apps {
        // Check for cancellation between apps.
        if *cancel.borrow() {
            break;
        }

        let server_name = cdp_server_name(&cdp_app.name);

        // Check if the app is already running with a debug port — if so, skip
        // the quit/relaunch cycle and reuse the existing port.
        let port = match existing_debug_port(&cdp_app.name).await {
            Some(p) => {
                tracing::info!(
                    "'{}' already running with --remote-debugging-port={}, reusing",
                    cdp_app.name,
                    p
                );
                emit_cdp_progress(app, &cdp_app.name, CdpSetupStatus::Connecting);
                p
            }
            None => {
                let port = rand_ephemeral_port();

                if cdp_app.binary_path.is_some() {
                    emit_cdp_progress(app, &cdp_app.name, CdpSetupStatus::Launching);
                } else {
                    emit_cdp_progress(app, &cdp_app.name, CdpSetupStatus::Restarting);
                }

                // Quit existing instance and wait for it to exit.
                let quit_args = serde_json::json!({ "app_name": &cdp_app.name });
                match mcp.call_tool("quit_app", Some(quit_args)).await {
                    Ok(r) if r.is_error == Some(true) => {
                        tracing::debug!(
                            "quit_app for '{}' returned error (may not be running)",
                            cdp_app.name
                        );
                    }
                    Err(e) => {
                        tracing::debug!("quit_app for '{}' failed: {e}", cdp_app.name);
                    }
                    _ => {}
                }

                // Poll until the app is no longer reported as running (up to 10s).
                let poll_args =
                    serde_json::json!({ "app_name": &cdp_app.name, "user_apps_only": true });
                let mut quit_confirmed = false;
                for _ in 0..20 {
                    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                    if let Ok(r) = mcp.call_tool("list_apps", Some(poll_args.clone())).await {
                        let text = r
                            .content
                            .iter()
                            .filter_map(|c| c.as_text())
                            .collect::<String>();
                        if text.trim() == "[]" {
                            quit_confirmed = true;
                            break;
                        }
                    }
                }

                if !quit_confirmed {
                    tracing::warn!("'{}' did not quit within 10s, force-killing", cdp_app.name);
                    let force_args =
                        serde_json::json!({ "app_name": &cdp_app.name, "force": true });
                    let _ = mcp.call_tool("quit_app", Some(force_args)).await;
                    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                }

                // Relaunch with debug port.
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

                let launch_result = mcp.call_tool("launch_app", Some(launch_args)).await;

                match &launch_result {
                    Err(e) => {
                        tracing::warn!("Failed to launch '{}' with CDP: {}", cdp_app.name, e);
                        emit_cdp_progress(
                            app,
                            &cdp_app.name,
                            CdpSetupStatus::Failed {
                                reason: e.to_string(),
                            },
                        );
                        continue;
                    }
                    Ok(r) if r.is_error == Some(true) => {
                        let reason = r
                            .content
                            .iter()
                            .filter_map(|c| c.as_text())
                            .collect::<Vec<_>>()
                            .join("; ");
                        tracing::warn!(
                            "launch_app for '{}' returned error: {reason}",
                            cdp_app.name
                        );
                        emit_cdp_progress(app, &cdp_app.name, CdpSetupStatus::Failed { reason });
                        continue;
                    }
                    _ => {}
                }

                // Wait for the app to start.
                tokio::time::sleep(std::time::Duration::from_secs(3)).await;

                port
            }
        };

        emit_cdp_progress(app, &cdp_app.name, CdpSetupStatus::Connecting);

        // Spawn the CDP server.
        let config = cdp_server_config(&server_name, port);
        if let Err(e) = mcp.spawn_server(&config).await {
            tracing::warn!("Failed to spawn CDP server for '{}': {}", cdp_app.name, e);
            emit_cdp_progress(
                app,
                &cdp_app.name,
                CdpSetupStatus::Failed {
                    reason: e.to_string(),
                },
            );
            continue;
        }

        // Poll until ready (10s timeout), with cancellation.
        let ready = tokio::select! {
            biased;
            _ = cancel.changed() => {
                tracing::info!("CDP setup cancelled during poll for '{}'", cdp_app.name);
                break;
            }
            result = poll_cdp_ready(mcp, &server_name, 10) => result,
        };

        match ready {
            Ok(()) => {
                tracing::info!(
                    "CDP connected to '{}' (port {}, server '{}')",
                    cdp_app.name,
                    port,
                    server_name,
                );
                emit_cdp_progress(app, &cdp_app.name, CdpSetupStatus::Ready);
                state.insert(cdp_app.name.clone(), server_name);
            }
            Err(e) => {
                tracing::warn!("CDP poll failed for '{}': {}", cdp_app.name, e);
                emit_cdp_progress(app, &cdp_app.name, CdpSetupStatus::Failed { reason: e });
            }
        }
    }

    state
}

/// Poll `list_pages` on a CDP server until it returns at least one page.
async fn poll_cdp_ready(
    mcp: &McpRouter,
    server_name: &str,
    timeout_secs: u64,
) -> Result<(), String> {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(timeout_secs);

    loop {
        match mcp
            .call_tool_on(server_name, "list_pages", Some(serde_json::json!({})))
            .await
        {
            Ok(result) if result.is_error != Some(true) => {
                let text: String = result
                    .content
                    .iter()
                    .filter_map(|c| c.as_text())
                    .collect::<Vec<_>>()
                    .join("\n");
                if text.contains("1:") {
                    return Ok(());
                }
            }
            Ok(result) => {
                let text: String = result
                    .content
                    .iter()
                    .filter_map(|c| c.as_text())
                    .collect::<Vec<_>>()
                    .join("\n");
                tracing::debug!("CDP list_pages error for '{}': {}", server_name, text);
            }
            Err(e) => {
                tracing::debug!("CDP list_pages call failed for '{}': {}", server_name, e);
            }
        }

        if tokio::time::Instant::now() >= deadline {
            return Err(format!(
                "Timed out waiting for CDP server '{}' to be ready ({}s)",
                server_name, timeout_secs
            ));
        }

        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
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

/// Capture a CDP snapshot for a click and persist as a CdpSnapshotCaptured event.
async fn cdp_snapshot_for_click(
    mcp: &McpRouter,
    server_name: &str,
    app: &tauri::AppHandle,
    storage: &WalkthroughStorage,
    session_dir: &std::path::Path,
    click_event_id: Uuid,
    click_timestamp: u64,
) {
    let call_fut = mcp.call_tool_on(server_name, "take_snapshot", Some(serde_json::json!({})));
    let snapshot = match tokio::time::timeout(CDP_SNAPSHOT_TIMEOUT, call_fut).await {
        Ok(Ok(r)) if r.is_error != Some(true) => r
            .content
            .iter()
            .filter_map(|c| c.as_text())
            .collect::<Vec<_>>()
            .join("\n"),
        Ok(Ok(r)) => {
            let err_text: String = r
                .content
                .iter()
                .filter_map(|c| c.as_text())
                .collect::<Vec<_>>()
                .join("\n");
            tracing::debug!(
                "CDP take_snapshot returned error for click {click_event_id}: {err_text}"
            );
            return;
        }
        Ok(Err(e)) => {
            tracing::debug!("CDP take_snapshot failed for click {click_event_id}: {e}");
            return;
        }
        Err(_) => {
            tracing::debug!("CDP take_snapshot timed out for click {click_event_id}");
            return;
        }
    };

    if snapshot.is_empty() {
        return;
    }

    let event = WalkthroughEvent {
        id: Uuid::new_v4(),
        timestamp: click_timestamp,
        kind: WalkthroughEventKind::CdpSnapshotCaptured {
            snapshot_text: snapshot,
            click_event_id,
        },
    };
    persist_and_emit(app, storage, session_dir, &event);
}

// ---------------------------------------------------------------------------
// MCP helpers
// ---------------------------------------------------------------------------

pub(super) async fn spawn_mcp(mcp_command: &str) -> Option<McpRouter> {
    let configs = clickweave_mcp::default_server_configs(mcp_command);
    match McpRouter::spawn(&configs).await {
        Ok(router) => {
            tracing::info!(
                "MCP router spawned for walkthrough enrichment: {} servers, {} tools",
                router.server_count(),
                router.tools().len()
            );
            Some(router)
        }
        Err(e) => {
            tracing::warn!(
                "Failed to spawn MCP servers for walkthrough: {e}. Continuing without enrichment."
            );
            None
        }
    }
}

pub(super) async fn populate_app_cache(mcp: &McpRouter, cache: &mut HashMap<i32, CachedApp>) {
    let result = mcp
        .call_tool(
            "list_apps",
            Some(serde_json::json!({"user_apps_only": true})),
        )
        .await;

    if let Ok(result) = result {
        for content in &result.content {
            if let Some(text) = content.as_text() {
                // list_apps returns JSON with apps array.
                if let Ok(apps) = serde_json::from_str::<serde_json::Value>(text)
                    && let Some(arr) = apps.as_array()
                {
                    for app in arr {
                        if let (Some(name), Some(pid)) = (app["name"].as_str(), app["pid"].as_i64())
                        {
                            cache.insert(
                                pid as i32,
                                CachedApp {
                                    name: name.to_string(),
                                    bundle_id: app["bundle_id"].as_str().map(|s| s.to_string()),
                                },
                            );
                        }
                    }
                }
            }
        }
        tracing::debug!("App cache populated with {} entries", cache.len());
    }
}

async fn resolve_app_name(
    pid: i32,
    mcp: &Option<std::sync::Arc<McpRouter>>,
    cache: &mut HashMap<i32, CachedApp>,
) -> String {
    if let Some(cached) = cache.get(&pid) {
        return cached.name.clone();
    }

    // Re-fetch the app list from MCP to find the new PID.
    if let Some(mcp) = mcp {
        populate_app_cache(mcp.as_ref(), cache).await;
        if let Some(cached) = cache.get(&pid) {
            return cached.name.clone();
        }
    }

    // Insert negative-cache entry to avoid repeated MCP calls for unknown PIDs.
    let fallback = format!("PID:{pid}");
    cache.insert(
        pid,
        CachedApp {
            name: fallback.clone(),
            bundle_id: None,
        },
    );
    fallback
}

/// Background task that enriches a click with MCP data, generates a click crop,
/// and optionally resolves the target via VLM. Persists and emits all resulting
/// events.
///
/// Runs entirely off the main event loop so click capture is never blocked.
/// The crop and VLM resolution run concurrently — neither depends on the other.
#[allow(clippy::too_many_arguments)]
async fn enrich_click_background(
    mcp: std::sync::Arc<McpRouter>,
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
    #[cfg(target_os = "macos")] prehover_screenshot: Option<Arc<CursorRegionCapture>>,
) {
    use base64::Engine;
    use clickweave_core::walkthrough::ScreenshotMeta;

    // Run enrichment without checking the cancel token — we want MCP calls
    // to complete even after Stop is pressed so every click gets a screenshot.
    // The drain timeout in the event loop bounds total shutdown time.
    let enrichment_events =
        enrich_click(&mcp, &session_dir, x, y, app_name.as_deref(), timestamp).await;

    for ev in &enrichment_events {
        persist_and_emit(&app, &storage, &session_dir, ev);
    }

    // Extract screenshot info and AX label from enrichment events.
    let mut screenshot_path: Option<String> = None;
    let mut screenshot_meta: Option<ScreenshotMeta> = None;
    let mut ax_label_data: Option<(String, Option<String>)> = None;
    let mut has_actionable_ax = false;

    for ev in &enrichment_events {
        match &ev.kind {
            WalkthroughEventKind::ScreenshotCaptured { path, meta, .. } => {
                screenshot_path = Some(path.clone());
                screenshot_meta = *meta;
            }
            WalkthroughEventKind::AccessibilityElementCaptured { label, role } => {
                has_actionable_ax =
                    clickweave_core::walkthrough::is_actionable_ax_role(role.as_deref());
                ax_label_data = Some((label.clone(), role.clone()));
            }
            _ => {}
        }
    }

    // Reactive Electron detection: if native AX returned nothing useful
    // and the app is still classified as Native, recheck for Electron
    // framework. This catches apps with unusual bundle structures that
    // slipped past proactive detection.
    if !has_actionable_ax {
        let current_kind = app_kind_cache.lock().unwrap().get(&target_pid).copied();
        if current_kind == Some(AppKind::Native) {
            let rechecked = classify_app_by_pid(target_pid);
            if rechecked != AppKind::Native {
                tracing::info!(
                    "Reactive detection: PID {} reclassified as {:?} (empty AX triggered recheck)",
                    target_pid,
                    rechecked,
                );
                app_kind_cache.lock().unwrap().insert(target_pid, rechecked);

                // Re-emit focus event with corrected app_kind so downstream
                // normalization picks up the reclassification.
                let updated_focus = WalkthroughEvent {
                    id: Uuid::new_v4(),
                    timestamp,
                    kind: WalkthroughEventKind::AppFocused {
                        app_name: app_name.clone().unwrap_or_default(),
                        pid: target_pid,
                        window_title: None,
                        app_kind: rechecked,
                    },
                };
                persist_and_emit(&app, &storage, &session_dir, &updated_focus);
            }
        }
    }

    // Both crop and VLM need a screenshot. Bail early if we don't have one.
    let (Some(screenshot_path), Some(screenshot_meta)) = (screenshot_path, screenshot_meta) else {
        return;
    };

    // Crop and VLM are independent — run them concurrently.
    //
    // For the crop, the cursor region capture (polled every 100ms) IS the
    // template — it's already the right size and shows the screen before
    // hover effects. Just JPEG-encode and emit it. Fall back to the MCP
    // screenshot + crop_click_region if the buffer was empty.
    //
    // VLM sends the screenshot to the vision model to identify the element.
    // Skipped when the click already has an actionable accessibility label.

    let crop_app = app.clone();
    let crop_storage = storage.clone();
    let crop_dir = session_dir.clone();
    let crop_path = screenshot_path.clone();
    let crop_fut = async move {
        use super::walkthrough_enrichment::crop_click_region;

        let artifacts_dir = crop_dir.join("artifacts");

        let emit_crop = |b64: String, path: std::path::PathBuf| {
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
            persist_and_emit(&crop_app, &crop_storage, &crop_dir, &ev);
        };

        // Try the cursor region capture first (pre-hover, already cropped).
        #[cfg(target_os = "macos")]
        if let Some(shot) = prehover_screenshot {
            tracing::debug!("Using cursor region capture for click crop");
            let artifacts_for_capture = artifacts_dir.clone();
            let crop_result = tokio::task::spawn_blocking(move || {
                let img =
                    image::RgbaImage::from_raw(shot.width, shot.height, shot.rgba_bytes.clone())?;
                let dynamic = image::DynamicImage::ImageRgba8(img);
                let mut jpeg_buf = std::io::Cursor::new(Vec::new());
                dynamic
                    .write_to(&mut jpeg_buf, image::ImageFormat::Jpeg)
                    .ok()?;
                let jpeg_bytes = jpeg_buf.into_inner();
                let b64 = base64::engine::general_purpose::STANDARD.encode(&jpeg_bytes);
                let filename = format!("crop_{timestamp}.jpg");
                let path = artifacts_for_capture.join(&filename);
                let _ = std::fs::write(&path, &jpeg_bytes);
                Some((b64, path))
            })
            .await;
            if let Ok(Some((crop_b64, crop_path))) = crop_result {
                emit_crop(crop_b64, crop_path);
                return;
            }
        }

        // Fallback: crop from the MCP screenshot.
        tracing::debug!("Falling back to MCP screenshot for crop");
        let bytes = match tokio::fs::read(&crop_path).await {
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
            emit_crop(crop_b64, crop_path);
        }
    };

    let vlm_fut = async {
        if has_actionable_ax {
            return;
        }
        let backend = match vlm_backend {
            Some(ref b) => b,
            None => return,
        };
        let ax_ref = ax_label_data
            .as_ref()
            .map(|(l, r)| (l.as_str(), r.as_deref()));
        let req = match prepare_vlm_click_request(
            &screenshot_path,
            x,
            y,
            screenshot_meta,
            ax_ref,
            None,
            app_name.as_deref(),
        ) {
            Some(r) => r,
            None => return,
        };

        let vlm_result = tokio::time::timeout(
            vlm_timeout,
            execute_vlm_click_request(backend.as_ref(), &req),
        )
        .await;

        match vlm_result {
            Ok(Some(label)) => {
                tracing::info!("VLM resolved click at ts={timestamp} → \"{label}\"");
                let vlm_event = WalkthroughEvent {
                    id: Uuid::new_v4(),
                    timestamp,
                    kind: WalkthroughEventKind::VlmLabelResolved { label },
                };
                persist_and_emit(&app, &storage, &session_dir, &vlm_event);
            }
            Ok(None) => {}
            Err(_) => {
                tracing::warn!("VLM timed out for click at ts={timestamp}");
            }
        }
    };

    tokio::join!(crop_fut, vlm_fut);
}

#[cfg(test)]
mod tests {
    use clickweave_core::MouseButton;

    use super::*;

    fn click_event(timestamp: u64, x: f64, y: f64) -> WalkthroughEvent {
        WalkthroughEvent {
            id: Uuid::new_v4(),
            timestamp,
            kind: WalkthroughEventKind::MouseClicked {
                x,
                y,
                button: MouseButton::Left,
                click_count: 1,
                modifiers: vec![],
                cdp_element: None,
            },
        }
    }

    fn stopped_event(timestamp: u64) -> WalkthroughEvent {
        WalkthroughEvent {
            id: Uuid::new_v4(),
            timestamp,
            kind: WalkthroughEventKind::Stopped,
        }
    }

    // --- strip_recording_bar_click ---

    #[test]
    fn strip_removes_click_inside_bar() {
        let bar = (100.0, 200.0, 300.0, 50.0);
        let mut events = vec![
            click_event(1, 50.0, 50.0),   // outside bar — keep
            click_event(2, 150.0, 220.0), // inside bar (last click)
        ];
        strip_recording_bar_click(&mut events, bar);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].timestamp, 1);
    }

    #[test]
    fn strip_keeps_click_outside_bar() {
        let bar = (100.0, 200.0, 300.0, 50.0);
        let mut events = vec![
            click_event(1, 50.0, 50.0),
            click_event(2, 50.0, 100.0), // outside bar
        ];
        strip_recording_bar_click(&mut events, bar);
        assert_eq!(events.len(), 2);
    }

    #[test]
    fn strip_noop_when_no_clicks() {
        let bar = (100.0, 200.0, 300.0, 50.0);
        let mut events = vec![stopped_event(1)];
        strip_recording_bar_click(&mut events, bar);
        assert_eq!(events.len(), 1);
    }

    #[test]
    fn strip_removes_all_events_with_same_timestamp() {
        let bar = (100.0, 200.0, 300.0, 50.0);
        let mut events = vec![
            click_event(1, 50.0, 50.0),   // different ts — keep
            click_event(2, 150.0, 220.0), // inside bar, ts=2
            stopped_event(2),             // same ts as bar click — also removed
        ];
        strip_recording_bar_click(&mut events, bar);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].timestamp, 1);
    }

    // --- rand_ephemeral_port ---

    #[test]
    fn ephemeral_port_in_range() {
        for _ in 0..100 {
            let port = rand_ephemeral_port();
            assert!(
                (49152..=65535).contains(&port),
                "port {port} outside ephemeral range"
            );
        }
    }

    // --- cdp_server_config ---

    #[test]
    fn cdp_server_config_builds_correctly() {
        let config = cdp_server_config("cdp:Discord", 9222);
        assert_eq!(config.name, "cdp:Discord");
        assert_eq!(config.command, "npx");
        assert_eq!(
            config.args,
            vec![
                "-y",
                "chrome-devtools-mcp",
                "--browserUrl=http://127.0.0.1:9222"
            ]
        );
    }
}
