use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};

use base64::Engine;
use clickweave_core::app_detection::{bundle_path_from_pid, classify_app, classify_app_by_pid};
use clickweave_core::storage::now_millis;
use clickweave_core::walkthrough::{
    AppKind, ScreenshotKind, ScreenshotMeta, WalkthroughAction, WalkthroughAnnotations,
    WalkthroughEvent, WalkthroughEventKind, WalkthroughSession, WalkthroughStatus,
    WalkthroughStorage,
};
use clickweave_mcp::McpRouter;
use tauri::{Emitter, Manager};
use uuid::Uuid;

use super::types::{
    AppDataDir, WalkthroughDraftPayload, WalkthroughEventPayload, WalkthroughStatePayload,
    parse_uuid,
};
use crate::platform::{CaptureCommand, CaptureEvent, CaptureEventKind};

#[cfg(target_os = "macos")]
use crate::platform::macos::{CursorRegionCapture, MacOSEventTap};

/// Shared buffer holding the most recent cursor region capture (64×64pt around
/// the cursor, polled every 100ms). Used as the click crop template — always
/// reflects the screen before hover effects from the click itself.
///
/// Inner `Arc` avoids cloning the pixel data when reading on click — only an
/// `Arc` pointer bump instead of a 64 KB memcpy.
#[cfg(target_os = "macos")]
type ScreenshotBuffer = Arc<RwLock<Option<Arc<CursorRegionCapture>>>>;

const RECORDING_BAR_LABEL: &str = "recording-bar";
const SELF_APP_NAME: &str = "clickweave-tauri";

/// Maximum length of a VLM-resolved label to accept. Longer responses
/// are likely full sentences rather than a concise element name.
const VLM_LABEL_MAX_LEN: usize = 80;

/// Cached info about a running app, populated from MCP's `list_apps` response.
struct CachedApp {
    name: String,
    bundle_id: Option<String>,
}

/// Manages the walkthrough recording lifecycle.
pub struct WalkthroughHandle {
    pub session: Option<WalkthroughSession>,
    pub session_dir: Option<std::path::PathBuf>,
    storage: Option<WalkthroughStorage>,
    mcp_command: Option<String>,
    #[cfg(target_os = "macos")]
    event_tap: Option<MacOSEventTap>,
    processing_task: Option<tauri::async_runtime::JoinHandle<()>>,
    /// Cancellation signal for the processing loop.
    cancel_tx: tokio::sync::watch::Sender<bool>,
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
    fn ensure_status(&self, expected: &[WalkthroughStatus]) -> Result<&WalkthroughSession, String> {
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
    fn stop_capture(&mut self) -> Option<tauri::async_runtime::JoinHandle<()>> {
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

/// A running app detected as Electron or Chrome, returned to the frontend for CDP selection.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, specta::Type)]
pub struct DetectedCdpApp {
    pub name: String,
    pub pid: i32,
    pub app_kind: AppKind,
}

/// User-selected app for CDP during walkthrough.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, specta::Type)]
pub struct CdpAppConfig {
    pub name: String,
    /// Path to the app binary (from file picker). None for already-running apps.
    pub binary_path: Option<String>,
    pub app_kind: AppKind,
}

/// Status updates emitted during CDP setup.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, specta::Type)]
pub struct CdpSetupProgress {
    pub app_name: String,
    pub status: CdpSetupStatus,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, specta::Type)]
pub enum CdpSetupStatus {
    Restarting,
    Launching,
    Connecting,
    Ready,
    Failed { reason: String },
    Done,
}

#[tauri::command]
#[specta::specta]
pub async fn detect_cdp_apps(mcp_command: String) -> Result<Vec<DetectedCdpApp>, String> {
    let mcp = spawn_mcp(&mcp_command)
        .await
        .ok_or("Failed to spawn MCP server")?;

    let mut cache: HashMap<i32, CachedApp> = HashMap::new();
    populate_app_cache(&mcp, &mut cache).await;

    let mut cdp_apps = Vec::new();
    for (pid, cached) in &cache {
        let bundle_path = bundle_path_from_pid(*pid);
        let kind = classify_app(cached.bundle_id.as_deref(), bundle_path.as_deref());
        if kind.uses_cdp() {
            cdp_apps.push(DetectedCdpApp {
                name: cached.name.clone(),
                pid: *pid,
                app_kind: kind,
            });
        }
    }

    // MCP router is dropped here, killing the server processes.
    Ok(cdp_apps)
}

#[tauri::command]
#[specta::specta]
pub async fn validate_app_path(path: String) -> Result<DetectedCdpApp, String> {
    let kind = classify_app(None, Some(std::path::Path::new(&path)));
    if !kind.uses_cdp() {
        return Err(format!("Not an Electron or Chrome app: {}", path));
    }

    let name = std::path::Path::new(&path)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("Unknown")
        .to_string();

    Ok(DetectedCdpApp {
        name,
        pid: 0,
        app_kind: kind,
    })
}

fn emit_state(app: &tauri::AppHandle, status: WalkthroughStatus) {
    let _ = app.emit("walkthrough://state", WalkthroughStatePayload { status });
}

fn emit_event(app: &tauri::AppHandle, event: &WalkthroughEvent) {
    let _ = app.emit(
        "walkthrough://event",
        WalkthroughEventPayload {
            event: event.clone(),
        },
    );
}

#[tauri::command]
#[specta::specta]
pub async fn start_walkthrough(
    app: tauri::AppHandle,
    workflow_id: String,
    mcp_command: String,
    project_path: Option<String>,
    planner: Option<super::types::EndpointConfig>,
    cdp_apps: Vec<CdpAppConfig>,
) -> Result<(), String> {
    let wf_id = parse_uuid(&workflow_id, "workflow")?;

    // Set up session and storage under the lock, then release it before
    // spawning async work (which needs the app handle, not the lock).
    let (session_dir, processing_storage, cancel) = {
        let handle = app.state::<Mutex<WalkthroughHandle>>();
        let mut guard = handle.lock().unwrap();

        if guard.session.is_some() {
            return Err("A walkthrough session is already active".to_string());
        }

        let session = WalkthroughSession::new(wf_id);

        let storage = match &project_path {
            Some(p) => {
                let dir = super::types::project_dir(p);
                WalkthroughStorage::new(&dir)
            }
            None => {
                let app_data = app.state::<AppDataDir>();
                WalkthroughStorage::new_app_data(&app_data.0)
            }
        };

        let session_dir = storage
            .create_session_dir(&session)
            .map_err(|e| format!("Failed to create session dir: {e}"))?;

        storage
            .save_session(&session_dir, &session)
            .map_err(|e| format!("Failed to save initial session: {e}"))?;

        let processing_storage = storage.clone();
        guard.session = Some(session);
        guard.session_dir = Some(session_dir.clone());
        guard.storage = Some(storage);
        guard.mcp_command = Some(mcp_command.clone());

        // Fresh cancellation channel for this session.
        let (cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);
        guard.cancel_tx = cancel_tx;
        let cancel = cancel_rx;

        (session_dir, processing_storage, cancel)
    };

    // Helper to roll back session state and clean up the session directory on failure.
    let clear_session = |app: &tauri::AppHandle| {
        let handle = app.state::<Mutex<WalkthroughHandle>>();
        let mut guard = handle.lock().unwrap();
        if let Some(dir) = &guard.session_dir
            && let Err(e) = std::fs::remove_dir_all(dir)
        {
            tracing::warn!("Failed to clean up session dir on rollback: {e}");
        }
        guard.session = None;
        guard.session_dir = None;
        guard.storage = None;
        guard.mcp_command = None;
    };

    // Start the platform event tap and processing loop.
    #[cfg(target_os = "macos")]
    {
        let (event_tap, event_rx) = match MacOSEventTap::start() {
            Ok(pair) => pair,
            Err(e) => {
                clear_session(&app);
                return Err(format!("Failed to start event tap: {e}"));
            }
        };

        let emit_handle = app.clone();
        let processing_task = tauri::async_runtime::spawn(async move {
            process_capture_events(
                emit_handle,
                event_rx,
                mcp_command,
                planner,
                processing_storage,
                session_dir,
                cancel,
                cdp_apps,
            )
            .await;
        });

        let handle = app.state::<Mutex<WalkthroughHandle>>();
        let mut guard = handle.lock().unwrap();
        guard.event_tap = Some(event_tap);
        guard.processing_task = Some(processing_task);
    }

    #[cfg(not(target_os = "macos"))]
    {
        clear_session(&app);
        return Err("Walkthrough capture is only supported on macOS".to_string());
    }

    emit_state(&app, WalkthroughStatus::Recording);
    tracing::info!("Walkthrough session started for workflow {workflow_id}");
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub async fn pause_walkthrough(app: tauri::AppHandle) -> Result<(), String> {
    let handle = app.state::<Mutex<WalkthroughHandle>>();
    let mut guard = handle.lock().unwrap();

    guard.ensure_status(&[WalkthroughStatus::Recording])?;
    guard.session.as_mut().unwrap().status = WalkthroughStatus::Paused;

    #[cfg(target_os = "macos")]
    if let Some(tap) = &guard.event_tap {
        tap.send_command(CaptureCommand::Pause);
    }

    // Persist a Paused event.
    if let (Some(storage), Some(dir)) = (&guard.storage, &guard.session_dir) {
        let event = WalkthroughEvent {
            id: Uuid::new_v4(),
            timestamp: now_millis(),
            kind: WalkthroughEventKind::Paused,
        };
        let _ = storage.append_event(dir, &event);
    }

    drop(guard);
    emit_state(&app, WalkthroughStatus::Paused);
    tracing::info!("Walkthrough paused");
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub async fn resume_walkthrough(app: tauri::AppHandle) -> Result<(), String> {
    let handle = app.state::<Mutex<WalkthroughHandle>>();
    let mut guard = handle.lock().unwrap();

    guard.ensure_status(&[WalkthroughStatus::Paused])?;
    guard.session.as_mut().unwrap().status = WalkthroughStatus::Recording;

    #[cfg(target_os = "macos")]
    if let Some(tap) = &guard.event_tap {
        tap.send_command(CaptureCommand::Resume);
    }

    // Persist a Resumed event.
    if let (Some(storage), Some(dir)) = (&guard.storage, &guard.session_dir) {
        let event = WalkthroughEvent {
            id: Uuid::new_v4(),
            timestamp: now_millis(),
            kind: WalkthroughEventKind::Resumed,
        };
        let _ = storage.append_event(dir, &event);
    }

    drop(guard);
    emit_state(&app, WalkthroughStatus::Recording);
    tracing::info!("Walkthrough resumed");
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub async fn stop_walkthrough(
    app: tauri::AppHandle,
    planner: Option<super::types::EndpointConfig>,
) -> Result<(), String> {
    let (task, storage, session_dir, workflow_id, session_id) = {
        let handle = app.state::<Mutex<WalkthroughHandle>>();
        let mut guard = handle.lock().unwrap();

        guard.ensure_status(&[WalkthroughStatus::Recording, WalkthroughStatus::Paused])?;
        let session = guard.session.as_mut().unwrap();
        session.status = WalkthroughStatus::Processing;
        session.ended_at = Some(now_millis());

        let task = guard.stop_capture();

        // Persist the Stopped event.
        if let (Some(storage), Some(dir)) = (&guard.storage, &guard.session_dir) {
            let event = WalkthroughEvent {
                id: Uuid::new_v4(),
                timestamp: now_millis(),
                kind: WalkthroughEventKind::Stopped,
            };
            let _ = storage.append_event(dir, &event);
        }

        let sess = guard.session.as_ref().unwrap();
        (
            task,
            guard.storage.clone(),
            guard.session_dir.clone(),
            sess.workflow_id,
            sess.id,
        )
    };
    emit_state(&app, WalkthroughStatus::Processing);

    // Wait for the processing loop to exit. The cancel token was already
    // signalled by stop_capture(), so any in-flight MCP call is dropped
    // via select! and the task exits near-instantly.
    if let Some(task) = task {
        let _ = task.await;
    }

    // --- Processing phase (outside the lock) ---

    let (actions, draft, warnings) = match (&storage, &session_dir) {
        (Some(storage), Some(dir)) => {
            // Read events from disk.
            let mut events = storage
                .read_events(dir)
                .map_err(|e| format!("Failed to read events: {e}"))?;

            // Strip the stop-button click captured just before the tap shut down.
            if let Some(bar_rect) = get_recording_bar_rect(&app) {
                strip_recording_bar_click(&mut events, bar_rect);
            }

            // Normalize.
            let (mut actions, mut norm_warnings) =
                clickweave_core::walkthrough::normalize_events(&events);

            // VLM: resolve click targets using vision (parallel).
            if let Some(ref planner_cfg) = planner {
                resolve_click_targets_with_vlm(&mut actions, planner_cfg).await;
            }

            // Save actions.
            if let Err(e) = storage.save_actions(dir, &actions) {
                tracing::warn!("Failed to save actions: {e}");
            }

            // Synthesize draft.
            let draft = clickweave_core::walkthrough::synthesize_draft(
                &actions,
                workflow_id,
                "Walkthrough Draft",
            );

            // Validate (non-fatal — warnings only).
            if !draft.nodes.is_empty()
                && let Err(e) = clickweave_core::validate_workflow(&draft)
            {
                norm_warnings.push(format!("Draft validation warning: {e}"));
            }

            // Save draft.
            if let Err(e) = storage.save_draft(dir, &draft) {
                tracing::warn!("Failed to save draft: {e}");
            }

            (actions, draft, norm_warnings)
        }
        _ => (
            vec![],
            clickweave_core::Workflow::default(),
            vec!["No storage available".to_string()],
        ),
    };

    let action_node_map = clickweave_core::walkthrough::build_action_node_map(&actions, &draft);

    // Persist draft to disk.
    if let (Some(storage), Some(dir)) = (&storage, &session_dir)
        && let Err(e) = storage.save_draft(dir, &draft)
    {
        tracing::warn!("Failed to save final draft: {e}");
    }

    // Store results, persist, and emit — all under the same lock acquisition
    // to prevent cancel_walkthrough() from racing between the session update
    // and the frontend emission.
    {
        let handle = app.state::<Mutex<WalkthroughHandle>>();
        let mut guard = handle.lock().unwrap();
        let same_session = guard.session.as_ref().is_some_and(|s| s.id == session_id);
        if !same_session {
            tracing::info!(
                "Walkthrough session changed during processing (expected {session_id}), skipping review"
            );
            return Ok(());
        }
        let session = guard.session.as_mut().unwrap();
        session.actions = actions.clone();
        session.warnings = warnings.clone();
        session.status = WalkthroughStatus::Review;

        // Persist the updated session.
        if let (Some(storage), Some(dir)) = (&storage, &session_dir) {
            let _ = storage.save_session(dir, session);
        }

        // Emit results to frontend while still holding the lock, so cancel
        // cannot interleave between the session update and the emission.
        let _ = app.emit(
            "walkthrough://draft_ready",
            WalkthroughDraftPayload {
                actions,
                draft,
                warnings,
                action_node_map,
            },
        );
        emit_state(&app, WalkthroughStatus::Review);
    }

    tracing::info!("Walkthrough processing complete, entering review");
    Ok(())
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, specta::Type)]
pub struct WalkthroughDraftResponse {
    pub actions: Vec<WalkthroughAction>,
    pub draft: Option<clickweave_core::Workflow>,
    pub warnings: Vec<String>,
}

#[tauri::command]
#[specta::specta]
pub async fn get_walkthrough_draft(
    app: tauri::AppHandle,
) -> Result<WalkthroughDraftResponse, String> {
    let handle = app.state::<Mutex<WalkthroughHandle>>();

    // Extract needed data under lock, then drop it before doing file I/O.
    let (actions, warnings, draft_path) = {
        let guard = handle.lock().unwrap();
        guard.ensure_status(&[WalkthroughStatus::Review])?;
        let session = guard.session.as_ref().unwrap();
        let path = guard.session_dir.as_ref().map(|dir| dir.join("draft.json"));
        (session.actions.clone(), session.warnings.clone(), path)
    };

    // Read draft from disk if available (no lock held).
    let draft = match draft_path {
        Some(path) => match std::fs::read_to_string(&path) {
            Ok(data) => Some(
                serde_json::from_str(&data).map_err(|e| format!("Failed to parse draft: {e}"))?,
            ),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(format!("Failed to read draft: {e}")),
        },
        None => None,
    };

    Ok(WalkthroughDraftResponse {
        actions,
        draft,
        warnings,
    })
}

#[tauri::command]
#[specta::specta]
pub async fn cancel_walkthrough(app: tauri::AppHandle) -> Result<(), String> {
    let (task, session_dir) = {
        let handle = app.state::<Mutex<WalkthroughHandle>>();
        let mut guard = handle.lock().unwrap();

        if guard.session.is_none() {
            return Err("No walkthrough session is active".to_string());
        }

        let task = guard.stop_capture();
        let dir = guard.session_dir.take();

        guard.session = None;
        guard.storage = None;
        guard.mcp_command = None;

        (task, dir)
    };

    // Await graceful shutdown outside the lock.
    if let Some(task) = task {
        let _ = task.await;
    }

    // Clean up session artifacts from disk (events, screenshots, draft).
    // The recording may contain typed secrets, so we don't leave it behind.
    if let Some(dir) = &session_dir
        && let Err(e) = std::fs::remove_dir_all(dir)
    {
        tracing::warn!("Failed to clean up walkthrough session dir: {e}");
    }

    emit_state(&app, WalkthroughStatus::Idle);
    tracing::info!("Walkthrough cancelled");
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub async fn apply_walkthrough_annotations(
    app: tauri::AppHandle,
    annotations: WalkthroughAnnotations,
) -> Result<(), String> {
    let handle = app.state::<Mutex<WalkthroughHandle>>();
    let mut guard = handle.lock().unwrap();

    guard.ensure_status(&[WalkthroughStatus::Review])?;
    let session = guard.session.as_mut().unwrap();
    session.status = WalkthroughStatus::Applied;

    // TODO(M5): Actually apply annotations to the session's actions and
    // persist the result. For now we only update the status.
    tracing::warn!(
        "Walkthrough annotations received but not yet applied (stub): {} deletions, {} renames, {} target overrides, {} variable promotions",
        annotations.deleted_node_ids.len(),
        annotations.renamed_nodes.len(),
        annotations.target_overrides.len(),
        annotations.variable_promotions.len(),
    );

    drop(guard);
    emit_state(&app, WalkthroughStatus::Applied);
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub async fn seed_walkthrough_cache(
    app: tauri::AppHandle,
    workflow_id: String,
    workflow_name: String,
    project_path: Option<String>,
    app_entries: Vec<super::types::AppResolutionSeedEntry>,
) -> Result<(), String> {
    use clickweave_core::decision_cache::{AppResolution, DecisionCache, cache_key};

    let wf_id = parse_uuid(&workflow_id, "workflow")?;

    if app_entries.is_empty() {
        return Ok(());
    }

    let storage = super::types::resolve_storage(&app, &project_path, &workflow_name, wf_id);
    let cache_path = storage.cache_path();

    // Load existing cache or create new one.
    let mut cache = DecisionCache::load(&cache_path).unwrap_or_else(|| DecisionCache::new(wf_id));

    for entry in &app_entries {
        let node_id = parse_uuid(&entry.node_id, "node")?;
        let key = cache_key(node_id, &entry.app_name, None);
        cache.app_resolution.insert(
            key,
            AppResolution {
                user_input: entry.app_name.clone(),
                resolved_name: entry.app_name.clone(),
            },
        );
    }

    cache
        .save(&cache_path)
        .map_err(|e| format!("Failed to save cache: {e}"))?;

    tracing::info!(
        "Seeded decision cache with {} app resolution entries at {:?}",
        app_entries.len(),
        cache_path,
    );

    Ok(())
}

/// Timeout for individual VLM resolution requests (during and after recording).
const VLM_CALL_TIMEOUT: tokio::time::Duration = tokio::time::Duration::from_secs(10);

/// Timeout for CDP take_snapshot calls.
const CDP_SNAPSHOT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

// ---------------------------------------------------------------------------
// Async event processing loop
// ---------------------------------------------------------------------------

/// Process captured events: enrich with MCP data, persist, and emit to frontend.
///
/// Click enrichment (screenshot + accessibility + VLM) runs in background tasks
/// so the event loop never blocks on MCP calls and captures every click.
async fn process_capture_events(
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
fn get_recording_bar_rect(app: &tauri::AppHandle) -> Option<(f64, f64, f64, f64)> {
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
fn strip_recording_bar_click(events: &mut Vec<WalkthroughEvent>, bar_rect: (f64, f64, f64, f64)) {
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

fn persist_and_emit(
    app: &tauri::AppHandle,
    storage: &WalkthroughStorage,
    session_dir: &std::path::Path,
    event: &WalkthroughEvent,
) {
    let _ = storage.append_event(session_dir, event);
    emit_event(app, event);
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
        {
            if let Some(port_str) = flag.strip_prefix("--remote-debugging-port=") {
                if let Ok(port) = port_str.parse::<u16>() {
                    return Some(port);
                }
            }
        }
    }
    None
}

/// Pick a random port in the ephemeral range (49152–65535).
fn rand_ephemeral_port() -> u16 {
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
fn cdp_server_config(server_name: &str, port: u16) -> clickweave_mcp::McpServerConfig {
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

fn emit_cdp_progress(app: &tauri::AppHandle, app_name: &str, status: CdpSetupStatus) {
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

async fn spawn_mcp(mcp_command: &str) -> Option<McpRouter> {
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

async fn populate_app_cache(mcp: &McpRouter, cache: &mut HashMap<i32, CachedApp>) {
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

/// Enrich a click event with accessibility data and a screenshot with OCR.
///
/// Returns accessibility, screenshot, and OCR events if successful.
async fn enrich_click(
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

/// Find the first JSON object in MCP tool response content.
fn find_json_in_content(content: &[clickweave_mcp::ToolContent]) -> Option<serde_json::Value> {
    content.iter().find_map(|item| {
        item.as_text()
            .and_then(|text| serde_json::from_str::<serde_json::Value>(text).ok())
    })
}

/// Parse the `element_at_point` MCP response into `(label, role)`.
///
/// Picks the best display text from the response fields:
/// `name` (AXTitle) > `value` (AXValue) > `label` (AXDescription).
fn parse_accessibility_result(
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
fn parse_screenshot_metadata(
    content: &[clickweave_mcp::ToolContent],
) -> Option<clickweave_core::walkthrough::ScreenshotMeta> {
    let obj = find_json_in_content(content)?;
    Some(clickweave_core::walkthrough::ScreenshotMeta {
        origin_x: obj["screenshot_origin_x"].as_f64()?,
        origin_y: obj["screenshot_origin_y"].as_f64()?,
        scale: obj["screenshot_scale"].as_f64()?,
    })
}

/// Data needed to fire a VLM request for a single click.
struct VlmClickRequest {
    image_b64: String,
    prompt: String,
}

/// Prepare a VLM request for a single click: read screenshot, mark crosshair,
/// build prompt with context hints. Returns `None` if prerequisites are missing.
fn prepare_vlm_click_request(
    screenshot_path: &str,
    click_x: f64,
    click_y: f64,
    meta: clickweave_core::walkthrough::ScreenshotMeta,
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
async fn execute_vlm_click_request(
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
async fn resolve_click_targets_with_vlm(
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
        meta: clickweave_core::walkthrough::ScreenshotMeta,
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

/// Half-size of the click crop in screen points (32pt radius → 64pt square →
/// 128px on Retina). On macOS this re-exports the platform constant; on other
/// platforms it's defined inline.
#[cfg(target_os = "macos")]
use crate::platform::macos::CURSOR_REGION_HALF_PT as CROP_HALF_SIZE_PTS;
#[cfg(not(target_os = "macos"))]
const CROP_HALF_SIZE_PTS: f64 = 32.0;

/// Crop a region around the click point from a screenshot and encode as JPEG.
///
/// `img` — decoded screenshot (raw RGBA from pre-hover buffer or PNG from disk).
/// `(px, py)` — click position in **image-pixel** coordinates.
/// `scale` — display scale factor (e.g. 2.0 for Retina).
///
/// Returns `(jpeg_bytes_for_disk, base64_jpeg)`, or `None` on failure.
fn crop_click_region(
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
fn mark_click_point(png_bytes: &[u8], px: f64, py: f64) -> Option<String> {
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
