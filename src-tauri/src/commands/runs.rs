use super::error::CommandError;
use super::types::*;
use clickweave_core::{NodeRun, TraceEvent};
use tracing::warn;

#[tauri::command]
#[specta::specta]
pub fn list_runs(app: tauri::AppHandle, query: RunsQuery) -> Result<Vec<NodeRun>, CommandError> {
    let workflow_id = parse_uuid(&query.workflow_id, "workflow")?;

    let storage = resolve_storage(&app, &query.project_path, &query.workflow_name, workflow_id);
    storage
        .load_runs_for_node(&query.node_name)
        .map_err(|e| CommandError::io(format!("Failed to load runs: {}", e)))
}

#[tauri::command]
#[specta::specta]
pub fn load_run_events(
    app: tauri::AppHandle,
    query: RunEventsQuery,
) -> Result<Vec<TraceEvent>, CommandError> {
    let workflow_id = parse_uuid(&query.workflow_id, "workflow")?;
    let run_id = parse_uuid(&query.run_id, "run")?;

    let storage = resolve_storage(&app, &query.project_path, &query.workflow_name, workflow_id);
    let run_dir = storage
        .find_run_dir(&query.node_name, run_id, query.execution_dir.as_deref())
        .map_err(|e| CommandError::io(format!("Failed to find run directory: {}", e)))?;
    let events_path = run_dir.join("events.jsonl");

    if !events_path.exists() {
        return Ok(vec![]);
    }

    let content = std::fs::read_to_string(&events_path)
        .map_err(|e| CommandError::io(format!("Failed to read events.jsonl: {}", e)))?;

    let mut events = Vec::new();
    let mut malformed = 0;
    for line in content.lines() {
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<TraceEvent>(line) {
            Ok(event) => events.push(event),
            Err(e) => {
                malformed += 1;
                warn!("Malformed trace event line: {}", e);
            }
        }
    }
    if malformed > 0 {
        warn!(
            "Skipped {} malformed line(s) in {}",
            malformed,
            events_path.display()
        );
    }

    Ok(events)
}

#[tauri::command]
#[specta::specta]
pub fn read_artifact_base64(path: String) -> Result<String, CommandError> {
    use base64::Engine;
    let data = std::fs::read(&path)
        .map_err(|e| CommandError::io(format!("Failed to read artifact: {}", e)))?;
    Ok(base64::engine::general_purpose::STANDARD.encode(&data))
}
