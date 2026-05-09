use super::error::CommandError;
use super::types::*;
use clickweave_core::ProjectManifest;
use clickweave_core::permissions::CONFIRMABLE_TOOLS;
use clickweave_engine::agent::skills::move_skills_to_project;
use std::path::{Path, PathBuf};
use tauri::Manager;
use tauri_plugin_dialog::DialogExt;
use uuid::Uuid;

#[tauri::command]
#[specta::specta]
pub fn ping() -> String {
    "pong".to_string()
}

/// Returns Ok(path) if the MCP sidecar was found at startup, or Err(reason) if not.
#[tauri::command]
#[specta::specta]
pub fn get_mcp_status(app: tauri::AppHandle) -> Result<String, String> {
    let status = app.state::<McpStatus>();
    status.0.clone()
}

#[tauri::command]
#[specta::specta]
pub async fn pick_workflow_file(app: tauri::AppHandle) -> Result<Option<String>, CommandError> {
    let file = app
        .dialog()
        .file()
        .add_filter("Clickweave Workflow", &["json"])
        .blocking_pick_file();
    Ok(file.map(|p| p.to_string()))
}

#[tauri::command]
#[specta::specta]
pub async fn pick_save_file(app: tauri::AppHandle) -> Result<Option<String>, CommandError> {
    let file = app
        .dialog()
        .file()
        .add_filter("Clickweave Workflow", &["json"])
        .set_file_name("workflow.json")
        .blocking_save_file();
    Ok(file.map(|p| p.to_string()))
}

/// Read the slim [`ProjectManifest`] from `path`.
///
/// Pre-1.0 (D33): legacy `Workflow`-shaped envelopes are **not**
/// auto-migrated. If the JSON parses but contains the legacy graph
/// keys (`nodes`, `edges`), the loader returns a typed validation
/// error so the UI can surface a "start a new project" hint without
/// corrupting the file by overwriting it on the next save.
#[tauri::command]
#[specta::specta]
pub fn open_project(path: String) -> Result<ProjectData, CommandError> {
    let file_path = PathBuf::from(&path);

    if !file_path.exists() {
        return Err(CommandError::io(format!("File not found: {}", path)));
    }

    let content = std::fs::read_to_string(&file_path)
        .map_err(|e| CommandError::io(format!("Failed to read file: {}", e)))?;

    match serde_json::from_str::<ProjectManifest>(&content) {
        Ok(manifest) => Ok(ProjectData { path, manifest }),
        Err(parse_err) => {
            // Detect the legacy Workflow JSON shape so the user gets a
            // typed error instead of a generic parse failure. Either
            // `nodes` or `edges` at the top level is enough to flag the
            // legacy shape — both keys are absent from `ProjectManifest`.
            if let Ok(value) = serde_json::from_str::<serde_json::Value>(&content)
                && let Some(obj) = value.as_object()
                && (obj.contains_key("nodes") || obj.contains_key("edges"))
            {
                return Err(CommandError::validation(
                    "Legacy workflow project files are no longer supported; \
                     start a new project to migrate.",
                ));
            }
            Err(CommandError::validation(format!(
                "Failed to parse project manifest: {}",
                parse_err
            )))
        }
    }
}

#[tauri::command]
#[specta::specta]
pub fn save_project(
    app: tauri::AppHandle,
    path: String,
    manifest: ProjectManifest,
) -> Result<(), CommandError> {
    let app_data = app.state::<AppDataDir>().0.clone();
    save_project_with_app_data(&app_data, path, manifest)
}

fn save_project_with_app_data(
    app_data_dir: &Path,
    path: String,
    manifest: ProjectManifest,
) -> Result<(), CommandError> {
    let file_path = PathBuf::from(&path);
    if let Some(parent) = file_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| CommandError::io(format!("Failed to create directory: {}", e)))?;
    }

    let content = serde_json::to_string_pretty(&manifest)
        .map_err(|e| CommandError::internal(format!("Failed to serialize manifest: {}", e)))?;

    // Atomic write: stage to `<path>.tmp`, then rename over `<path>`.
    // A crash mid-write leaves the prior `<path>` intact.
    let tmp_path = file_path.with_extension(match file_path.extension() {
        Some(ext) => format!("{}.tmp", ext.to_string_lossy()),
        None => "tmp".to_string(),
    });
    std::fs::write(&tmp_path, content)
        .map_err(|e| CommandError::io(format!("Failed to write file: {}", e)))?;
    std::fs::rename(&tmp_path, &file_path)
        .map_err(|e| CommandError::io(format!("Failed to commit file: {}", e)))?;

    let unsaved_skills_root = app_data_dir.join("skills");
    let saved_project_dir = project_dir(&path);
    move_skills_to_project(
        &unsaved_skills_root,
        &manifest.id.to_string(),
        &saved_project_dir,
    )
    .map_err(|e| CommandError::io(format!("Failed to move skills to project: {e}")))?;

    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn confirmable_tools() -> Vec<ConfirmableTool> {
    CONFIRMABLE_TOOLS
        .iter()
        .map(|(name, description)| ConfirmableTool { name, description })
        .collect()
}

#[tauri::command]
#[specta::specta]
pub async fn import_asset(
    app: tauri::AppHandle,
    project_path: String,
) -> Result<Option<ImportedAsset>, CommandError> {
    let file = app
        .dialog()
        .file()
        .add_filter("Images", &["png", "jpg", "jpeg", "webp", "bmp"])
        .blocking_pick_file();

    let source = match file {
        Some(f) => PathBuf::from(f.to_string()),
        None => return Ok(None),
    };

    let ext = source.extension().and_then(|e| e.to_str()).unwrap_or("png");
    let filename = format!("{}.{}", Uuid::new_v4(), ext);

    let assets_dir = project_dir(&project_path).join("assets");
    std::fs::create_dir_all(&assets_dir)
        .map_err(|e| CommandError::io(format!("Failed to create assets directory: {}", e)))?;

    let dest = assets_dir.join(&filename);
    std::fs::copy(&source, &dest)
        .map_err(|e| CommandError::io(format!("Failed to copy asset: {}", e)))?;

    let relative_path = format!("assets/{}", filename);
    let absolute_path = dest
        .to_str()
        .ok_or(CommandError::internal("Invalid path"))?
        .to_string();

    Ok(Some(ImportedAsset {
        relative_path,
        absolute_path,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_project_moves_unsaved_skills_to_saved_project_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let app_data = tmp.path().join("app-data");
        let manifest = ProjectManifest {
            id: Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap(),
            name: "Saved Workflow".to_string(),
            ..ProjectManifest::default()
        };

        let unsaved_dir = app_data.join("skills").join(manifest.id.to_string());
        std::fs::create_dir_all(&unsaved_dir).unwrap();
        std::fs::write(unsaved_dir.join("alpha-v1.md"), b"alpha").unwrap();
        std::fs::write(unsaved_dir.join("alpha-v1.proposal.json"), b"{}").unwrap();

        let project_path = tmp.path().join("saved").join("workflow.json");
        save_project_with_app_data(
            &app_data,
            project_path.to_string_lossy().into_owned(),
            manifest,
        )
        .unwrap();

        assert!(project_path.exists());
        assert!(!unsaved_dir.exists());
        assert_eq!(
            std::fs::read(tmp.path().join("saved/.clickweave/skills/alpha-v1.md")).unwrap(),
            b"alpha"
        );
        assert_eq!(
            std::fs::read(
                tmp.path()
                    .join("saved/.clickweave/skills/alpha-v1.proposal.json")
            )
            .unwrap(),
            b"{}"
        );
    }

    #[test]
    fn open_project_round_trips_a_freshly_saved_manifest() {
        let tmp = tempfile::tempdir().unwrap();
        let app_data = tmp.path().join("app-data");
        let manifest = ProjectManifest {
            id: Uuid::parse_str("11111111-2222-3333-4444-555555555555").unwrap(),
            name: "Round Trip".to_string(),
            intent: Some("test intent".to_string()),
            ..ProjectManifest::default()
        };
        let project_path = tmp.path().join("rt").join("project.json");
        save_project_with_app_data(
            &app_data,
            project_path.to_string_lossy().into_owned(),
            manifest.clone(),
        )
        .unwrap();

        let loaded = open_project(project_path.to_string_lossy().into_owned()).unwrap();
        assert_eq!(loaded.manifest, manifest);
    }

    #[test]
    fn open_project_rejects_legacy_workflow_envelope() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("legacy.json");
        // Synthesize the legacy `Workflow` JSON shape: top-level
        // `nodes`/`edges` keys without a `schema_version`. The loader
        // must not silently accept this.
        let legacy = serde_json::json!({
            "id": "00000000-0000-0000-0000-000000000000",
            "name": "Legacy",
            "nodes": [],
            "edges": [],
        });
        std::fs::write(&path, serde_json::to_string(&legacy).unwrap()).unwrap();

        let err = open_project(path.to_string_lossy().into_owned()).unwrap_err();
        assert!(err.message.to_lowercase().contains("legacy"));
    }
}
