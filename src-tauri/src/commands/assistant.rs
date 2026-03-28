use super::error::CommandError;
use super::planner_session::{AssistantSessionHandle, PlannerHandle, PlannerSession};
use super::types::*;
use clickweave_llm::planner::conversation::{ChatEntry, ChatRole, RunContext};
use std::sync::Arc;
use tauri::{Emitter, Manager};

fn format_run_context(ctx: &RunContext) -> String {
    let mut lines = vec![format!("Execution: {}", ctx.execution_dir)];
    for nr in &ctx.node_results {
        let mut line = format!("  - {} [{}]", nr.node_name, nr.status);
        if let Some(err) = &nr.error {
            line.push_str(&format!(": {}", err));
        }
        lines.push(line);
    }
    lines.join("\n")
}

fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn patch_summary_from_ui(
    patch: &WorkflowPatch,
) -> clickweave_llm::planner::conversation::PatchSummary {
    clickweave_llm::planner::conversation::PatchSummary {
        added: patch.added_nodes.len() as u32,
        removed: patch.removed_node_ids.len() as u32,
        updated: patch.updated_nodes.len() as u32,
        added_names: patch.added_nodes.iter().map(|n| n.name.clone()).collect(),
        removed_names: Vec::new(),
        updated_names: patch.updated_nodes.iter().map(|n| n.name.clone()).collect(),
        description: None,
    }
}

#[tauri::command]
#[specta::specta]
pub async fn assistant_chat(
    app: tauri::AppHandle,
    request: AssistantChatRequest,
) -> Result<AssistantChatResponse, CommandError> {
    let chrome_profiles = super::chrome_profiles::get_store(&app).load_profiles();

    let planner_handle_state = app.state::<Arc<std::sync::Mutex<PlannerHandle>>>();
    let session_handle_state = app.state::<tokio::sync::Mutex<AssistantSessionHandle>>();

    // Check execution lock
    {
        let guard = session_handle_state.lock().await;
        if guard.is_execution_locked() {
            return Err(CommandError::validation(
                "Cannot send assistant messages during execution",
            ));
        }
    }

    // Step 1: Check if we need to create a session (brief lock)
    let needs_creation = {
        let guard = session_handle_state.lock().await;
        !guard.has_session()
    };

    // Step 2: Create session outside any lock if needed (async MCP spawn)
    if needs_creation {
        let mcp = super::planner::spawn_planning_mcp().await?;
        let tools = mcp.tools_as_openai();
        let session =
            PlannerSession::try_new(mcp, app.clone(), Arc::clone(&planner_handle_state), &tools)
                .await?;
        let mut guard = session_handle_state.lock().await;
        guard.return_session(session);

        // Emit session_started
        let session_id = guard.session_id().unwrap_or("").to_string();
        let _ = app.emit(
            "assistant://session_started",
            SessionStartedPayload {
                session_id: session_id.clone(),
            },
        );
    }

    // Step 3: Take session out and wrap in Arc (brief lock)
    let (session, conversation, config_for_task) = {
        let mut guard = session_handle_state.lock().await;
        guard.session_in_use = true;
        let session = Arc::new(
            guard
                .take_session()
                .ok_or_else(|| CommandError::validation("No planning session available"))?,
        );
        let conversation = guard.conversation.clone();
        let config = request.planner.clone().into_llm_config(None);
        (session, conversation, config)
    };

    let session_id_for_events = session.session_id().to_string();

    // Emit user message event immediately
    let _ = app.emit(
        "assistant://message",
        AssistantMessagePayload {
            session_id: session_id_for_events.clone(),
            entry: ChatEntry {
                role: ChatRole::User,
                content: request.user_message.clone(),
                timestamp: now_millis(),
                patch_summary: None,
                run_context: request.run_context.clone(),
                tool_call_id: None,
                tool_name: None,
            },
        },
    );

    // Step 4: Spawn the LLM call as a task so it can be cancelled.
    let session_for_task = Arc::clone(&session);
    let app_for_task = app.clone();
    let request_user_message = request.user_message.clone();
    let request_run_context = request.run_context.clone();
    let request_run_context_for_task = request_run_context.clone();
    let request_project_path = request.project_path.clone();
    let join_handle = tokio::task::spawn(async move {
        let profiles_ref = if chrome_profiles.len() > 1 {
            Some(chrome_profiles.as_slice())
        } else {
            None
        };

        let emit_handle = app_for_task.clone();
        let on_repair = move |attempt: usize, max: usize| {
            let _ = emit_handle.emit("assistant://repairing", (attempt, max));
        };

        let run_context_text = request_run_context_for_task
            .as_ref()
            .map(format_run_context);
        let workflow_tools = session_for_task.mcp_tools_openai().await;

        let result = clickweave_llm::planner::assistant_chat(
            &request.workflow,
            &request.user_message,
            &conversation,
            run_context_text.as_deref(),
            config_for_task,
            &workflow_tools,
            request.allow_ai_transforms,
            request.allow_agent_steps,
            (request.max_repair_attempts as usize).min(10),
            Some(&on_repair),
            profiles_ref,
            Some(&*session_for_task),
        )
        .await
        .map_err(|e| {
            let root = e.root_cause().to_string();
            CommandError::llm(root)
        })?;

        // Write chat trace (non-fatal)
        let trace_base = match &request_project_path {
            Some(p) => super::types::project_dir(p).join(".clickweave"),
            None => {
                let app_data_dir = app_for_task.state::<AppDataDir>();
                app_data_dir.0.clone()
            }
        };
        let trace =
            clickweave_core::chat_trace::ChatTraceWriter::new(&trace_base, &request.workflow.name);
        trace.append(&serde_json::json!({"role": "user", "content": request.user_message}));
        for tc in &result.tool_entries {
            trace.append(&serde_json::to_value(tc).unwrap_or_default());
        }
        trace.append(&serde_json::json!({"role": "assistant", "content": result.message}));

        // Compute context_usage percentage
        let context_usage = result.prompt_tokens.map(|tokens| {
            let context_window = 32000.0_f32;
            (tokens as f32 / context_window).min(1.0)
        });

        let patch = result.patch.map(|p| WorkflowPatch {
            added_nodes: p.added_nodes,
            removed_node_ids: p.removed_node_ids.iter().map(|id| id.to_string()).collect(),
            updated_nodes: p.updated_nodes,
            added_edges: p.added_edges,
            removed_edges: p.removed_edges,
            warnings: p.warnings,
        });

        Ok((
            result.message,
            result.tool_entries,
            result.new_summary,
            result.warnings,
            patch,
            context_usage,
        ))
    });

    // Store the abort handle so cancel_assistant_chat can abort this task
    {
        let mut guard = session_handle_state.lock().await;
        guard.abort = Some(join_handle.abort_handle());
    }

    let result = match join_handle.await {
        Ok(inner) => inner,
        Err(e) if e.is_cancelled() => Err(CommandError::cancelled()),
        Err(e) => Err(CommandError::internal(format!(
            "Assistant chat panicked: {}",
            e
        ))),
    };

    // Return session to handle
    {
        let mut guard = session_handle_state.lock().await;
        guard.abort = None;
        guard.session_in_use = false;
        match Arc::try_unwrap(session) {
            Ok(session) => guard.return_session(session),
            Err(arc_session) => {
                tokio::spawn(async move {
                    arc_session.cleanup().await;
                });
            }
        }
    }

    let (message, tool_entries, new_summary, warnings, patch, context_usage) = result?;

    // Update backend conversation
    {
        let mut guard = session_handle_state.lock().await;
        // Append user message
        guard
            .conversation
            .push_user(request_user_message.clone(), request_run_context);
        // Append tool entries
        for tc in &tool_entries {
            guard.conversation.messages.push(tc.clone());
        }
        // Append assistant message
        let patch_summary = patch.as_ref().map(|p| patch_summary_from_ui(p));
        guard
            .conversation
            .push_assistant(message.clone(), patch_summary);
        // Handle summarization
        if let Some(ref summary) = new_summary {
            guard.conversation.set_summary(summary.clone(), None);
        }
        // Update config
        guard.assistant_config = Some(request.planner.into_llm_config(None));
    }

    // Emit tool call/result events
    for tc in &tool_entries {
        let _ = app.emit(
            "assistant://message",
            AssistantMessagePayload {
                session_id: session_id_for_events.clone(),
                entry: tc.clone(),
            },
        );
    }

    // Emit assistant response event
    let patch_summary = patch.as_ref().map(|p| patch_summary_from_ui(p));
    let _ = app.emit(
        "assistant://message",
        AssistantMessagePayload {
            session_id: session_id_for_events,
            entry: ChatEntry {
                role: ChatRole::Assistant,
                content: message,
                timestamp: now_millis(),
                patch_summary,
                run_context: None,
                tool_call_id: None,
                tool_name: None,
            },
        },
    );

    Ok(AssistantChatResponse {
        patch,
        warnings,
        context_usage,
    })
}

#[tauri::command]
#[specta::specta]
pub async fn cancel_assistant_chat(app: tauri::AppHandle) -> Result<(), CommandError> {
    let handle = app.state::<tokio::sync::Mutex<AssistantSessionHandle>>();
    let mut guard = handle.lock().await;
    if let Some(abort) = guard.abort.take() {
        abort.abort();
    }
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub async fn get_assistant_session_id(
    app: tauri::AppHandle,
) -> Result<Option<String>, CommandError> {
    let handle = app.state::<tokio::sync::Mutex<AssistantSessionHandle>>();
    let guard = handle.lock().await;
    Ok(guard.session_id().map(|s| s.to_string()))
}

#[tauri::command]
#[specta::specta]
pub async fn rewind_conversation(
    app: tauri::AppHandle,
    to_index: usize,
) -> Result<Vec<ChatEntry>, CommandError> {
    let handle = app.state::<tokio::sync::Mutex<AssistantSessionHandle>>();
    let mut guard = handle.lock().await;
    if guard.execution_locked {
        return Err(CommandError::validation(
            "Cannot rewind conversation during execution",
        ));
    }
    guard.conversation.messages.truncate(to_index);
    if to_index < guard.conversation.summary_cutoff {
        guard.conversation.summary = None;
        guard.conversation.summary_cutoff = 0;
    }
    Ok(guard.conversation.messages.clone())
}
