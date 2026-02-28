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

/// Manages the walkthrough recording lifecycle.
#[derive(Default)]
pub struct WalkthroughHandle {
    pub session: Option<WalkthroughSession>,
    pub session_dir: Option<std::path::PathBuf>,
    storage: Option<WalkthroughStorage>,
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

    fn stop_capture(&mut self) {
        #[cfg(target_os = "macos")]
        if let Some(tap) = self.event_tap.take() {
            tap.send_command(CaptureCommand::Stop);
            // Drop the tap handle — this joins the thread.
            drop(tap);
        }
        if let Some(task) = self.processing_task.take() {
            task.abort();
        }
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

        (session_dir, processing_storage)
    };

    // Start the platform event tap.
    #[cfg(target_os = "macos")]
    let (event_tap, event_rx) =
        MacOSEventTap::start().map_err(|e| format!("Failed to start event tap: {e}"))?;

    // Spawn the async processing loop.
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

    // Store handles under the lock.
    {
        let handle = app.state::<Mutex<WalkthroughHandle>>();
        let mut guard = handle.lock().unwrap();
        #[cfg(target_os = "macos")]
        {
            guard.event_tap = Some(event_tap);
        }
        guard.processing_task = Some(processing_task);
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
pub async fn stop_walkthrough(app: tauri::AppHandle) -> Result<(), String> {
    let handle = app.state::<Mutex<WalkthroughHandle>>();
    let mut guard = handle.lock().unwrap();

    guard.ensure_status(&[WalkthroughStatus::Recording, WalkthroughStatus::Paused])?;
    let session = guard.session.as_mut().unwrap();
    session.status = WalkthroughStatus::Processing;
    session.ended_at = Some(now_millis());

    guard.stop_capture();

    // Persist the Stopped event.
    if let (Some(storage), Some(dir)) = (&guard.storage, &guard.session_dir) {
        let event = WalkthroughEvent {
            id: Uuid::new_v4(),
            timestamp: now_millis(),
            kind: WalkthroughEventKind::Stopped,
        };
        let _ = storage.append_event(dir, &event);
    }

    let storage = guard.storage.clone();
    let session_dir = guard.session_dir.clone();
    let workflow_id = guard.session.as_ref().unwrap().workflow_id;

    drop(guard);
    emit_state(&app, WalkthroughStatus::Processing);

    // --- Processing phase (outside the lock) ---

    let (actions, draft, warnings) = match (&storage, &session_dir) {
        (Some(storage), Some(dir)) => {
            // Read events from disk.
            let events = storage
                .read_events(dir)
                .map_err(|e| format!("Failed to read events: {e}"))?;

            // Normalize.
            let (actions, mut norm_warnings) =
                clickweave_core::walkthrough::normalize_events(&events);

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

    // Store results back on the session.
    {
        let handle = app.state::<Mutex<WalkthroughHandle>>();
        let mut guard = handle.lock().unwrap();
        if let Some(session) = guard.session.as_mut() {
            session.actions = actions.clone();
            session.warnings = warnings.clone();
            session.status = WalkthroughStatus::Review;
        }

        // Persist the updated session.
        if let (Some(storage), Some(dir)) = (&guard.storage, &guard.session_dir) {
            let _ = storage.save_session(dir, guard.session.as_ref().unwrap());
        }
    }

    // Emit results to frontend.
    let _ = app.emit(
        "walkthrough://draft_ready",
        WalkthroughDraftPayload {
            actions,
            draft,
            warnings,
        },
    );
    emit_state(&app, WalkthroughStatus::Review);

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

    guard.stop_capture();
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

    if let Some(ref mcp) = mcp {
        populate_app_cache(mcp, &mut app_cache).await;
    }

    while let Some(capture) = event_rx.recv().await {
        // Detect app focus changes.
        if capture.target_pid != 0 && capture.target_pid != last_pid {
            let app_name = resolve_app_name(capture.target_pid, &mcp, &mut app_cache).await;

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

                // Emit enrichment events first (screenshot + OCR), then click event.
                for ev in &enrichment_events {
                    persist_and_emit(&app, &storage, &session_dir, ev);
                }

                click_event
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

/// Enrich a click event by taking a screenshot with OCR.
///
/// Returns screenshot and OCR events if successful.
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

    let mut args = serde_json::json!({
        "mode": "window",
        "include_ocr": true,
    });
    if let Some(name) = app_name {
        args["app_name"] = serde_json::Value::String(name.to_string());
    }

    let result = match mcp.call_tool("take_screenshot", Some(args)).await {
        Ok(r) => r,
        Err(e) => {
            tracing::debug!("Screenshot enrichment failed: {e}");
            return vec![];
        }
    };

    let mut events = Vec::new();

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

    events
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
