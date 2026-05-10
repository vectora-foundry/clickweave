//! Commands that back the conversational-agent surface:
//! - `load_agent_chat` / `save_agent_chat` — per-workflow transcript
//! - `prune_skill_lineage_for_nodes` — selective draft-skill lineage pruning on node delete
//! - `clear_agent_conversation` — one-click wipe of draft skills + variant index + transcript
//!
//! All file mutations are gated on the privacy kill switch: when
//! `store_traces` is false, on-disk writes are skipped.

use super::error::CommandError;
use super::types::resolve_storage;
use clickweave_engine::agent::skills::{SkillState, SkillStore};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::Path;
use uuid::Uuid;

/// Persisted transcript — a sibling file to the workflow run metadata.
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
    pub project_name: String,
    pub project_id: String,
}

#[derive(Debug, Clone, Deserialize, specta::Type)]
pub struct SaveAgentChatRequest {
    pub project_path: Option<String>,
    pub project_name: String,
    pub project_id: String,
    pub chat: AgentChat,
    pub store_traces: bool,
}

#[derive(Debug, Clone, Deserialize, specta::Type)]
pub struct PruneSkillLineageRequest {
    pub project_path: Option<String>,
    pub project_name: String,
    pub project_id: String,
    pub node_ids: Vec<Uuid>,
    pub store_traces: bool,
}

#[derive(Debug, Clone, Deserialize, specta::Type)]
pub struct ClearAgentConversationRequest {
    pub project_path: Option<String>,
    pub project_name: String,
    pub project_id: String,
    pub store_traces: bool,
}

fn proposal_path(dir: &std::path::Path, skill_id: &str) -> std::path::PathBuf {
    dir.join(skill_id).join("proposal.json")
}

fn remove_proposal_if_present(
    dir: &std::path::Path,
    skill_id: &str,
    _version: u32,
) -> Result<(), CommandError> {
    let path = proposal_path(dir, skill_id);
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(CommandError::io(format!("remove skill proposal: {e}"))),
    }
}

fn prune_skill_lineage_in_dir(
    skills_dir: &Path,
    deleted: &HashSet<Uuid>,
) -> Result<(), CommandError> {
    let store = SkillStore::new(skills_dir.to_path_buf());
    for path in store
        .list_files()
        .map_err(|e| CommandError::io(format!("list skill files: {e}")))?
    {
        let mut skill = store
            .read_skill(&path)
            .map_err(|e| CommandError::io(format!("read skill {}: {e}", path.display())))?;
        if skill.state != SkillState::Draft {
            continue;
        }

        let before = skill.produced_node_ids.len();
        skill.produced_node_ids.retain(|id| !deleted.contains(id));
        if before == skill.produced_node_ids.len() {
            continue;
        }

        if skill.produced_node_ids.is_empty() {
            store
                .delete_skill(&path)
                .map_err(|e| CommandError::io(format!("delete empty draft skill: {e}")))?;
            remove_proposal_if_present(skills_dir, &skill.id, skill.version)?;
        } else {
            store
                .write_skill(&skill)
                .map_err(|e| CommandError::io(format!("persist pruned skill lineage: {e}")))?;
        }
    }
    Ok(())
}

fn clear_draft_skills_in_dir(skills_dir: &Path) -> Result<(), CommandError> {
    let store = SkillStore::new(skills_dir.to_path_buf());
    for path in store
        .list_files()
        .map_err(|e| CommandError::io(format!("list skill files: {e}")))?
    {
        let skill = store
            .read_skill(&path)
            .map_err(|e| CommandError::io(format!("read skill {}: {e}", path.display())))?;
        if skill.state == SkillState::Draft {
            store
                .delete_skill(&path)
                .map_err(|e| CommandError::io(format!("delete draft skill: {e}")))?;
            remove_proposal_if_present(skills_dir, &skill.id, skill.version)?;
        }
    }
    Ok(())
}

/// Resolve the `agent_chat.json` path for the current project.
fn resolve_chat_path(
    app: &tauri::AppHandle,
    project_path: Option<&str>,
    project_name: &str,
    project_id: &str,
) -> Result<std::path::PathBuf, CommandError> {
    let uuid: Uuid = project_id
        .parse()
        .map_err(|_| CommandError::validation("Invalid project ID"))?;
    let storage = resolve_storage(app, &project_path.map(String::from), project_name, uuid);
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
        &request.project_name,
        &request.project_id,
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
    // std::fs:: call so the privacy contract is respected.
    if !request.store_traces {
        return Ok(());
    }
    let path = resolve_chat_path(
        &app,
        request.project_path.as_deref(),
        &request.project_name,
        &request.project_id,
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
pub async fn prune_skill_lineage_for_nodes(
    app: tauri::AppHandle,
    request: PruneSkillLineageRequest,
) -> Result<(), CommandError> {
    // Privacy kill switch: don't touch the skill files. Must run before any
    // std::fs:: call.
    if !request.store_traces {
        return Ok(());
    }
    let project_uuid: Uuid = request
        .project_id
        .parse()
        .map_err(|_| CommandError::validation("Invalid project ID"))?;
    let storage = resolve_storage(
        &app,
        &request.project_path,
        &request.project_name,
        project_uuid,
    );
    let skills_dir = storage
        .project_skills_dir()
        .map_err(|e| CommandError::io(format!("resolve project skills dir: {e}")))?;
    let deleted: HashSet<Uuid> = request.node_ids.into_iter().collect();
    prune_skill_lineage_in_dir(&skills_dir, &deleted)
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
    let project_uuid: Uuid = request
        .project_id
        .parse()
        .map_err(|_| CommandError::validation("Invalid project ID"))?;
    let storage = resolve_storage(
        &app,
        &request.project_path,
        &request.project_name,
        project_uuid,
    );
    // Remove draft skills derived from the current agent conversation.
    let skills_dir = storage
        .project_skills_dir()
        .map_err(|e| CommandError::io(format!("resolve project skills dir: {e}")))?;
    clear_draft_skills_in_dir(&skills_dir)?;

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
    use clickweave_engine::agent::skills::{
        ApplicabilityHints, ApplicabilitySignature, OutcomePredicate, Skill, SkillScope,
        SkillStats, SubgoalSignature,
    };

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
    fn prune_skill_lineage_updates_drafts_and_deletes_empty_ones() {
        let tmp = tempfile::tempdir().unwrap();
        let store = SkillStore::new(tmp.path().to_path_buf());
        let deleted = Uuid::from_u128(10);
        let kept = Uuid::from_u128(11);

        let partial_path = store
            .write_skill(&sample_skill(
                "partial-draft",
                SkillState::Draft,
                vec![deleted, kept],
            ))
            .unwrap();
        let empty_path = store
            .write_skill(&sample_skill(
                "empty-draft",
                SkillState::Draft,
                vec![deleted],
            ))
            .unwrap();
        let confirmed_path = store
            .write_skill(&sample_skill(
                "confirmed",
                SkillState::Confirmed,
                vec![deleted],
            ))
            .unwrap();
        std::fs::create_dir_all(tmp.path().join("empty-draft")).unwrap();
        std::fs::write(proposal_path(tmp.path(), "empty-draft"), "{}").unwrap();

        prune_skill_lineage_in_dir(tmp.path(), &HashSet::from([deleted])).unwrap();

        let partial = store.read_skill(&partial_path).unwrap();
        assert_eq!(partial.produced_node_ids, vec![kept]);
        assert!(!empty_path.exists());
        assert!(!proposal_path(tmp.path(), "empty-draft").exists());
        let confirmed = store.read_skill(&confirmed_path).unwrap();
        assert_eq!(confirmed.produced_node_ids, vec![deleted]);
    }

    #[test]
    fn clear_draft_skills_removes_only_drafts_and_their_proposals() {
        let tmp = tempfile::tempdir().unwrap();
        let store = SkillStore::new(tmp.path().to_path_buf());

        let draft_path = store
            .write_skill(&sample_skill(
                "draft",
                SkillState::Draft,
                vec![Uuid::from_u128(20)],
            ))
            .unwrap();
        let confirmed_path = store
            .write_skill(&sample_skill(
                "confirmed",
                SkillState::Confirmed,
                vec![Uuid::from_u128(21)],
            ))
            .unwrap();
        std::fs::create_dir_all(tmp.path().join("draft")).unwrap();
        std::fs::create_dir_all(tmp.path().join("confirmed")).unwrap();
        std::fs::write(proposal_path(tmp.path(), "draft"), "{}").unwrap();
        std::fs::write(proposal_path(tmp.path(), "confirmed"), "{}").unwrap();

        clear_draft_skills_in_dir(tmp.path()).unwrap();

        assert!(!draft_path.exists());
        assert!(!proposal_path(tmp.path(), "draft").exists());
        assert!(confirmed_path.exists());
        assert!(proposal_path(tmp.path(), "confirmed").exists());
    }

    #[test]
    fn privacy_kill_switch_branches_return_before_file_io() {
        // The guard is `if !request.store_traces { return Ok(()); }`.
        // A change that moved file I/O above this guard would break the
        // privacy contract. Keep this contract documented as a test —
        // scan the function body for the first real std::fs:: call
        // (skipping comments) and assert the guard precedes it.
        let src = include_str!("agent_chat.rs");
        for fn_name in [
            "pub async fn prune_skill_lineage_for_nodes",
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

    fn sample_skill(id: &str, state: SkillState, produced_node_ids: Vec<Uuid>) -> Skill {
        Skill {
            id: id.into(),
            version: 1,
            state,
            scope: SkillScope::ProjectLocal,
            name: id.into(),
            description: "desc".into(),
            tags: vec![],
            subgoal_text: "subgoal".into(),
            subgoal_signature: SubgoalSignature("sig".into()),
            applicability: ApplicabilityHints {
                apps: vec![],
                hosts: vec![],
                signature: ApplicabilitySignature("appsig".into()),
            },
            parameter_schema: vec![],
            action_sketch: vec![],
            outputs: vec![],
            outcome_predicate: OutcomePredicate::SubgoalCompleted {
                post_state_world_model_signature: None,
            },
            provenance: vec![],
            stats: SkillStats::default(),
            edited_by_user: false,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            produced_node_ids,
            body: String::new(),
            schema_version: clickweave_engine::agent::skills::SKILL_SCHEMA_VERSION,
            variables: vec![],
            sections: vec![],
            replay: None,
        }
    }
}
