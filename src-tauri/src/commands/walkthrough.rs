use std::sync::Mutex;

use clickweave_core::AppKind;
use clickweave_core::app_detection::{bundle_path_from_pid, classify_app};
use clickweave_core::storage::now_millis;
use clickweave_core::walkthrough::{
    WalkthroughAction, WalkthroughAnnotations, WalkthroughEvent, WalkthroughEventKind,
    WalkthroughSession, WalkthroughStatus, WalkthroughStorage,
};
use tauri::{Emitter, Manager};
use uuid::Uuid;

use super::error::CommandError;
use super::types::{AppDataDir, WalkthroughDraftPayload, WalkthroughStatePayload, parse_uuid};
use crate::platform::CaptureCommand;

#[cfg(target_os = "macos")]
use crate::platform::macos::MacOSEventTap;

#[cfg(target_os = "windows")]
use crate::platform::windows::WindowsEventHook;

// Re-export from submodules for use within the commands crate.
pub use super::walkthrough_session::WalkthroughHandle;

use clickweave_core::walkthrough::enrichment::RecordedFrame;
use clickweave_core::walkthrough::session::{
    find_chronological_insert_position, retrieve_hover_candidates,
};

use super::walkthrough_enrichment::{
    attach_recording_frames, generate_hover_screenshots, resolve_click_targets_with_vlm,
};
use super::walkthrough_session::{
    get_recording_bar_rect, populate_app_cache, process_capture_events, spawn_mcp,
    strip_recording_bar_click,
};

pub(super) const RECORDING_BAR_LABEL: &str = "recording-bar";
pub(super) const SELF_APP_NAME: &str = "clickweave-tauri";

/// Timeout for individual VLM resolution requests (during and after recording).
pub(super) const VLM_CALL_TIMEOUT: tokio::time::Duration = tokio::time::Duration::from_secs(10);

/// Timeout for CDP take_snapshot calls.
pub(super) const CDP_SNAPSHOT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

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
pub async fn detect_cdp_apps() -> Result<Vec<DetectedCdpApp>, CommandError> {
    let mcp_binary =
        crate::mcp_resolve::resolve_mcp_binary().map_err(|e| CommandError::mcp(format!("{e}")))?;
    let mcp = spawn_mcp(&mcp_binary)
        .await
        .ok_or(CommandError::mcp("Failed to spawn MCP server"))?;

    let mut cache = std::collections::HashMap::new();
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
pub async fn validate_app_path(path: String) -> Result<DetectedCdpApp, CommandError> {
    let kind = classify_app(None, Some(std::path::Path::new(&path)));
    if !kind.uses_cdp() {
        return Err(CommandError::validation(format!(
            "Not an Electron or Chrome app: {}",
            path
        )));
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

#[tauri::command]
#[specta::specta]
pub async fn start_walkthrough(
    app: tauri::AppHandle,
    workflow_id: String,
    project_path: Option<String>,
    planner: Option<super::types::EndpointConfig>,
    cdp_apps: Vec<CdpAppConfig>,
    hover_dwell_threshold: Option<u64>,
) -> Result<(), CommandError> {
    let wf_id = parse_uuid(&workflow_id, "workflow")?;

    // Resolve MCP binary before acquiring the session lock so a failure
    // doesn't leave walkthrough state wedged as "already running".
    let mcp_binary_path =
        crate::mcp_resolve::resolve_mcp_binary().map_err(|e| CommandError::mcp(format!("{e}")))?;

    // Set up session and storage under the lock, then release it before
    // spawning async work (which needs the app handle, not the lock).
    let (session_dir, processing_storage, cancel) = {
        let handle = app.state::<Mutex<WalkthroughHandle>>();
        let mut guard = handle.lock().unwrap();

        if guard.session.is_some() {
            return Err(CommandError::already_running());
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
            .map_err(|e| CommandError::io(format!("Failed to create session dir: {e}")))?;

        storage
            .save_session(&session_dir, &session)
            .map_err(|e| CommandError::io(format!("Failed to save initial session: {e}")))?;

        let processing_storage = storage.clone();
        guard.session = Some(session);
        guard.session_dir = Some(session_dir.clone());
        guard.storage = Some(storage);
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
    };

    // Start the platform event tap and processing loop.
    #[cfg(target_os = "macos")]
    {
        let (event_tap, event_rx) = match MacOSEventTap::start() {
            Ok(pair) => pair,
            Err(e) => {
                clear_session(&app);
                return Err(CommandError::internal(format!(
                    "Failed to start event tap: {e}"
                )));
            }
        };

        let emit_handle = app.clone();
        let hover_dwell_ms = hover_dwell_threshold.unwrap_or(2000);
        let mcp_path = mcp_binary_path.clone();
        let processing_task = tauri::async_runtime::spawn(async move {
            process_capture_events(
                emit_handle,
                event_rx,
                mcp_path,
                planner,
                processing_storage,
                session_dir,
                cancel,
                cdp_apps,
                hover_dwell_ms,
            )
            .await;
        });

        let handle = app.state::<Mutex<WalkthroughHandle>>();
        let mut guard = handle.lock().unwrap();
        guard.event_tap = Some(event_tap);
        guard.processing_task = Some(processing_task);
    }

    #[cfg(target_os = "windows")]
    {
        let (event_hook, event_rx) = match WindowsEventHook::start() {
            Ok(pair) => pair,
            Err(e) => {
                clear_session(&app);
                return Err(CommandError::internal(format!(
                    "Failed to start event hook: {e}"
                )));
            }
        };

        let emit_handle = app.clone();
        let hover_dwell_ms = hover_dwell_threshold.unwrap_or(2000);
        let mcp_path = mcp_binary_path.clone();
        let processing_task = tauri::async_runtime::spawn(async move {
            process_capture_events(
                emit_handle,
                event_rx,
                mcp_path,
                planner,
                processing_storage,
                session_dir,
                cancel,
                cdp_apps,
                hover_dwell_ms,
            )
            .await;
        });

        let handle = app.state::<Mutex<WalkthroughHandle>>();
        let mut guard = handle.lock().unwrap();
        guard.event_hook = Some(event_hook);
        guard.processing_task = Some(processing_task);
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        clear_session(&app);
        return Err(CommandError::internal(
            "Walkthrough capture is only supported on macOS and Windows",
        ));
    }

    emit_state(&app, WalkthroughStatus::Recording);
    tracing::info!("Walkthrough session started for workflow {workflow_id}");
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub async fn pause_walkthrough(app: tauri::AppHandle) -> Result<(), CommandError> {
    let handle = app.state::<Mutex<WalkthroughHandle>>();
    let mut guard = handle.lock().unwrap();

    guard.ensure_status(&[WalkthroughStatus::Recording])?;
    guard.session.as_mut().unwrap().status = WalkthroughStatus::Paused;

    #[cfg(target_os = "macos")]
    if let Some(tap) = &guard.event_tap {
        tap.send_command(CaptureCommand::Pause);
    }

    #[cfg(target_os = "windows")]
    if let Some(hook) = &guard.event_hook {
        hook.send_command(CaptureCommand::Pause);
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
pub async fn resume_walkthrough(app: tauri::AppHandle) -> Result<(), CommandError> {
    let handle = app.state::<Mutex<WalkthroughHandle>>();
    let mut guard = handle.lock().unwrap();

    guard.ensure_status(&[WalkthroughStatus::Paused])?;
    guard.session.as_mut().unwrap().status = WalkthroughStatus::Recording;

    #[cfg(target_os = "macos")]
    if let Some(tap) = &guard.event_tap {
        tap.send_command(CaptureCommand::Resume);
    }

    #[cfg(target_os = "windows")]
    if let Some(hook) = &guard.event_hook {
        hook.send_command(CaptureCommand::Resume);
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
    hover_dwell_threshold: Option<u64>,
) -> Result<(), CommandError> {
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
                .map_err(|e| CommandError::io(format!("Failed to read events: {e}")))?;

            // Strip the stop-button click captured just before the tap shut down.
            if let Some(bar_rect) = get_recording_bar_rect(&app) {
                strip_recording_bar_click(&mut events, bar_rect);
            }

            // Normalize.
            let (mut actions, mut norm_warnings) =
                clickweave_core::walkthrough::normalize_events(&events);

            // Hover: retrieve hover events and convert to candidate actions.
            let hover_candidates =
                retrieve_hover_candidates(&events, hover_dwell_threshold.unwrap_or(2000));
            for candidate in hover_candidates {
                let insert_idx = find_chronological_insert_position(&actions, &candidate, &events);
                actions.insert(insert_idx, candidate);
            }

            // Attach before/after recording frames to hover candidates so
            // VLM can compare pre-hover and post-hover visual state.
            let frames_path = dir.join("recording_frames.json");
            let recording_frames: Vec<RecordedFrame> = std::fs::read_to_string(&frames_path)
                .ok()
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or_default();
            if recording_frames.is_empty() {
                tracing::warn!(
                    "No recording frames available — hover candidates will lack screenshots"
                );
            } else {
                attach_recording_frames(&mut actions, &recording_frames, &events);
            }

            // Generate crosshair-marked screenshots for hover candidates
            // so the review panel shows where each hover was on the window.
            generate_hover_screenshots(&mut actions, dir).await;

            // VLM: resolve click and hover targets using vision (parallel).
            if let Some(ref planner_cfg) = planner {
                resolve_click_targets_with_vlm(&mut actions, planner_cfg).await;
            }

            // Clean up raw recording frames — they're no longer needed after
            // hover screenshots have been generated and VLM has resolved targets.
            // The raw stream may contain unrelated app states and typed secrets.
            if !recording_frames.is_empty() {
                let mut cleaned = 0u32;
                for frame in &recording_frames {
                    if std::fs::remove_file(&frame.path).is_ok() {
                        cleaned += 1;
                    }
                }
                let _ = std::fs::remove_file(&frames_path);
                if cleaned > 0 {
                    tracing::info!("Cleaned up {cleaned} raw recording frames");
                }
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
) -> Result<WalkthroughDraftResponse, CommandError> {
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
                serde_json::from_str(&data)
                    .map_err(|e| CommandError::validation(format!("Failed to parse draft: {e}")))?,
            ),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(CommandError::io(format!("Failed to read draft: {e}"))),
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
pub async fn cancel_walkthrough(app: tauri::AppHandle) -> Result<(), CommandError> {
    let (task, session_dir) = {
        let handle = app.state::<Mutex<WalkthroughHandle>>();
        let mut guard = handle.lock().unwrap();

        if guard.session.is_none() {
            return Err(CommandError::validation("No walkthrough session is active"));
        }

        let task = guard.stop_capture();
        let dir = guard.session_dir.take();

        guard.session = None;
        guard.storage = None;

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
) -> Result<(), CommandError> {
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
) -> Result<(), CommandError> {
    use clickweave_core::decision_cache::{AppResolution, DecisionCache, cache_key};

    let wf_id = parse_uuid(&workflow_id, "workflow")?;

    if app_entries.is_empty() {
        return Ok(());
    }

    let storage = super::types::resolve_storage(&app, &project_path, &workflow_name, wf_id);
    let cache_path = storage.cache_path();

    // Load existing cache or create new one.
    let mut cache =
        DecisionCache::load(&cache_path, wf_id).unwrap_or_else(|| DecisionCache::new(wf_id));

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
        .map_err(|e| CommandError::io(format!("Failed to save cache: {e}")))?;

    tracing::info!(
        "Seeded decision cache with {} app resolution entries at {:?}",
        app_entries.len(),
        cache_path,
    );

    Ok(())
}
