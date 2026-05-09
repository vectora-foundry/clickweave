use super::error::CommandError;
use super::types::*;
use clickweave_core::{NodeRun, TraceEvent};
use std::path::{Path, PathBuf};
use tracing::warn;

#[tauri::command]
#[specta::specta]
pub fn list_runs(app: tauri::AppHandle, query: RunsQuery) -> Result<Vec<NodeRun>, CommandError> {
    let project_id = parse_uuid(&query.project_id, "project")?;

    let storage = resolve_storage(&app, &query.project_path, &query.project_name, project_id);
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
    let project_id = parse_uuid(&query.project_id, "project")?;
    let run_id = parse_uuid(&query.run_id, "run")?;

    let storage = resolve_storage(&app, &query.project_path, &query.project_name, project_id);
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
pub fn read_artifact_base64(
    app: tauri::AppHandle,
    query: ReadArtifactQuery,
) -> Result<String, CommandError> {
    use base64::Engine;
    let project_id = parse_uuid(&query.project_id, "project")?;
    let run_id = parse_uuid(&query.run_id, "run")?;

    let storage = resolve_storage(&app, &query.project_path, &query.project_name, project_id);
    let run_dir = storage
        .find_run_dir(&query.node_name, run_id, query.execution_dir.as_deref())
        .map_err(|e| CommandError::io(format!("Failed to find run directory: {}", e)))?;
    let data = read_artifact_bytes(&run_dir, &query.artifact_path)?;
    Ok(base64::engine::general_purpose::STANDARD.encode(&data))
}

fn read_artifact_bytes(run_dir: &Path, artifact_path: &str) -> Result<Vec<u8>, CommandError> {
    let artifacts_dir = run_dir.join("artifacts");
    let artifacts_dir = std::fs::canonicalize(&artifacts_dir)
        .map_err(|e| CommandError::io(format!("Failed to resolve artifacts directory: {}", e)))?;

    let requested = PathBuf::from(artifact_path);
    let requested = if requested.is_absolute() {
        requested
    } else {
        artifacts_dir.join(requested)
    };
    let requested = std::fs::canonicalize(&requested)
        .map_err(|e| CommandError::io(format!("Failed to resolve artifact: {}", e)))?;
    if !requested.starts_with(&artifacts_dir) {
        return Err(CommandError::validation(
            "Artifact path is outside the selected run",
        ));
    }

    std::fs::read(&requested)
        .map_err(|e| CommandError::io(format!("Failed to read artifact: {}", e)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_artifact_bytes_accepts_files_inside_run_artifacts_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let run_dir = tmp.path().join("run");
        let artifacts_dir = run_dir.join("artifacts");
        std::fs::create_dir_all(&artifacts_dir).unwrap();
        let artifact = artifacts_dir.join("shot.png");
        std::fs::write(&artifact, b"image-bytes").unwrap();

        let data = read_artifact_bytes(&run_dir, artifact.to_str().unwrap()).unwrap();

        assert_eq!(data, b"image-bytes");
    }

    #[test]
    fn read_artifact_bytes_rejects_paths_outside_run_artifacts_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let run_dir = tmp.path().join("run");
        let artifacts_dir = run_dir.join("artifacts");
        std::fs::create_dir_all(&artifacts_dir).unwrap();
        let secret = tmp.path().join("secret.txt");
        std::fs::write(&secret, b"secret").unwrap();

        let err = read_artifact_bytes(&run_dir, secret.to_str().unwrap()).unwrap_err();

        assert!(matches!(
            err.kind,
            super::super::error::ErrorKind::Validation
        ));
        assert!(err.message.contains("outside the selected run"));
    }
}
