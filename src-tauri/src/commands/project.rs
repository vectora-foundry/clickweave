use super::error::CommandError;
use super::types::*;
use clickweave_core::{NodeType, Workflow, validate_workflow};
use clickweave_llm::planner::conversation::ConversationSession;
use std::path::PathBuf;
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

#[tauri::command]
#[specta::specta]
pub fn open_project(path: String) -> Result<ProjectData, CommandError> {
    let file_path = PathBuf::from(&path);

    if !file_path.exists() {
        return Err(CommandError::io(format!("File not found: {}", path)));
    }

    let content = std::fs::read_to_string(&file_path)
        .map_err(|e| CommandError::io(format!("Failed to read file: {}", e)))?;

    let mut workflow: Workflow = serde_json::from_str(&content)
        .map_err(|e| CommandError::validation(format!("Failed to parse workflow: {}", e)))?;

    workflow.fixup_auto_ids();

    Ok(ProjectData { path, workflow })
}

#[tauri::command]
#[specta::specta]
pub fn save_project(path: String, workflow: Workflow) -> Result<(), CommandError> {
    let file_path = PathBuf::from(path);

    if let Some(parent) = file_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| CommandError::io(format!("Failed to create directory: {}", e)))?;
    }

    let content = serde_json::to_string_pretty(&workflow)
        .map_err(|e| CommandError::internal(format!("Failed to serialize workflow: {}", e)))?;

    std::fs::write(&file_path, content)
        .map_err(|e| CommandError::io(format!("Failed to write file: {}", e)))?;

    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn validate(workflow: Workflow) -> ValidationResult {
    match validate_workflow(&workflow) {
        Ok(_) => ValidationResult {
            valid: true,
            errors: vec![],
        },
        Err(e) => ValidationResult {
            valid: false,
            errors: vec![e.to_string()],
        },
    }
}

#[tauri::command]
#[specta::specta]
pub fn node_type_defaults() -> Vec<NodeTypeInfo> {
    NodeType::all_defaults()
        .into_iter()
        .map(|nt| NodeTypeInfo {
            name: nt.display_name(),
            output_role: format!("{:?}", nt.output_role()),
            node_context: format!("{:?}", nt.node_context()),
            icon: nt.icon(),
            node_type: nt,
        })
        .collect()
}

#[tauri::command]
#[specta::specta]
pub fn generate_auto_id(
    node_type_name: String,
    counters_json: String,
) -> Result<(String, String), String> {
    let mut counters: std::collections::HashMap<String, u32> =
        serde_json::from_str(&counters_json).map_err(|e| e.to_string())?;

    let node_type = NodeType::default_for_name(&node_type_name)
        .ok_or_else(|| format!("Unknown node type: {}", node_type_name))?;

    let auto_id = clickweave_core::auto_id::assign_auto_id(&node_type, &mut counters);

    let updated_counters = serde_json::to_string(&counters).map_err(|e| e.to_string())?;

    Ok((auto_id, updated_counters))
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

#[tauri::command]
#[specta::specta]
pub fn save_conversation(
    path: String,
    conversation: ConversationSession,
) -> Result<(), CommandError> {
    let dir = project_dir(&path);
    let conv_path = dir.join("conversation.json");

    let content = serde_json::to_string_pretty(&conversation)
        .map_err(|e| CommandError::internal(format!("Failed to serialize conversation: {}", e)))?;

    std::fs::write(&conv_path, content)
        .map_err(|e| CommandError::io(format!("Failed to write conversation: {}", e)))?;

    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn load_conversation(path: String) -> Result<Option<ConversationSession>, CommandError> {
    let dir = project_dir(&path);
    let conv_path = dir.join("conversation.json");

    if !conv_path.exists() {
        return Ok(None);
    }

    let content = std::fs::read_to_string(&conv_path)
        .map_err(|e| CommandError::io(format!("Failed to read conversation: {}", e)))?;

    let conversation: ConversationSession = serde_json::from_str(&content)
        .map_err(|e| CommandError::validation(format!("Failed to parse conversation: {}", e)))?;

    Ok(Some(conversation))
}
