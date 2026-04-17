//! Commands that back the conversational-agent surface:
//! - `load_agent_chat` / `save_agent_chat` — per-workflow transcript
//! - `prune_agent_cache_for_nodes` — selective eviction on node delete
//! - `clear_agent_conversation` — one-click wipe of cache + variant index + transcript
//!
//! All file mutations are gated on the privacy kill switch: when
//! `store_traces` is false, on-disk writes are skipped. In-memory
//! eviction is handled by the engine on the next run when it reloads
//! the on-disk cache — no in-memory handle is reachable from these
//! commands because the agent is idle by contract while they run.

use super::error::CommandError;
use super::types::resolve_storage;
use clickweave_engine::agent::AgentCache;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Persisted transcript — a sibling file to `agent_cache.json`.
/// Kept deliberately minimal (no schema version) until the format
/// changes; versioning is added lazily when it matters.
#[derive(Debug, Clone, Serialize, Deserialize, Default, specta::Type)]
pub struct AgentChat {
    pub messages: Vec<AgentChatMessage>,
}

#[derive(Debug, Clone, Serialize, Deserialize, specta::Type)]
pub struct AgentChatMessage {
    pub role: AgentChatRole,
    pub content: String,
    pub timestamp: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<Uuid>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, specta::Type, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum AgentChatRole {
    User,
    Assistant,
    System,
}

#[derive(Debug, Clone, Deserialize, specta::Type)]
pub struct LoadAgentChatRequest {
    pub project_path: Option<String>,
    pub workflow_name: String,
    pub workflow_id: String,
}

#[derive(Debug, Clone, Deserialize, specta::Type)]
pub struct SaveAgentChatRequest {
    pub project_path: Option<String>,
    pub workflow_name: String,
    pub workflow_id: String,
    pub chat: AgentChat,
    pub store_traces: bool,
}

#[derive(Debug, Clone, Deserialize, specta::Type)]
pub struct PruneAgentCacheRequest {
    pub project_path: Option<String>,
    pub workflow_name: String,
    pub workflow_id: String,
    pub node_ids: Vec<Uuid>,
    pub store_traces: bool,
}

#[derive(Debug, Clone, Deserialize, specta::Type)]
pub struct ClearAgentConversationRequest {
    pub project_path: Option<String>,
    pub workflow_name: String,
    pub workflow_id: String,
    pub store_traces: bool,
}

/// Resolve the `agent_chat.json` path for the current project + workflow.
fn resolve_chat_path(
    app: &tauri::AppHandle,
    project_path: Option<&str>,
    workflow_name: &str,
    workflow_id: &str,
) -> Result<std::path::PathBuf, CommandError> {
    let uuid: Uuid = workflow_id
        .parse()
        .map_err(|_| CommandError::validation("Invalid workflow ID"))?;
    let storage = resolve_storage(app, &project_path.map(String::from), workflow_name, uuid);
    Ok(storage.agent_chat_path())
}

#[tauri::command]
#[specta::specta]
pub async fn load_agent_chat(
    app: tauri::AppHandle,
    request: LoadAgentChatRequest,
) -> Result<AgentChat, CommandError> {
    let path = resolve_chat_path(
        &app,
        request.project_path.as_deref(),
        &request.workflow_name,
        &request.workflow_id,
    )?;
    match std::fs::read_to_string(&path) {
        Ok(json) => serde_json::from_str::<AgentChat>(&json)
            .map_err(|e| CommandError::validation(format!("agent_chat.json malformed: {e}"))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(AgentChat::default()),
        Err(e) => Err(CommandError::io(format!("read agent_chat.json: {e}"))),
    }
}

#[tauri::command]
#[specta::specta]
pub async fn save_agent_chat(
    app: tauri::AppHandle,
    request: SaveAgentChatRequest,
) -> Result<(), CommandError> {
    // Privacy kill switch: skip file write entirely. Must run before any
    // std::fs:: call so the D1.M4 contract is respected.
    if !request.store_traces {
        return Ok(());
    }
    let path = resolve_chat_path(
        &app,
        request.project_path.as_deref(),
        &request.workflow_name,
        &request.workflow_id,
    )?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| CommandError::io(format!("mkdir agent_chat parent: {e}")))?;
    }
    let json = serde_json::to_string_pretty(&request.chat)
        .map_err(|e| CommandError::validation(format!("serialize agent_chat: {e}")))?;
    std::fs::write(&path, json)
        .map_err(|e| CommandError::io(format!("write agent_chat.json: {e}")))?;
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub async fn prune_agent_cache_for_nodes(
    app: tauri::AppHandle,
    request: PruneAgentCacheRequest,
) -> Result<(), CommandError> {
    // Privacy kill switch: D1.M4 says don't touch the file. The next run
    // still picks up the unmutated cache and applies `evict_for_node`
    // before use. Must run before any std::fs:: call.
    if !request.store_traces {
        return Ok(());
    }
    let workflow_uuid: Uuid = request
        .workflow_id
        .parse()
        .map_err(|_| CommandError::validation("Invalid workflow ID"))?;
    let storage = resolve_storage(
        &app,
        &request.project_path,
        &request.workflow_name,
        workflow_uuid,
    );
    let cache_path = storage.agent_cache_path();
    let mut cache = AgentCache::load_from_path(&cache_path);
    for node_id in &request.node_ids {
        cache.evict_for_node(*node_id);
    }
    cache
        .save_to_path(&cache_path)
        .map_err(|e| CommandError::io(format!("persist pruned cache: {e}")))?;
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub async fn clear_agent_conversation(
    app: tauri::AppHandle,
    request: ClearAgentConversationRequest,
) -> Result<(), CommandError> {
    // Privacy kill switch: no file mutation. Must run before any std::fs:: call.
    if !request.store_traces {
        return Ok(());
    }
    let workflow_uuid: Uuid = request
        .workflow_id
        .parse()
        .map_err(|_| CommandError::validation("Invalid workflow ID"))?;
    let storage = resolve_storage(
        &app,
        &request.project_path,
        &request.workflow_name,
        workflow_uuid,
    );
    // Truncate agent_cache.json to an empty object.
    let cache_path = storage.agent_cache_path();
    if cache_path.exists() {
        std::fs::write(&cache_path, "{}")
            .map_err(|e| CommandError::io(format!("truncate agent_cache.json: {e}")))?;
    }
    // Truncate variant_index.jsonl to empty. `VariantIndex::load_existing`
    // will read an empty file as "no prior runs" on the next run.
    let variant_path = storage.variant_index_path();
    if variant_path.exists() {
        std::fs::write(&variant_path, "")
            .map_err(|e| CommandError::io(format!("truncate variant_index.jsonl: {e}")))?;
    }
    // Remove agent_chat.json.
    let chat_path = storage.agent_chat_path();
    if chat_path.exists() {
        std::fs::remove_file(&chat_path)
            .map_err(|e| CommandError::io(format!("remove agent_chat.json: {e}")))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chat_serialize_round_trip_preserves_roles_and_run_ids() {
        let chat = AgentChat {
            messages: vec![
                AgentChatMessage {
                    role: AgentChatRole::User,
                    content: "goal one".into(),
                    timestamp: "2026-04-17T00:00:00Z".into(),
                    run_id: Some(Uuid::from_u128(1)),
                },
                AgentChatMessage {
                    role: AgentChatRole::Assistant,
                    content: "done".into(),
                    timestamp: "2026-04-17T00:00:10Z".into(),
                    run_id: Some(Uuid::from_u128(1)),
                },
                AgentChatMessage {
                    role: AgentChatRole::System,
                    content: "Deleted 1 node".into(),
                    timestamp: "2026-04-17T00:01:00Z".into(),
                    run_id: None,
                },
            ],
        };

        let json = serde_json::to_string(&chat).unwrap();
        let back: AgentChat = serde_json::from_str(&json).unwrap();
        assert_eq!(back.messages.len(), 3);
        assert_eq!(back.messages[2].role, AgentChatRole::System);
        assert_eq!(back.messages[0].run_id, Some(Uuid::from_u128(1)));
    }

    #[test]
    fn legacy_chat_without_run_id_loads_as_none() {
        let json = r#"{
            "messages": [
                { "role": "user", "content": "hi", "timestamp": "t1" }
            ]
        }"#;
        let chat: AgentChat = serde_json::from_str(json).unwrap();
        assert_eq!(chat.messages[0].run_id, None);
    }

    #[test]
    fn privacy_kill_switch_branches_return_before_file_io() {
        // The guard is `if !request.store_traces { return Ok(()); }`.
        // A change that moved file I/O above this guard would break the
        // spec's D1.M4 resolution. Keep this contract documented as a
        // test — scan the function body for the first real std::fs::
        // call (skipping comments) and assert the guard precedes it.
        let src = include_str!("agent_chat.rs");
        for fn_name in [
            "pub async fn prune_agent_cache_for_nodes",
            "pub async fn clear_agent_conversation",
            "pub async fn save_agent_chat",
        ] {
            let body = src.split(fn_name).nth(1).expect("fn present");
            let guard_pos = body
                .find("if !request.store_traces")
                .expect("kill-switch guard present");
            // Find the first `std::fs::` that is NOT inside a `//` comment.
            // We scan line-by-line so comment occurrences (e.g. `// ... std::fs:: ...`)
            // don't misfire.
            let mut mutation_pos: usize = usize::MAX;
            let mut offset = 0usize;
            for line in body.lines() {
                let trimmed = line.trim_start();
                if let Some(rel) = line.find("std::fs::")
                    && !trimmed.starts_with("//")
                {
                    mutation_pos = offset + rel;
                    break;
                }
                offset += line.len() + 1; // +1 for '\n'
            }
            assert!(
                guard_pos < mutation_pos,
                "privacy-flag guard must execute before any std::fs:: mutation in {}",
                fn_name,
            );
        }
    }
}
