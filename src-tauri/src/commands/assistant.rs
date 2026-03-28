use super::error::CommandError;
use super::planner_session::{AssistantSessionHandle, PlannerHandle, PlannerSession};
use super::types::*;
use clickweave_llm::planner::conversation::{ConversationSession, RunContext};
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

#[tauri::command]
#[specta::specta]
pub async fn assistant_chat(
    app: tauri::AppHandle,
    request: AssistantChatRequest,
) -> Result<AssistantChatResponse, CommandError> {
    let chrome_profiles = super::chrome_profiles::get_store(&app).load_profiles();

    let planner_handle_state = app.state::<Arc<std::sync::Mutex<PlannerHandle>>>();
    let session_handle_state = app.state::<tokio::sync::Mutex<AssistantSessionHandle>>();

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
    }

    // Step 3: Take session out and wrap in Arc (brief lock)
    let session = {
        let mut guard = session_handle_state.lock().await;
        Arc::new(
            guard
                .take_session()
                .ok_or_else(|| CommandError::validation("No planning session available"))?,
        )
    };

    // Step 4: Spawn the LLM call as a task so it can be cancelled.
    // The session Arc is shared into the task; we unwrap it back after completion.
    let session_for_task = Arc::clone(&session);
    let app_for_task = app.clone();
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

        let config = request.planner.into_llm_config(None);
        let conversation_session = ConversationSession {
            messages: request.history,
            summary: request.summary,
            summary_cutoff: request.summary_cutoff,
        };
        let run_context_text = request.run_context.as_ref().map(format_run_context);
        let workflow_tools = session_for_task.mcp_tools_openai().await;

        let result = clickweave_llm::planner::assistant_chat(
            &request.workflow,
            &request.user_message,
            &conversation_session,
            run_context_text.as_deref(),
            config,
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
            // Use the root cause for a cleaner user-facing message
            let root = e.root_cause().to_string();
            CommandError::llm(root)
        })?;

        // Write chat trace (non-fatal)
        let trace_base = match &request.project_path {
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
            let context_window = 32000.0_f32; // fallback; ideally query model_info
            (tokens as f32 / context_window).min(1.0)
        });

        let new_cutoff = if result.new_summary.is_some() {
            conversation_session.current_cutoff(None)
        } else {
            request.summary_cutoff
        };

        let patch = result.patch.map(|p| WorkflowPatch {
            added_nodes: p.added_nodes,
            removed_node_ids: p.removed_node_ids.iter().map(|id| id.to_string()).collect(),
            updated_nodes: p.updated_nodes,
            added_edges: p.added_edges,
            removed_edges: p.removed_edges,
            warnings: p.warnings,
        });

        Ok(AssistantChatResponse {
            assistant_message: result.message,
            patch,
            new_summary: result.new_summary,
            summary_cutoff: new_cutoff,
            warnings: result.warnings,
            tool_entries: result.tool_entries,
            context_usage,
        })
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

    // Return session to handle, or clean up if the task was cancelled.
    {
        let mut guard = session_handle_state.lock().await;
        guard.abort = None;
        match Arc::try_unwrap(session) {
            Ok(session) => guard.return_session(session),
            Err(arc_session) => {
                // Task was cancelled — can't unwrap Arc yet because the aborted
                // task may still briefly hold a ref. Run cleanup via &self (works
                // through Arc) to clear PlannerHandle.session_id so the next turn
                // can create a fresh session.
                tokio::spawn(async move {
                    arc_session.cleanup().await;
                });
            }
        }
    }

    result
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
