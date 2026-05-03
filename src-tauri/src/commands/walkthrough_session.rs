use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use clickweave_core::AppKind;
use clickweave_core::app_detection::{bundle_path_from_pid, classify_app, classify_app_by_pid};
use clickweave_core::cdp::{
    current_selected_page_url as pick_current_selected_url, parse_cdp_page_list,
    pick_page_index_for_url,
};
use clickweave_core::walkthrough::enrichment::parse_cdp_click_data;
use clickweave_core::walkthrough::session::{
    self as session_lib, CDP_CHECK_AND_REINJECT_JS, CDP_CLICK_LISTENER_JS, CDP_HOVER_LISTENER_JS,
    CDP_RETRIEVE_CLICK_JS, CDP_RETRIEVE_HOVERS_JS, CDP_STOP_HOVER_JS, CachedApp,
};
use clickweave_core::walkthrough::{
    ScreenshotKind, ScreenshotMeta, WalkthroughEvent, WalkthroughEventKind,
    WalkthroughSessionRuntime, WalkthroughStatus, WalkthroughStorage,
};
use clickweave_mcp::McpClient;
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

// CDP JavaScript constants (CDP_CLICK_LISTENER_JS, CDP_RETRIEVE_CLICK_JS,
// CDP_CHECK_AND_REINJECT_JS, CDP_HOVER_LISTENER_JS, CDP_RETRIEVE_HOVERS_JS,
// CDP_STOP_HOVER_JS) are imported from clickweave_core::walkthrough::session.

use crate::platform::CursorRegionCapture;

#[cfg(target_os = "macos")]
use crate::platform::macos::MacOSEventTap;
#[cfg(target_os = "windows")]
use crate::platform::windows::WindowsEventHook;

#[cfg(any(target_os = "macos", target_os = "windows"))]
use std::sync::RwLock;

/// Shared buffer holding the most recent cursor region capture (64×64pt around
/// the cursor, polled every 100ms). Used as the click crop template — always
/// reflects the screen before hover effects from the click itself.
///
/// Inner `Arc` avoids cloning the pixel data when reading on click — only an
/// `Arc` pointer bump instead of a 64 KB memcpy.
#[cfg(any(target_os = "macos", target_os = "windows"))]
type ScreenshotBuffer = Arc<RwLock<Option<Arc<CursorRegionCapture>>>>;

/// Per-app CDP session info captured during `setup_cdp_apps` and consumed by
/// the walkthrough event loop when reconnecting to retrieve click/hover data.
///
/// `selected_page_url` is the URL of the page on which the click/hover JS
/// listeners were injected. Future reconnects call `cdp_select_page` with the
/// matching index so retrieval always hits the right tab even when the
/// browser has multiple tabs open or added new ones during recording.
#[derive(Debug, Clone)]
struct CdpAppState {
    port: u16,
    selected_page_url: Option<String>,
}

struct CaptureServices {
    mcp: Option<Arc<McpClient>>,
    cdp_state: HashMap<String, CdpAppState>,
    vlm_backend: Option<Arc<clickweave_llm::LlmClient>>,
}

struct CdpClickRequest {
    port: u16,
    /// URL of the tab the click/hover listeners were injected into —
    /// restored after reconnect so retrieval hits the same page even if
    /// the browser has multiple tabs open.
    selected_page_url: Option<String>,
    click_event_id: Uuid,
    click_timestamp: u64,
}

#[derive(Default)]
struct CaptureFocusState {
    last_pid: i32,
    self_focused: bool,
}

struct ClickEnrichmentSummary {
    screenshot_path: Option<String>,
    screenshot_meta: Option<ScreenshotMeta>,
    ax_label_data: Option<(String, Option<String>)>,
    has_actionable_ax: bool,
}

enum CdpConnectOutcome {
    Ready,
    Cancelled,
    Failed(String),
}

// CachedApp is imported from clickweave_core::walkthrough::session.

/// Manages the walkthrough recording lifecycle.
pub struct WalkthroughHandle {
    pub session: Option<WalkthroughSessionRuntime>,
    pub session_dir: Option<std::path::PathBuf>,
    pub(super) storage: Option<WalkthroughStorage>,
    #[cfg(target_os = "macos")]
    pub(super) event_tap: Option<MacOSEventTap>,
    #[cfg(target_os = "windows")]
    pub(super) event_hook: Option<WindowsEventHook>,
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
            #[cfg(target_os = "macos")]
            event_tap: None,
            #[cfg(target_os = "windows")]
            event_hook: None,
            processing_task: None,
            cancel_tx,
        }
    }
}

impl WalkthroughHandle {
    pub(super) fn ensure_status(
        &self,
        expected: &[WalkthroughStatus],
    ) -> Result<&WalkthroughSessionRuntime, super::error::CommandError> {
        let session = self
            .session
            .as_ref()
            .ok_or(super::error::CommandError::validation(
                "No walkthrough session is active",
            ))?;
        if !expected.contains(&session.meta.status) {
            return Err(super::error::CommandError::validation(format!(
                "Walkthrough is in {:?} state, expected one of {:?}",
                session.meta.status, expected
            )));
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

        #[cfg(target_os = "windows")]
        if let Some(hook) = self.event_hook.take() {
            hook.send_command(CaptureCommand::Stop);
            drop(hook);
        }

        self.processing_task.take()
    }
}

// ---------------------------------------------------------------------------
// Async event processing loop
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
async fn initialize_capture_services(
    app: &tauri::AppHandle,
    event_rx: &mut tokio::sync::mpsc::UnboundedReceiver<CaptureEvent>,
    mcp_binary_path: &str,
    supervisor: Option<super::types::EndpointConfig>,
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

fn vlm_capture_config(supervisor: super::types::EndpointConfig) -> clickweave_llm::LlmConfig {
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
pub(super) async fn process_capture_events(
    app: tauri::AppHandle,
    mut event_rx: tokio::sync::mpsc::UnboundedReceiver<CaptureEvent>,
    mcp_binary_path: String,
    supervisor: Option<super::types::EndpointConfig>,
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

async fn stop_recording_and_persist_frames(mcp: &McpClient, session_dir: &std::path::Path) {
    let recording_timeout = tokio::time::Duration::from_secs(10);
    match tokio::time::timeout(recording_timeout, mcp.call_tool("stop_recording", None)).await {
        Ok(Ok(result)) if result.is_error != Some(true) => {
            let frames = super::walkthrough_enrichment::parse_recording_frames(&result.content);
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

async fn persist_native_hover_events(
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

async fn persist_cdp_hover_events(
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
/// Delegates to `session_lib::strip_recording_bar_click` in the library crate.
pub(super) fn strip_recording_bar_click(
    events: &mut Vec<WalkthroughEvent>,
    bar_rect: (f64, f64, f64, f64),
) {
    session_lib::strip_recording_bar_click(events, bar_rect);
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

pub(super) use clickweave_core::cdp::rand_ephemeral_port;

/// Set up CDP connections for user-selected apps.
///
/// For each app: quit the running instance, relaunch with
/// `--remote-debugging-port`, connect via `cdp_connect`, inject
/// listeners, and disconnect. Returns a map of app_name → CDP port.
async fn setup_cdp_apps(
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
async fn restore_selected_page(mcp: &McpClient, target_url: &str) {
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

/// Retrieve the last click's DOM element data from the injected listener.
///
/// Returns a `CdpClickResolved` event if data is available, or None if the
/// click landed outside the CDP app / listener was lost.
#[allow(clippy::too_many_arguments)]
async fn cdp_retrieve_click(
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

pub(super) async fn spawn_mcp(mcp_binary_path: &str) -> Option<McpClient> {
    match McpClient::spawn(mcp_binary_path, &[]).await {
        Ok(client) => {
            tracing::info!(
                "MCP client spawned for walkthrough enrichment: {} tools",
                client.tool_count()
            );
            Some(client)
        }
        Err(e) => {
            tracing::warn!(
                "Failed to spawn MCP client for walkthrough: {e}. Continuing without enrichment."
            );
            None
        }
    }
}

pub(super) async fn populate_app_cache(mcp: &McpClient, cache: &mut HashMap<i32, CachedApp>) {
    let result = mcp
        .call_tool(
            "list_apps",
            Some(serde_json::json!({"user_apps_only": true})),
        )
        .await;

    if let Ok(result) = result {
        for content in &result.content {
            if let Some(text) = content.as_text() {
                for (pid, name, bundle_id) in session_lib::parse_app_list(text) {
                    cache.insert(pid, CachedApp { name, bundle_id });
                }
            }
        }
        tracing::debug!("App cache populated with {} entries", cache.len());
    }
}

async fn resolve_app_name(
    pid: i32,
    mcp: &Option<std::sync::Arc<McpClient>>,
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
    use super::walkthrough_enrichment::crop_click_region;

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
}
