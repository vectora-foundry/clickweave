use super::error::CommandError;
use super::types::*;
use clickweave_core::{SkillRun, TraceEvent};
use std::path::{Path, PathBuf};
use tracing::warn;

/// List historical runs for a skill (D27).
///
/// When `query.run_id` is `Some`, the result contains at most a single
/// matching record (or empty when the run cannot be found). When
/// `None`, every persisted run for the skill is returned, sorted
/// oldest-first by `started_at`. The legacy node-keyed result shape
/// no longer exists — every run is keyed on `skill_id` per D28.
#[tauri::command]
#[specta::specta]
pub fn list_runs(app: tauri::AppHandle, query: RunsQuery) -> Result<Vec<SkillRun>, CommandError> {
    let project_id = parse_uuid(&query.project_id, "project")?;
    let storage = resolve_storage(&app, &query.project_path, &query.project_name, project_id);

    if let Some(run_id_str) = query.run_id.as_deref() {
        let run_id = parse_uuid(run_id_str, "run")?;
        let run = storage
            .find_skill_run(&query.skill_id, run_id)
            .map_err(|e| CommandError::io(format!("Failed to load skill run: {e}")))?;
        return Ok(run.into_iter().collect());
    }

    storage
        .load_runs_for_skill(&query.skill_id)
        .map_err(|e| CommandError::io(format!("Failed to load runs: {e}")))
}

/// Load the trace event log for a single skill run (D28).
#[tauri::command]
#[specta::specta]
pub fn load_run_events(
    app: tauri::AppHandle,
    query: RunEventsQuery,
) -> Result<Vec<TraceEvent>, CommandError> {
    let project_id = parse_uuid(&query.project_id, "project")?;
    let run_id = parse_uuid(&query.run_id, "run")?;

    let storage = resolve_storage(&app, &query.project_path, &query.project_name, project_id);
    // The run record must exist so the events.jsonl path is anchored
    // on a known run identity. Missing record = empty event list.
    if storage
        .find_skill_run(&query.skill_id, run_id)
        .map_err(|e| CommandError::io(format!("Failed to look up skill run: {e}")))?
        .is_none()
    {
        return Ok(Vec::new());
    }
    let events_path = storage
        .skill_run_events_dir(&query.skill_id, run_id)
        .join("events.jsonl");

    if !events_path.exists() {
        return Ok(Vec::new());
    }

    let content = std::fs::read_to_string(&events_path)
        .map_err(|e| CommandError::io(format!("Failed to read events.jsonl: {e}")))?;

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

/// Read a base64-encoded artifact file from a skill run's per-run
/// directory.
///
/// The artifact path is sandboxed under `<run_id>/artifacts/` so a
/// caller cannot escape outside the directory tree even with absolute
/// or `..`-prefixed inputs. The shape of this command is unchanged
/// from the legacy node-keyed surface — only the run-locator fields
/// (`skill_id` + `run_id`) are skill-keyed.
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
    let run_dir = storage.skill_run_events_dir(&query.skill_id, run_id);
    if !run_dir.exists() {
        return Err(CommandError::validation(format!(
            "Run directory not found for skill {} run {run_id}",
            query.skill_id
        )));
    }
    let data = read_artifact_bytes(&run_dir, &query.artifact_path)?;
    Ok(base64::engine::general_purpose::STANDARD.encode(&data))
}

fn read_artifact_bytes(run_dir: &Path, artifact_path: &str) -> Result<Vec<u8>, CommandError> {
    let artifacts_dir = run_dir.join("artifacts");
    let artifacts_dir = std::fs::canonicalize(&artifacts_dir)
        .map_err(|e| CommandError::io(format!("Failed to resolve artifacts directory: {e}")))?;

    let requested = PathBuf::from(artifact_path);
    let requested = if requested.is_absolute() {
        requested
    } else {
        artifacts_dir.join(requested)
    };
    let requested = std::fs::canonicalize(&requested)
        .map_err(|e| CommandError::io(format!("Failed to resolve artifact: {e}")))?;
    if !requested.starts_with(&artifacts_dir) {
        return Err(CommandError::validation(
            "Artifact path is outside the selected run",
        ));
    }

    std::fs::read(&requested).map_err(|e| CommandError::io(format!("Failed to read artifact: {e}")))
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
