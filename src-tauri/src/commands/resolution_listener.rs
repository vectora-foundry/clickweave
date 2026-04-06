use super::error::CommandError;
use super::planner_session::AssistantSessionHandle;
use super::types::*;
use clickweave_core::{RuntimeResolution, WorkflowPatchCompact};
use clickweave_engine::RuntimeQuery;
use clickweave_llm::planner::assistant::resolution_chat_with_backend;
use clickweave_llm::planner::conversation::{ChatRole, PatchSummary};
use clickweave_llm::{LlmClient, Message};
use std::sync::Mutex;
use tauri::{Emitter, Manager};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

/// Managed state holding the oneshot sender for user approval of a resolution
/// and the auto-approve flag snapshotted at run start.
#[derive(Default)]
pub struct ResolutionState {
    pub(crate) response_tx: Option<oneshot::Sender<bool>>,
    pub(crate) auto_approve: bool,
}

/// Spawn the resolution listener task.
///
/// Receives `RuntimeQuery` values from the executor, calls the LLM for a
/// patch, emits events to the frontend, and waits for user approval.
pub fn spawn_listener(
    app: tauri::AppHandle,
    mut resolution_rx: tokio::sync::mpsc::Receiver<RuntimeQuery>,
    cancel_token: CancellationToken,
) -> tauri::async_runtime::JoinHandle<()> {
    tauri::async_runtime::spawn(async move {
        loop {
            let query = tokio::select! {
                _ = cancel_token.cancelled() => break,
                q = resolution_rx.recv() => match q {
                    Some(q) => q,
                    None => break, // executor dropped the sender
                },
            };

            let resolution = handle_query(&app, &query, &cancel_token).await;

            // Send the resolution back to the executor (oneshot).
            // If the receiver was dropped (executor was cancelled) we just discard.
            let _ = query.response_tx.send(resolution);
        }
    })
}

/// Process a single resolution query:
/// 1. Take PlannerSession from AssistantSessionHandle
/// 2. Call the LLM
/// 3. If patch proposed, emit event and wait for user approval
/// 4. Return appropriate RuntimeResolution
async fn handle_query(
    app: &tauri::AppHandle,
    query: &RuntimeQuery,
    cancel_token: &CancellationToken,
) -> RuntimeResolution {
    // --- Retrieve the LLM config and conversation from session handle ---
    let session_handle = app.state::<tokio::sync::Mutex<AssistantSessionHandle>>();

    let (config, conversation, resolution_wf, mcp_tools) = {
        let mut guard = session_handle.lock().await;
        let config = match &guard.assistant_config {
            Some(c) => c.clone(),
            None => {
                warn!("Resolution query but no assistant config available");
                return RuntimeResolution::Rejected;
            }
        };
        let conversation = guard.conversation.clone();
        let wf = guard.resolution_workflow.clone().unwrap_or_default();
        // Take the session briefly to get MCP tools, then return it
        let tools = if let Some(session) = guard.take_session() {
            let tools = session.mcp_tools_openai().await;
            guard.return_session(session);
            tools
        } else {
            Vec::new()
        };
        (config, conversation, wf, tools)
    };

    // Build the query message for the LLM (may include screenshot)
    let query_text = format!(
        "Runtime resolution needed for node \"{}\" (ID: {}).\n\
         Action: {}\n\
         Target: {}\n\
         \n\
         Element inventory:\n\
         {}",
        query.node_name,
        query.node_id,
        query.action_description,
        query.target,
        query.element_inventory,
    );

    let query_message = if let Some(ref screenshot) = query.screenshot {
        Message::user_with_images(
            &query_text,
            vec![(screenshot.clone(), "image/png".to_string())],
        )
    } else {
        Message::user(&query_text)
    };

    // Emit the query as a user message in the assistant conversation
    let session_id = {
        let guard = session_handle.lock().await;
        guard.session_id().unwrap_or("resolution").to_string()
    };

    let _ = app.emit(
        "assistant://message",
        AssistantMessagePayload {
            session_id: session_id.clone(),
            entry: clickweave_llm::planner::conversation::ChatEntry {
                role: ChatRole::User,
                content: query_text.clone(),
                timestamp: now_millis(),
                patch_summary: None,
                run_context: None,
                tool_call_id: None,
                tool_name: None,
            },
        },
    );

    // Use the workflow snapshot stored during run_workflow for the system prompt.
    let workflow = resolution_wf;

    let client = LlmClient::new(config);
    let result =
        resolution_chat_with_backend(&client, &workflow, query_message, &conversation, &mcp_tools)
            .await;

    let assistant_result = match result {
        Ok(r) => r,
        Err(e) => {
            warn!("Resolution LLM call failed: {}", e);
            return RuntimeResolution::Rejected;
        }
    };

    // Emit the assistant response
    let patch_summary = assistant_result.patch.as_ref().map(|p| PatchSummary {
        added: p.added_nodes.len() as u32,
        removed: p.removed_node_ids.len() as u32,
        updated: p.updated_nodes.len() as u32,
        added_names: p.added_nodes.iter().map(|n| n.name.clone()).collect(),
        removed_names: Vec::new(),
        updated_names: p.updated_nodes.iter().map(|n| n.name.clone()).collect(),
        description: None,
    });

    let _ = app.emit(
        "assistant://message",
        AssistantMessagePayload {
            session_id: session_id.clone(),
            entry: clickweave_llm::planner::conversation::ChatEntry {
                role: ChatRole::Assistant,
                content: assistant_result.message.clone(),
                timestamp: now_millis(),
                patch_summary,
                run_context: None,
                tool_call_id: None,
                tool_name: None,
            },
        },
    );

    // Append to backend conversation
    {
        let mut guard = session_handle.lock().await;
        guard.conversation.push_user(query_text, None);
        let ps = assistant_result.patch.as_ref().map(|p| PatchSummary {
            added: p.added_nodes.len() as u32,
            removed: p.removed_node_ids.len() as u32,
            updated: p.updated_nodes.len() as u32,
            added_names: p.added_nodes.iter().map(|n| n.name.clone()).collect(),
            removed_names: Vec::new(),
            updated_names: p.updated_nodes.iter().map(|n| n.name.clone()).collect(),
            description: None,
        });
        guard
            .conversation
            .push_assistant(assistant_result.message.clone(), ps);
    }

    // If there's a patch, propose it to the user
    let Some(patch_result) = assistant_result.patch else {
        info!("Resolution LLM returned no patch, rejecting");
        return RuntimeResolution::Rejected;
    };

    // Build the compact patch for the executor
    let compact = WorkflowPatchCompact {
        added_nodes: patch_result.added_nodes,
        removed_node_ids: patch_result.removed_node_ids,
        updated_nodes: patch_result.updated_nodes,
        added_edges: patch_result.added_edges,
        removed_edges: patch_result.removed_edges,
    };

    // Build the frontend patch for event payloads (computed once, cloned as needed)
    let frontend_patch = WorkflowPatch {
        added_nodes: compact.added_nodes.clone(),
        removed_node_ids: compact
            .removed_node_ids
            .iter()
            .map(|id| id.to_string())
            .collect(),
        updated_nodes: compact.updated_nodes.clone(),
        added_edges: compact.added_edges.clone(),
        removed_edges: compact.removed_edges.clone(),
        warnings: patch_result.warnings,
    };

    // Check auto-approve flag (snapshotted at run start)
    let auto_approve = {
        let state = app.state::<Mutex<ResolutionState>>();
        state.lock().unwrap().auto_approve
    };

    if auto_approve {
        info!(
            "Auto-approved resolution for node {}: +{} added, ~{} updated, -{} removed",
            query.node_name,
            compact.added_nodes.len(),
            compact.updated_nodes.len(),
            compact.removed_node_ids.len(),
        );

        // Observational event for frontend counter/log (NOT for patch application)
        let _ = app.emit(
            "executor://resolution_auto_approved",
            ResolutionProposedPayload {
                node_id: query.node_id.to_string(),
                node_name: query.node_name.clone(),
                reason: assistant_result.message.clone(),
                patch: frontend_patch.clone(),
                screenshot: None,
            },
        );
    } else {
        // Manual approval flow: emit proposal, wait for user response
        let _ = app.emit(
            "executor://resolution_proposed",
            ResolutionProposedPayload {
                node_id: query.node_id.to_string(),
                node_name: query.node_name.clone(),
                reason: assistant_result.message.clone(),
                patch: frontend_patch.clone(),
                screenshot: query.screenshot.clone(),
            },
        );

        let (tx, rx) = oneshot::channel::<bool>();
        {
            let state = app.state::<Mutex<ResolutionState>>();
            let mut guard = state.lock().unwrap();
            guard.response_tx = Some(tx);
        }

        let approved = tokio::select! {
            _ = cancel_token.cancelled() => {
                info!("Resolution listener cancelled while waiting for approval");
                return RuntimeResolution::Rejected;
            }
            result = rx => match result {
                Ok(v) => v,
                Err(_) => {
                    warn!("Resolution approval channel closed");
                    return RuntimeResolution::Rejected;
                }
            },
        };

        if !approved {
            info!("User rejected resolution for node {}", query.node_name);
            return RuntimeResolution::Rejected;
        }

        info!(
            "User approved resolution for node {}: +{} nodes, ~{} updated, -{} removed",
            query.node_name,
            compact.added_nodes.len(),
            compact.updated_nodes.len(),
            compact.removed_node_ids.len(),
        );
    }

    // Emit patch_applied event (sole trigger for applyRuntimePatch in frontend)
    let _ = app.emit(
        "executor://patch_applied",
        PatchAppliedPayload {
            patch: frontend_patch,
        },
    );

    // Update the resolution workflow snapshot so subsequent queries
    // see the patched graph (not the stale pre-patch version).
    {
        let mut guard = session_handle.lock().await;
        if let Some(ref wf) = guard.resolution_workflow {
            guard.resolution_workflow = Some(clickweave_core::merge_patch_into_workflow(
                wf,
                &compact.added_nodes,
                &compact.removed_node_ids,
                &compact.updated_nodes,
                &compact.added_edges,
                &compact.removed_edges,
            ));
        }
    }

    // Determine the resolution variant.
    // If there are only updates (no adds/removes), it's Updated.
    // If there are removals, it's Removed.
    // TODO: implement insert_before splice logic to return Rewind for added nodes.
    if !compact.removed_node_ids.is_empty() {
        RuntimeResolution::Removed(compact)
    } else {
        RuntimeResolution::Updated(compact)
    }
}

fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// Tauri command: user responds to a resolution proposal (approve/reject).
#[tauri::command]
#[specta::specta]
pub async fn resolution_respond(app: tauri::AppHandle, approved: bool) -> Result<(), CommandError> {
    let state = app.state::<Mutex<ResolutionState>>();
    let tx = {
        let mut guard = state.lock().unwrap();
        guard.response_tx.take()
    };

    if let Some(tx) = tx {
        let _ = tx.send(approved);
        Ok(())
    } else {
        Err(CommandError::validation("No pending resolution"))
    }
}
