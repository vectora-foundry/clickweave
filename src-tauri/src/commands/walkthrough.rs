use std::collections::HashMap;
use std::sync::Mutex;

use base64::Engine;
use clickweave_core::storage::now_millis;
use clickweave_core::walkthrough::{
    OcrAnnotation, ScreenshotKind, WalkthroughAction, WalkthroughAnnotations, WalkthroughEvent,
    WalkthroughEventKind, WalkthroughSession, WalkthroughStatus, WalkthroughStorage,
};
use clickweave_mcp::McpClient;
use tauri::{Emitter, Manager};
use uuid::Uuid;

use super::types::{
    AppDataDir, WalkthroughDraftPayload, WalkthroughEventPayload, WalkthroughStatePayload,
    parse_uuid,
};
use crate::platform::{CaptureCommand, CaptureEvent, CaptureEventKind};

#[cfg(target_os = "macos")]
use crate::platform::macos::MacOSEventTap;

const RECORDING_BAR_LABEL: &str = "recording-bar";
const SELF_APP_NAME: &str = "clickweave-tauri";

/// Maximum length of a VLM-resolved label to accept. Longer responses
/// are likely full sentences rather than a concise element name.
const VLM_LABEL_MAX_LEN: usize = 80;

/// Manages the walkthrough recording lifecycle.
#[derive(Default)]
pub struct WalkthroughHandle {
    pub session: Option<WalkthroughSession>,
    pub session_dir: Option<std::path::PathBuf>,
    storage: Option<WalkthroughStorage>,
    mcp_command: Option<String>,
    #[cfg(target_os = "macos")]
    event_tap: Option<MacOSEventTap>,
    processing_task: Option<tauri::async_runtime::JoinHandle<()>>,
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
    /// Dropping the event tap closes the channel sender, so the processing
    /// loop will drain remaining events and exit naturally. The caller
    /// should await the returned handle (with a timeout) instead of aborting.
    fn stop_capture(&mut self) -> Option<tauri::async_runtime::JoinHandle<()>> {
        #[cfg(target_os = "macos")]
        if let Some(tap) = self.event_tap.take() {
            tap.send_command(CaptureCommand::Stop);
            // Drop the tap handle — this joins the thread and closes the sender.
            drop(tap);
        }
        self.processing_task.take()
    }
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
) -> Result<(), String> {
    let wf_id = parse_uuid(&workflow_id, "workflow")?;

    // Set up session and storage under the lock, then release it before
    // spawning async work (which needs the app handle, not the lock).
    let (session_dir, processing_storage) = {
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

        (session_dir, processing_storage)
    };

    // Helper to roll back session state and clean up the session directory on failure.
    let clear_session = |app: &tauri::AppHandle| {
        let handle = app.state::<Mutex<WalkthroughHandle>>();
        let mut guard = handle.lock().unwrap();
        if let Some(dir) = &guard.session_dir {
            if let Err(e) = std::fs::remove_dir_all(dir) {
                tracing::warn!("Failed to clean up session dir on rollback: {e}");
            }
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
                processing_storage,
                session_dir,
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
    let (processing_task, storage, session_dir, workflow_id, session_id, mcp_command) = {
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
            guard.mcp_command.clone(),
        )
    };
    emit_state(&app, WalkthroughStatus::Processing);

    // Wait for the processing task to drain remaining events (with timeout).
    if let Some(task) = processing_task {
        let drain_timeout = tokio::time::Duration::from_secs(5);
        if tokio::time::timeout(drain_timeout, task).await.is_err() {
            tracing::warn!("Processing task did not finish draining within timeout");
        }
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

    // --- LLM generalization (optional) ---

    let (final_draft, action_node_map, used_fallback, all_warnings) =
        if let Some(planner_cfg) = planner {
            if planner_cfg.is_empty() || actions.is_empty() {
                let map = clickweave_core::walkthrough::build_action_node_map(&actions, &draft);
                (draft, map, true, warnings)
            } else {
                // Fetch MCP tool schemas for the prompt.
                let mcp_tools = match &mcp_command {
                    Some(cmd) => super::planner::fetch_mcp_tool_schemas(cmd)
                        .await
                        .unwrap_or_else(|e| {
                            tracing::warn!("Failed to fetch MCP tools for generalization: {e}");
                            vec![]
                        }),
                    None => vec![],
                };

                let llm_config = planner_cfg.into_llm_config(None);
                let backend = clickweave_llm::LlmClient::new(llm_config);
                let result = clickweave_llm::planner::generalize_walkthrough(
                    &backend, &draft, &actions, &mcp_tools,
                )
                .await;

                let mut combined_warnings = warnings;
                combined_warnings.extend(result.warnings);
                (
                    result.workflow,
                    result.action_node_map,
                    result.used_fallback,
                    combined_warnings,
                )
            }
        } else {
            let map = clickweave_core::walkthrough::build_action_node_map(&actions, &draft);
            (draft, map, true, warnings)
        };

    // Persist final draft to disk (overwrites pre-generalization draft).
    if let (Some(storage), Some(dir)) = (&storage, &session_dir)
        && let Err(e) = storage.save_draft(dir, &final_draft)
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
        session.warnings = all_warnings.clone();
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
                draft: final_draft,
                warnings: all_warnings,
                action_node_map,
                used_fallback,
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
    let handle = app.state::<Mutex<WalkthroughHandle>>();
    let mut guard = handle.lock().unwrap();

    if guard.session.is_none() {
        return Err("No walkthrough session is active".to_string());
    }

    // For cancel, we don't need to drain — abort immediately.
    let task = guard.stop_capture();
    if let Some(task) = task {
        task.abort();
    }

    // Clean up session artifacts from disk (events, screenshots, draft).
    // The recording may contain typed secrets, so we don't leave it behind.
    if let Some(dir) = &guard.session_dir {
        if let Err(e) = std::fs::remove_dir_all(dir) {
            tracing::warn!("Failed to clean up walkthrough session dir: {e}");
        }
    }

    guard.session = None;
    guard.session_dir = None;
    guard.storage = None;

    drop(guard);
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

// ---------------------------------------------------------------------------
// Async event processing loop
// ---------------------------------------------------------------------------

/// Process captured events: enrich with MCP data, persist, and emit to frontend.
async fn process_capture_events(
    app: tauri::AppHandle,
    mut event_rx: tokio::sync::mpsc::UnboundedReceiver<CaptureEvent>,
    mcp_command: String,
    storage: WalkthroughStorage,
    session_dir: std::path::PathBuf,
) {
    // Spawn the MCP server for enrichment (screenshots + OCR).
    let mcp = spawn_mcp(&mcp_command).await;

    // Cache PID → app name to avoid repeated lookups.
    let mut app_cache: HashMap<i32, String> = HashMap::new();
    let mut last_pid: i32 = 0;
    let mut self_focused = false;

    if let Some(ref mcp) = mcp {
        populate_app_cache(mcp, &mut app_cache).await;
    }

    while let Some(capture) = event_rx.recv().await {
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

            let focus_event = WalkthroughEvent {
                id: Uuid::new_v4(),
                timestamp: capture.timestamp,
                kind: WalkthroughEventKind::AppFocused {
                    app_name: app_name.clone(),
                    pid: capture.target_pid,
                    window_title: None,
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
                    },
                };

                // Persist the click event immediately so it's never lost
                // if enrichment is slow and the drain timeout expires.
                persist_and_emit(&app, &storage, &session_dir, &click_event);

                // Enrich: take screenshot with OCR near the click point.
                let app_name = app_cache.get(&capture.target_pid).cloned();
                let enrichment_events = enrich_click(
                    &mcp,
                    &session_dir,
                    x,
                    y,
                    app_name.as_deref(),
                    capture.timestamp,
                )
                .await;

                for ev in &enrichment_events {
                    persist_and_emit(&app, &storage, &session_dir, ev);
                }

                // Already persisted above — skip the persist_and_emit at
                // the bottom of the loop.
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
/// down. This function removes that trailing click and its associated enrichment
/// events (screenshot, OCR) which share the same timestamp.
fn strip_recording_bar_click(events: &mut Vec<WalkthroughEvent>, bar_rect: (f64, f64, f64, f64)) {
    let (bar_x, bar_y, bar_w, bar_h) = bar_rect;

    let last_click_pos = events
        .iter()
        .rposition(|e| matches!(&e.kind, WalkthroughEventKind::MouseClicked { .. }));

    if let Some(idx) = last_click_pos {
        if let WalkthroughEventKind::MouseClicked { x, y, .. } = &events[idx].kind {
            if *x >= bar_x && *x <= bar_x + bar_w && *y >= bar_y && *y <= bar_y + bar_h {
                events.truncate(idx);
            }
        }
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
// MCP helpers
// ---------------------------------------------------------------------------

async fn spawn_mcp(mcp_command: &str) -> Option<McpClient> {
    let result = if mcp_command == "npx" {
        McpClient::spawn_npx().await
    } else {
        McpClient::spawn(mcp_command, &[]).await
    };

    match result {
        Ok(client) => {
            tracing::info!("MCP server spawned for walkthrough enrichment");
            Some(client)
        }
        Err(e) => {
            tracing::warn!(
                "Failed to spawn MCP server for walkthrough: {e}. Continuing without enrichment."
            );
            None
        }
    }
}

async fn populate_app_cache(mcp: &McpClient, cache: &mut HashMap<i32, String>) {
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
                            cache.insert(pid as i32, name.to_string());
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
    mcp: &Option<McpClient>,
    cache: &mut HashMap<i32, String>,
) -> String {
    if let Some(name) = cache.get(&pid) {
        return name.clone();
    }

    // Re-fetch the app list from MCP to find the new PID.
    if let Some(mcp) = mcp {
        populate_app_cache(mcp, cache).await;
        if let Some(name) = cache.get(&pid) {
            return name.clone();
        }
    }

    // Insert negative-cache entry to avoid repeated MCP calls for unknown PIDs.
    let fallback = format!("PID:{pid}");
    cache.insert(pid, fallback.clone());
    fallback
}

/// Enrich a click event with accessibility data and a screenshot with OCR.
///
/// Returns accessibility, screenshot, and OCR events if successful.
async fn enrich_click(
    mcp: &Option<McpClient>,
    session_dir: &std::path::Path,
    x: f64,
    y: f64,
    app_name: Option<&str>,
    timestamp: u64,
) -> Vec<WalkthroughEvent> {
    let mcp = match mcp.as_ref() {
        Some(m) => m,
        None => return vec![],
    };

    let mut events = Vec::new();

    // Query the accessibility element at the click point.
    let mut ax_args = serde_json::json!({ "x": x, "y": y });
    if let Some(name) = app_name {
        ax_args["app_name"] = serde_json::Value::String(name.to_string());
    }
    match mcp.call_tool("element_at_point", Some(ax_args)).await {
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
        Err(e) => {
            tracing::info!("Accessibility enrichment failed at ({x:.0}, {y:.0}): {e}");
        }
    }

    // Take screenshot with OCR for visual artifacts and text fallback.
    let mut screenshot_args = serde_json::json!({
        "mode": "window",
        "include_ocr": true,
    });
    if let Some(name) = app_name {
        screenshot_args["app_name"] = serde_json::Value::String(name.to_string());
    }

    if let Ok(result) = mcp
        .call_tool("take_screenshot", Some(screenshot_args))
        .await
    {
        // Parse screenshot metadata (origin, scale) from the JSON text content.
        let screenshot_meta = parse_screenshot_metadata(&result.content);

        // Extract screenshot image.
        for content in &result.content {
            if let clickweave_mcp::ToolContent::Image { data, .. } = content {
                let filename = format!("click_{timestamp}.png");
                let artifact_path = session_dir.join("artifacts").join(&filename);
                if let Ok(image_bytes) = base64::engine::general_purpose::STANDARD.decode(data) {
                    let _ = std::fs::write(&artifact_path, &image_bytes);
                    events.push(WalkthroughEvent {
                        id: Uuid::new_v4(),
                        timestamp,
                        kind: WalkthroughEventKind::ScreenshotCaptured {
                            path: artifact_path.to_string_lossy().to_string(),
                            kind: ScreenshotKind::AfterClick,
                            meta: screenshot_meta,
                        },
                    });
                }
            }
        }

        // Extract OCR annotations from text content.
        let annotations = parse_ocr_annotations(&result.content);
        if !annotations.is_empty() {
            events.push(WalkthroughEvent {
                id: Uuid::new_v4(),
                timestamp,
                kind: WalkthroughEventKind::OcrCaptured {
                    annotations,
                    click_x: x,
                    click_y: y,
                },
            });
        }
    }

    events
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

/// Parse OCR annotations from MCP take_screenshot response.
///
/// The MCP server returns OCR data as a markdown text content item with lines like:
/// `- "Button Text" at (123, 456) bounds: {x: 123, y: 456, w: 50, h: 20}`
fn parse_ocr_annotations(content: &[clickweave_mcp::ToolContent]) -> Vec<OcrAnnotation> {
    let mut annotations = Vec::new();
    for item in content {
        if let Some(text) = item.as_text() {
            if !text.contains("OCR Text Detected") {
                continue;
            }
            for line in text.lines() {
                let line = line.trim();
                if !line.starts_with("- \"") {
                    continue;
                }
                // Parse: - "text" at (x, y) bounds: ...
                if let Some(parsed) = parse_ocr_line(line) {
                    annotations.push(parsed);
                }
            }
        }
    }
    annotations
}

/// Parse a single OCR markdown line: `- "text" at (x, y) bounds: ...`
fn parse_ocr_line(line: &str) -> Option<OcrAnnotation> {
    // Strip leading `- "`
    let rest = line.strip_prefix("- \"")?;
    // Find closing quote before ` at (`
    let at_idx = rest.find("\" at (")?;
    let text = &rest[..at_idx];
    let after_at = &rest[at_idx + 5..]; // skip `" at (`
    let paren_end = after_at.find(')')?;
    let coords = &after_at[..paren_end];
    let mut parts = coords.split(',');
    let x: f64 = parts.next()?.trim().parse().ok()?;
    let y: f64 = parts.next()?.trim().parse().ok()?;

    Some(OcrAnnotation {
        text: text.to_string(),
        x,
        y,
    })
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

/// Use a VLM to identify click targets for all click actions (in parallel).
///
/// For each Click action that has a screenshot artifact and screenshot metadata,
/// crops a region around the click point and sends it to the VLM asking what UI
/// element was clicked. All requests are fired concurrently.
async fn resolve_click_targets_with_vlm(
    actions: &mut [WalkthroughAction],
    planner_cfg: &super::types::EndpointConfig,
) {
    use clickweave_core::walkthrough::{TargetCandidate, WalkthroughActionKind};

    if planner_cfg.is_empty() {
        return;
    }

    // Prepare VLM requests: read screenshots and crop on the main thread,
    // then fire all LLM calls in parallel.
    struct VlmRequest {
        action_idx: usize,
        image_b64: String,
    }

    let mut requests: Vec<VlmRequest> = Vec::new();

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

        let screenshot_path = match action.artifact_paths.first() {
            Some(p) => p.clone(),
            None => continue,
        };
        let meta = match &action.screenshot_meta {
            Some(m) => *m,
            None => continue,
        };

        let image_bytes = match std::fs::read(&screenshot_path) {
            Ok(bytes) => bytes,
            Err(e) => {
                tracing::warn!("VLM: failed to read {screenshot_path}: {e}");
                continue;
            }
        };

        // Compute click position in image pixel coordinates.
        let px = (click_x - meta.origin_x) * meta.scale;
        let py = (click_y - meta.origin_y) * meta.scale;

        let image_b64 = match mark_click_point(&image_bytes, px, py) {
            Some(b64) => b64,
            None => continue,
        };

        requests.push(VlmRequest {
            action_idx: idx,
            image_b64,
        });
    }

    if requests.is_empty() {
        return;
    }

    tracing::info!(
        "VLM: resolving {} click targets in parallel",
        requests.len()
    );

    let mut llm_config = planner_cfg.clone().into_llm_config(Some(0.1));
    llm_config.max_tokens = Some(4096);
    // Disable thinking/reasoning for this simple label extraction task.
    llm_config.extra_body.insert(
        "chat_template_kwargs".to_string(),
        serde_json::json!({"enable_thinking": false}),
    );
    let backend = std::sync::Arc::new(clickweave_llm::LlmClient::new(llm_config));

    let prompt = "This is a screenshot of an application window with a red \
         crosshair marking where the user clicked. What UI element is at \
         the crosshair? Return ONLY the text label or name of the element \
         (e.g., \"Send\", \"Note to Self\", \"Search\"). If there's no text \
         label, describe the element briefly (e.g., \"message input field\"). \
         Return just the label, nothing else.";

    // Fire all VLM requests in parallel.
    let mut join_set = tokio::task::JoinSet::new();

    for req in requests {
        let backend = backend.clone();
        let prompt = prompt.to_string();

        join_set.spawn(async move {
            let messages = vec![clickweave_llm::Message::user_with_images(
                prompt,
                vec![(req.image_b64, "image/jpeg".to_string())],
            )];
            let result = clickweave_llm::ChatBackend::chat(backend.as_ref(), messages, None).await;
            (req.action_idx, result)
        });
    }

    // Collect results and apply to actions.
    while let Some(join_result) = join_set.join_next().await {
        let (action_idx, llm_result) = match join_result {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!("VLM task panicked: {e}");
                continue;
            }
        };

        let (click_x, click_y) = match &actions[action_idx].kind {
            WalkthroughActionKind::Click { x, y, .. } => (*x, *y),
            _ => continue,
        };

        match llm_result {
            Ok(response) => {
                if let Some(label) = response
                    .choices
                    .first()
                    .and_then(|c| c.message.content_text())
                {
                    let label = label.trim().trim_matches('"').to_string();
                    if !label.is_empty() && label.len() <= VLM_LABEL_MAX_LEN {
                        tracing::info!(
                            "VLM resolved click at ({click_x:.0}, {click_y:.0}) → \"{label}\""
                        );
                        let action = &mut actions[action_idx];
                        // Insert VLM label after accessibility but before OCR/coordinates.
                        let insert_pos = action
                            .target_candidates
                            .iter()
                            .position(|c| !matches!(c, TargetCandidate::AccessibilityLabel { .. }))
                            .unwrap_or(action.target_candidates.len());
                        action
                            .target_candidates
                            .insert(insert_pos, TargetCandidate::VlmLabel { label });
                    }
                }
            }
            Err(e) => {
                tracing::warn!("VLM failed for click at ({click_x:.0}, {click_y:.0}): {e}");
            }
        }
    }
}

/// Downscale the full window screenshot and draw a red crosshair at the click point.
///
/// Draws a red crosshair at `(px, py)` in image-pixel coordinates, then
/// downscales + JPEG-encodes via the shared VLM image prep utility.
/// Returns `None` if the image can't be decoded.
fn mark_click_point(png_bytes: &[u8], px: f64, py: f64) -> Option<String> {
    let img = image::load_from_memory(png_bytes).ok()?;
    let (img_w, img_h) = (img.width(), img.height());

    // Draw crosshair at full resolution.
    let mut rgba = img.to_rgba8();
    let cx = (px as u32).min(img_w.saturating_sub(1));
    let cy = (py as u32).min(img_h.saturating_sub(1));
    let red = image::Rgba([255, 0, 0, 255]);
    let arm = 24u32;
    let gap = 6u32;

    for dx in gap..=arm {
        if cx + dx < img_w {
            rgba.put_pixel(cx + dx, cy, red);
        }
        if let Some(x) = cx.checked_sub(dx) {
            rgba.put_pixel(x, cy, red);
        }
    }
    for dy in gap..=arm {
        if cy + dy < img_h {
            rgba.put_pixel(cx, cy + dy, red);
        }
        if let Some(y) = cy.checked_sub(dy) {
            rgba.put_pixel(cx, y, red);
        }
    }

    let (b64, _mime) = clickweave_llm::prepare_dynimage_for_vlm(
        image::DynamicImage::ImageRgba8(rgba),
        clickweave_llm::DEFAULT_MAX_DIMENSION,
    );
    Some(b64)
}
