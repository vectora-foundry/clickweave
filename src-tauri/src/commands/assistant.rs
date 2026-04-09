use super::error::CommandError;
use super::planner_session::{AssistantSessionHandle, PlannerHandle, PlannerSession};
use super::types::*;
use clickweave_llm::LlmClient;
use clickweave_llm::planner::conversation::{ChatEntry, ChatRole, RunContext};
use std::sync::Arc;
use tauri::{Emitter, Manager};
use tracing::warn;

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

    // Health check: planner endpoint (hard fail)
    clickweave_llm::check_endpoint(
        &request.planner.base_url,
        request.planner.api_key.as_deref(),
        Some(&request.planner.model),
    )
    .await
    .map_err(|e| CommandError::validation(format!("Cannot reach planner model: {}", e)))?;

    // Health check: fast endpoint (warn and degrade)
    let fast_client: Option<LlmClient> = if let Some(ref fast_config) = request.fast {
        if fast_config.is_empty() {
            None
        } else {
            match clickweave_llm::check_endpoint(
                &fast_config.base_url,
                fast_config.api_key.as_deref(),
                Some(&fast_config.model),
            )
            .await
            {
                Ok(()) => Some(LlmClient::new(
                    fast_config
                        .clone()
                        .into_llm_config(Some(0.0))
                        .with_thinking(false)
                        .with_max_tokens(256),
                )),
                Err(e) => {
                    warn!("Fast model unreachable, falling back to planner: {}", e);
                    let _ = app.emit(
                        "assistant://fast_model_warning",
                        serde_json::json!({
                            "message": format!(
                                "Fast model at {} is unreachable. Falling back to planner model.",
                                fast_config.base_url
                            ),
                            "error": e,
                        }),
                    );
                    None
                }
            }
        }
    } else {
        None
    };

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
        let config = request
            .planner
            .clone()
            .into_llm_config(None)
            .with_thinking(false);
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

        // Pre-gather context (may cdp_connect and refresh the MCP tool set)
        let pre_gather_result = super::pre_gather::pre_gather(
            &request.user_message,
            &*session_for_task,
            fast_client.as_ref(),
        )
        .await;

        // Fetch tools AFTER pre-gather so cdp_connect-refreshed tools are included
        let workflow_tools = session_for_task.mcp_tools_openai().await;

        // Filter tools based on app type
        let workflow_tools = clickweave_llm::planner::tool_use::filter_tools_by_app_type(
            &workflow_tools,
            pre_gather_result.all_cdp,
            pre_gather_result.all_native,
        );

        // Pass pre-gathered context to the LLM call
        let pre_gathered_context = if pre_gather_result.context_text.is_empty() {
            None
        } else {
            Some(pre_gather_result.context_text.as_str())
        };

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
            pre_gathered_context,
            pre_gather_result.cdp_connected,
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
            result.intent,
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

    let (message, tool_entries, new_summary, warnings, patch, context_usage, intent) = result?;

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
        let patch_summary = patch.as_ref().map(patch_summary_from_ui);
        guard
            .conversation
            .push_assistant(message.clone(), patch_summary);
        // Handle summarization
        if let Some(ref summary) = new_summary {
            guard.conversation.set_summary(summary.clone(), None);
        }
        // Update config
        guard.assistant_config = Some(request.planner.into_llm_config(None).with_thinking(false));
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
    let patch_summary = patch.as_ref().map(patch_summary_from_ui);
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
        intent,
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
