use std::sync::Mutex;

use clickweave_core::app_detection::{bundle_path_from_pid, classify_app};
use clickweave_core::storage::now_millis;
use clickweave_core::walkthrough::{
    ActionConfidence, AppKind, WalkthroughAction, WalkthroughActionKind, WalkthroughAnnotations,
    WalkthroughEvent, WalkthroughEventKind, WalkthroughSession, WalkthroughStatus,
    WalkthroughStorage,
};
use tauri::{Emitter, Manager};
use uuid::Uuid;

use super::types::{AppDataDir, WalkthroughDraftPayload, WalkthroughStatePayload, parse_uuid};
use crate::platform::CaptureCommand;

#[cfg(target_os = "macos")]
use crate::platform::macos::MacOSEventTap;

// Re-export from submodules for use within the commands crate.
pub use super::walkthrough_session::WalkthroughHandle;

use super::walkthrough_enrichment::{
    RecordedFrame, attach_recording_frames, generate_hover_screenshots,
    resolve_click_targets_with_vlm,
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
pub async fn detect_cdp_apps(mcp_command: String) -> Result<Vec<DetectedCdpApp>, String> {
    let mcp = spawn_mcp(&mcp_command)
        .await
        .ok_or("Failed to spawn MCP server")?;

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

#[tauri::command]
#[specta::specta]
pub async fn start_walkthrough(
    app: tauri::AppHandle,
    workflow_id: String,
    mcp_command: String,
    project_path: Option<String>,
    planner: Option<super::types::EndpointConfig>,
    cdp_apps: Vec<CdpAppConfig>,
    hover_dwell_threshold: Option<u64>,
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
        let hover_dwell_ms = hover_dwell_threshold.unwrap_or(2000);
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
                hover_dwell_ms,
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
    hover_dwell_threshold: Option<u64>,
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

/// Maximum time window (ms) after a hover to look for a subsuming click.
const HOVER_CLICK_WINDOW_MS: u64 = 2000;

/// Retrieve hover candidates from HoverDetected events captured during recording.
///
/// Filters by dwell threshold and removes hovers immediately followed by a click
/// on the same location (the click subsumes the hover).
fn retrieve_hover_candidates(
    events: &[WalkthroughEvent],
    hover_threshold_ms: u64,
) -> Vec<WalkthroughAction> {
    let mut candidates = Vec::new();

    // Pre-collect AppFocused events sorted by timestamp so we can resolve
    // both previous and next focus for any hover, regardless of file
    // append order (hover events are written after recording stops, so
    // they appear at the end of events.jsonl, not at their chronological
    // position).
    let mut focus_events: Vec<(u64, String, Option<String>)> = events
        .iter()
        .filter_map(|e| match &e.kind {
            WalkthroughEventKind::AppFocused {
                app_name,
                window_title,
                ..
            } => Some((e.timestamp, app_name.clone(), window_title.clone())),
            _ => None,
        })
        .collect();
    focus_events.sort_by_key(|(ts, _, _)| *ts);

    for event in events {
        let WalkthroughEventKind::HoverDetected {
            x,
            y,
            element_name,
            element_role,
            dwell_ms,
            app_name,
        } = &event.kind
        else {
            continue;
        };

        // Filter by dwell threshold.
        if *dwell_ms < hover_threshold_ms {
            continue;
        }

        // Skip window-level hovers — these capture the window title (e.g.
        // "#general | DevCrew - Discord") rather than the specific element
        // the user is hovering on.  Common with Electron/Chrome apps where
        // macOS accessibility can't resolve finer-grained elements.
        if element_role.as_deref() == Some("AXWindow") {
            continue;
        }

        // Skip if any click near the same coordinates occurred shortly after
        // this hover (the click subsumes the hover intent).  Scans all events
        // because hover entries may be appended after clicks in the file.
        let click_follows = events.iter().any(|e| {
            matches!(
                &e.kind,
                WalkthroughEventKind::MouseClicked { x: cx, y: cy, .. }
                if (cx - x).abs() < 20.0 && (cy - y).abs() < 20.0
                    && e.timestamp > event.timestamp
                    && e.timestamp.saturating_sub(event.timestamp) < HOVER_CLICK_WINDOW_MS
            )
        });
        if click_follows {
            continue;
        }

        // For CDP hovers, coordinate matching doesn't work (clientX/clientY vs
        // screen coords).  Match on name+role against CdpClickResolved instead.
        // Use CdpHoverResolved presence (not app_name) to detect CDP provenance,
        // since native hovers can also carry app_name.
        let is_cdp_hover = events.iter().any(|e| {
            matches!(
                &e.kind,
                WalkthroughEventKind::CdpHoverResolved { hover_event_id, .. }
                if *hover_event_id == event.id
            )
        });
        if is_cdp_hover {
            let matches_click = events.iter().any(|e| {
                if let WalkthroughEventKind::CdpClickResolved {
                    name,
                    role: click_role,
                    ..
                } = &e.kind
                {
                    e.timestamp > event.timestamp
                        && e.timestamp.saturating_sub(event.timestamp) < HOVER_CLICK_WINDOW_MS
                        && name == element_name
                        && match (element_role, click_role) {
                            (Some(hr), Some(cr)) => hr == cr,
                            _ => true, // if either role is missing, name match is sufficient
                        }
                } else {
                    false
                }
            });
            if matches_click {
                continue;
            }
        }

        // Use explicit app_name from CDP if present; fall back to timestamp resolution.
        let (hover_app, hover_window) = if let Some(explicit_app) = app_name {
            let title = focus_events
                .iter()
                .rev()
                .find(|(_, a, _)| a == explicit_app)
                .and_then(|(_, _, t)| t.clone());
            (Some(explicit_app.clone()), title)
        } else {
            resolve_hover_app(event.timestamp, &focus_events)
        };

        let mut target_candidates = vec![];

        // Check for CDP DOM resolution for this hover event.
        let cdp_resolved = events.iter().find_map(|e| {
            if let WalkthroughEventKind::CdpHoverResolved {
                hover_event_id,
                name,
                role,
                href,
                parent_role,
                parent_name,
            } = &e.kind
                && *hover_event_id == event.id
            {
                return Some((
                    name.clone(),
                    role.clone(),
                    href.clone(),
                    parent_role.clone(),
                    parent_name.clone(),
                ));
            }
            None
        });

        if let Some((name, role, href, parent_role, parent_name)) = cdp_resolved {
            target_candidates.push(clickweave_core::walkthrough::TargetCandidate::CdpElement {
                name,
                role,
                href,
                parent_role,
                parent_name,
            });
        }

        if !element_name.is_empty() {
            target_candidates.push(
                clickweave_core::walkthrough::TargetCandidate::AccessibilityLabel {
                    label: element_name.clone(),
                    role: element_role.clone(),
                },
            );
        }

        candidates.push(WalkthroughAction {
            id: Uuid::new_v4(),
            kind: WalkthroughActionKind::Hover {
                x: *x,
                y: *y,
                dwell_ms: *dwell_ms,
            },
            app_name: hover_app,
            window_title: hover_window,
            target_candidates,
            artifact_paths: vec![],
            source_event_ids: vec![event.id],
            confidence: ActionConfidence::Medium,
            warnings: vec![],
            screenshot_meta: None,
            candidate: true,
        });
    }

    candidates
}

/// Determine which app a hover event belongs to.
///
/// Default: use the chronologically preceding `AppFocused` event (the app
/// that was focused when the hover occurred).  Override with the *next*
/// focus only when the hover falls within a short transition window — the
/// brief period where the cursor has entered the new app's window but the
/// PID-based focus detection hasn't fired yet.
///
/// Both lookups use the pre-collected, timestamp-sorted focus list rather
/// than depending on file append order, because hover events are written
/// to `events.jsonl` after recording stops (not at their chronological
/// position).
fn resolve_hover_app(
    hover_ts: u64,
    focus_events: &[(u64, String, Option<String>)],
) -> (Option<String>, Option<String>) {
    /// Maximum gap (ms) between a hover and the *next* AppFocused event for
    /// the hover to be considered a transition hover belonging to the
    /// incoming app.  Kept short so legitimate hovers near a focus change
    /// aren't misattributed.
    const TRANSITION_WINDOW_MS: u64 = 500;

    let prev = focus_events.iter().rev().find(|(ts, _, _)| *ts <= hover_ts);
    let next = focus_events.iter().find(|(ts, _, _)| *ts > hover_ts);

    match (prev, next) {
        (Some((_, papp, ptitle)), Some((nts, napp, ntitle))) => {
            let dist_next = nts - hover_ts;
            if dist_next <= TRANSITION_WINDOW_MS {
                (Some(napp.clone()), ntitle.clone())
            } else {
                (Some(papp.clone()), ptitle.clone())
            }
        }
        (Some((_, app, title)), None) => (Some(app.clone()), title.clone()),
        (None, Some((_, app, title))) => (Some(app.clone()), title.clone()),
        (None, None) => (None, None),
    }
}

/// Find the chronological insertion position for a hover candidate action
/// based on its source event timestamp, relative to existing actions.
///
/// Uses "insert after the last action at or before the hover's timestamp"
/// rather than "insert before the first action after." Hover transition
/// events fire right before the click that triggers AppFocused, so placing
/// them *after* nearby actions keeps hovers behind the Launch/Focus setup
/// they logically belong to.
fn find_chronological_insert_position(
    actions: &[WalkthroughAction],
    candidate: &WalkthroughAction,
    events: &[WalkthroughEvent],
) -> usize {
    let candidate_ts = candidate
        .source_event_ids
        .first()
        .and_then(|id| events.iter().find(|e| e.id == *id))
        .map(|e| e.timestamp)
        .unwrap_or(u64::MAX);

    // Find the last action whose source event timestamp is at or before the
    // candidate's, then insert after it.
    let mut insert_after: Option<usize> = None;
    for (i, action) in actions.iter().enumerate() {
        let action_ts = action
            .source_event_ids
            .first()
            .and_then(|id| events.iter().find(|e| e.id == *id))
            .map(|e| e.timestamp)
            .unwrap_or(0);
        if action_ts <= candidate_ts {
            insert_after = Some(i);
        }
    }
    insert_after.map_or(0, |i| i + 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use clickweave_core::walkthrough::{AppKind, WalkthroughEvent, WalkthroughEventKind};
    use uuid::Uuid;

    fn focus_event(ts: u64, app: &str) -> WalkthroughEvent {
        WalkthroughEvent {
            id: Uuid::new_v4(),
            timestamp: ts,
            kind: WalkthroughEventKind::AppFocused {
                app_name: app.to_string(),
                pid: 1,
                window_title: Some(format!("{app} Window")),
                app_kind: AppKind::Native,
            },
        }
    }

    fn hover_event(ts: u64, dwell_ms: u64) -> WalkthroughEvent {
        hover_event_with_app(ts, dwell_ms, None)
    }

    #[test]
    fn hover_within_transition_window_gets_next_app() {
        // Hover fires within 500ms of the next focus (transition hover).
        let events = vec![
            focus_event(1000, "Discord"),
            hover_event(1700, 1500), // 300ms before Signal → within window
            focus_event(2000, "Signal"),
        ];
        let candidates = retrieve_hover_candidates(&events, 1000);
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].app_name.as_deref(), Some("Signal"));
        assert_eq!(candidates[0].window_title.as_deref(), Some("Signal Window"));
    }

    #[test]
    fn hover_outside_transition_window_keeps_previous_app() {
        // Hover fires 900ms before Signal — outside the 500ms window.
        let events = vec![
            focus_event(1000, "Discord"),
            hover_event(1100, 1500), // 900ms before Signal → Discord
            focus_event(2000, "Signal"),
        ];
        let candidates = retrieve_hover_candidates(&events, 1000);
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].app_name.as_deref(), Some("Discord"));
    }

    #[test]
    fn hover_with_no_next_focus_uses_previous() {
        // No subsequent focus event — must fall back to previous.
        let events = vec![focus_event(1000, "Discord"), hover_event(5000, 1500)];
        let candidates = retrieve_hover_candidates(&events, 1000);
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].app_name.as_deref(), Some("Discord"));
    }

    #[test]
    fn hover_before_any_focus_uses_next_focus() {
        // Hover precedes all AppFocused events — only next exists.
        let events = vec![hover_event(500, 1500), focus_event(1000, "Signal")];
        let candidates = retrieve_hover_candidates(&events, 1000);
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].app_name.as_deref(), Some("Signal"));
    }

    #[test]
    fn hover_appended_after_all_focus_events_uses_preceding_app() {
        // Real file ordering: focus events first, then hovers appended
        // at end with earlier timestamps.
        let events = vec![
            focus_event(1000, "Discord"),
            focus_event(5000, "Signal"),
            // Hovers at file end:
            hover_event(4800, 1500), // 200ms before Signal → transition → Signal
            hover_event(1200, 1500), // 3800ms before Signal → Discord
        ];
        let candidates = retrieve_hover_candidates(&events, 1000);
        assert_eq!(candidates.len(), 2);
        // ts=4800: within 500ms of Signal(5000) → transition → Signal
        assert_eq!(candidates[0].app_name.as_deref(), Some("Signal"));
        // ts=1200: 3800ms from Signal → stays with Discord
        assert_eq!(candidates[1].app_name.as_deref(), Some("Discord"));
    }

    fn hover_event_with_app(ts: u64, dwell_ms: u64, app: Option<&str>) -> WalkthroughEvent {
        WalkthroughEvent {
            id: Uuid::new_v4(),
            timestamp: ts,
            kind: WalkthroughEventKind::HoverDetected {
                x: 100.0,
                y: 200.0,
                element_name: "Button".to_string(),
                element_role: Some("AXButton".to_string()),
                dwell_ms,
                app_name: app.map(|s| s.to_string()),
            },
        }
    }

    #[test]
    fn hover_with_app_name_uses_it_directly() {
        let events = vec![
            focus_event(1000, "Signal"),
            hover_event_with_app(1100, 1500, Some("Discord")),
            focus_event(2000, "Discord"),
        ];
        let candidates = retrieve_hover_candidates(&events, 1000);
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].app_name.as_deref(), Some("Discord"));
    }

    #[test]
    fn native_hover_with_app_name_not_subsumed_by_cdp_click() {
        // Native hover carries app_name (from MCP element.app_name) but has no
        // paired CdpHoverResolved. A CdpClickResolved on the same name/role
        // should NOT subsume it — the CDP click-matching guard only applies to
        // CDP hovers (identified by CdpHoverResolved presence).
        use clickweave_core::walkthrough::WalkthroughEventKind;
        let events = vec![
            focus_event(1000, "Finder"),
            hover_event_with_app(2000, 1500, Some("Finder")),
            WalkthroughEvent {
                id: Uuid::new_v4(),
                timestamp: 3000,
                kind: WalkthroughEventKind::CdpClickResolved {
                    name: "Button".to_string(),
                    role: Some("AXButton".to_string()),
                    href: None,
                    parent_role: None,
                    parent_name: None,
                    click_event_id: Uuid::new_v4(),
                },
            },
        ];
        let candidates = retrieve_hover_candidates(&events, 1000);
        assert_eq!(
            candidates.len(),
            1,
            "native hover should not be subsumed by CDP click"
        );
    }

    #[test]
    fn hover_text_matching_next_click_filtered_out() {
        use clickweave_core::walkthrough::WalkthroughEventKind;
        let hover_id = Uuid::new_v4();
        let events = vec![
            focus_event(1000, "App"),
            WalkthroughEvent {
                id: hover_id,
                timestamp: 2000,
                kind: WalkthroughEventKind::HoverDetected {
                    x: 0.0,
                    y: 0.0,
                    element_name: "Submit".to_string(),
                    element_role: Some("button".to_string()),
                    dwell_ms: 1200,
                    app_name: Some("App".to_string()),
                },
            },
            WalkthroughEvent {
                id: Uuid::new_v4(),
                timestamp: 2000,
                kind: WalkthroughEventKind::CdpHoverResolved {
                    hover_event_id: hover_id,
                    name: "Submit".to_string(),
                    role: Some("button".to_string()),
                    href: None,
                    parent_role: None,
                    parent_name: None,
                },
            },
            WalkthroughEvent {
                id: Uuid::new_v4(),
                timestamp: 3000,
                kind: WalkthroughEventKind::MouseClicked {
                    x: 100.0,
                    y: 200.0,
                    button: clickweave_core::MouseButton::Left,
                    click_count: 1,
                    modifiers: vec![],
                },
            },
            WalkthroughEvent {
                id: Uuid::new_v4(),
                timestamp: 3000,
                kind: WalkthroughEventKind::CdpClickResolved {
                    name: "Submit".to_string(),
                    role: Some("button".to_string()),
                    href: None,
                    parent_role: None,
                    parent_name: None,
                    click_event_id: Uuid::new_v4(),
                },
            },
        ];
        let candidates = retrieve_hover_candidates(&events, 1000);
        assert_eq!(
            candidates.len(),
            0,
            "hover matching next click target should be filtered"
        );
    }
}
