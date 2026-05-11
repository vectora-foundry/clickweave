use super::*;

// ---------------------------------------------------------------------------
// Completion verification (post-agent_done VLM check)
// ---------------------------------------------------------------------------

/// Hardcoded 1x1 transparent PNG as base64 — used so `prepare_base64_image_for_vlm`
/// has a genuinely decodable image without pulling the `image` crate into
/// clickweave-engine's test deps.
const TINY_PNG_BASE64: &str =
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNkAAIAAAoAAv/lxKUAAAAASUVORK5CYII=";

/// MCP mock that dispatches by tool name — required for completion tests
/// because the loop issues cdp_find_elements and take_screenshot in the
/// same run, and order is not predictable without tool-aware dispatch.
struct RoutingMockMcp {
    /// Sequential responses for `cdp_find_elements`.
    find_elements: Mutex<Vec<ToolCallResult>>,
    /// Sequential responses for `take_screenshot`.
    screenshots: Mutex<Vec<ToolCallResult>>,
    /// Tools advertised through `tools_as_openai`.
    tools: Vec<Value>,
}

impl RoutingMockMcp {
    fn new(find_elements: Vec<ToolCallResult>, screenshots: Vec<ToolCallResult>) -> Self {
        let tools = vec![serde_json::json!({
            "type": "function",
            "function": {
                "name": "take_screenshot",
                "description": "Take a screenshot",
                "parameters": {"type": "object", "properties": {}}
            }
        })];
        Self {
            find_elements: Mutex::new(find_elements),
            screenshots: Mutex::new(screenshots),
            tools,
        }
    }
}

impl Mcp for RoutingMockMcp {
    async fn call_tool(
        &self,
        name: &str,
        _arguments: Option<Value>,
    ) -> anyhow::Result<ToolCallResult> {
        let queue = match name {
            "cdp_find_elements" => &self.find_elements,
            "take_screenshot" => &self.screenshots,
            _ => {
                return Ok(ToolCallResult {
                    content: vec![ToolContent::Text {
                        text: "ok".to_string(),
                    }],
                    is_error: None,
                });
            }
        };
        let mut q = queue.lock().unwrap();
        if q.is_empty() {
            Ok(ToolCallResult {
                content: vec![ToolContent::Text {
                    text: "ok".to_string(),
                }],
                is_error: None,
            })
        } else {
            Ok(q.remove(0))
        }
    }

    fn has_tool(&self, name: &str) -> bool {
        if name == "cdp_summarize_page" || name == "cdp_find_elements" || name == "take_screenshot"
        {
            return true;
        }
        self.tools
            .iter()
            .any(|t| t["function"]["name"].as_str() == Some(name))
    }

    fn tools_as_openai(&self) -> Vec<Value> {
        self.tools.clone()
    }

    async fn refresh_server_tool_list(&self) -> anyhow::Result<()> {
        Ok(())
    }
}

/// MockAgent variant that distinguishes tool-call requests (agent role) from
/// chat requests without tools (VLM role). The agent queue handles requests
/// with a `tools` argument; the vision queue handles requests without.
struct RoutingMockAgent {
    agent_responses: Mutex<Vec<ChatResponse>>,
    vision_responses: Mutex<Vec<ChatResponse>>,
}

impl RoutingMockAgent {
    fn new(agent: Vec<ChatResponse>, vision: Vec<ChatResponse>) -> Self {
        Self {
            agent_responses: Mutex::new(agent),
            vision_responses: Mutex::new(vision),
        }
    }

    fn text_response(text: &str) -> ChatResponse {
        ChatResponse {
            id: "mock-text".to_string(),
            choices: vec![Choice {
                index: 0,
                message: Message::assistant(text),
                finish_reason: Some("stop".to_string()),
            }],
            usage: None,
        }
    }
}

impl ChatBackend for RoutingMockAgent {
    fn model_name(&self) -> &str {
        "routing-mock"
    }

    async fn chat_with_options(
        &self,
        _messages: &[Message],
        tools: Option<&[Value]>,
        _options: &ChatOptions,
    ) -> Result<ChatResponse> {
        let queue = if tools.is_some() {
            &self.agent_responses
        } else {
            &self.vision_responses
        };
        let mut q = queue.lock().unwrap();
        if q.is_empty() {
            // Fallback that keeps the loop from hanging.
            Ok(MockAgent::done_response("No more responses"))
        } else {
            Ok(q.remove(0))
        }
    }

    async fn fetch_model_info(&self) -> Result<Option<ModelInfo>> {
        Ok(None)
    }
}

fn cdp_empty_page_result() -> ToolCallResult {
    ToolCallResult {
        content: vec![ToolContent::Text {
            text: serde_json::json!({
                "page_url": "about:blank",
                "source": "cdp",
                "matches": []
            })
            .to_string(),
        }],
        is_error: None,
    }
}

fn screenshot_result() -> ToolCallResult {
    ToolCallResult {
        content: vec![ToolContent::Image {
            data: TINY_PNG_BASE64.to_string(),
            mime_type: "image/png".to_string(),
        }],
        is_error: None,
    }
}

#[tokio::test]
async fn vlm_yes_verdict_completes_run_normally() {
    use clickweave_llm::DynChatBackend;

    // Agent calls agent_done on step 0; vision backend replies YES.
    let agent_backend = Arc::new(RoutingMockAgent::new(
        vec![MockAgent::done_response("Task finished")],
        vec![RoutingMockAgent::text_response(
            "YES, the screenshot shows the expected state.",
        )],
    ));

    let mcp = RoutingMockMcp::new(vec![cdp_empty_page_result()], vec![screenshot_result()]);

    let config = AgentConfig {
        max_steps: 5,
        build_workflow: false,
        ..Default::default()
    };

    let vlm: Arc<dyn DynChatBackend> = agent_backend.clone();
    let workflow = crate::agent::trace_graph::AgentTraceGraph::new();
    let mcp_tools = mcp.tools_as_openai();

    let (event_tx, mut event_rx) = tokio::sync::mpsc::channel::<RunnerOutput>(16);
    let runner = StateRunner::new("Open settings".to_string(), config)
        .with_vision(vlm)
        .with_events(event_tx);

    let state = runner
        .run(
            &*agent_backend,
            &mcp,
            "Open settings".to_string(),
            workflow,
            mcp_tools,
            None,
        )
        .await
        .unwrap();

    assert!(state.completed, "YES should let the run complete");
    assert!(
        matches!(
            state.terminal_reason,
            Some(TerminalReason::Completed { .. })
        ),
        "Expected Completed, got {:?}",
        state.terminal_reason,
    );

    let mut saw_goal_complete = false;
    let mut saw_disagreement = false;
    while let Ok(ev) = event_rx.try_recv() {
        let Some(ev) = ev.into_event() else {
            continue;
        };
        match ev {
            AgentEvent::GoalComplete { .. } => saw_goal_complete = true,
            AgentEvent::CompletionDisagreement { .. } => saw_disagreement = true,
            _ => {}
        }
    }
    assert!(saw_goal_complete, "Expected GoalComplete event");
    assert!(
        !saw_disagreement,
        "YES must not emit CompletionDisagreement"
    );
}

#[tokio::test]
async fn vlm_no_verdict_halts_run_and_emits_disagreement() {
    use clickweave_llm::DynChatBackend;

    // Agent calls agent_done; vision backend replies NO — the run must halt
    // with CompletionDisagreement and emit a disagreement event.
    let agent_backend = Arc::new(RoutingMockAgent::new(
        vec![MockAgent::done_response("I think it's done")],
        vec![RoutingMockAgent::text_response(
            "NO — the page still shows the previous state.",
        )],
    ));

    let mcp = RoutingMockMcp::new(vec![cdp_empty_page_result()], vec![screenshot_result()]);

    let config = AgentConfig {
        max_steps: 5,
        build_workflow: false,
        ..Default::default()
    };

    let (event_tx, mut event_rx) = tokio::sync::mpsc::channel::<RunnerOutput>(16);
    let vlm: Arc<dyn DynChatBackend> = agent_backend.clone();
    let runner = StateRunner::new("Open settings".to_string(), config)
        .with_vision(vlm)
        .with_events(event_tx);
    let workflow = crate::agent::trace_graph::AgentTraceGraph::new();
    let mcp_tools = mcp.tools_as_openai();

    let state = runner
        .run(
            &*agent_backend,
            &mcp,
            "Open settings".to_string(),
            workflow,
            mcp_tools,
            None,
        )
        .await
        .unwrap();

    assert!(!state.completed, "NO must not mark the run completed");
    match state.terminal_reason {
        Some(TerminalReason::CompletionDisagreement {
            ref agent_summary,
            ref vlm_reasoning,
        }) => {
            assert_eq!(agent_summary, "I think it's done");
            assert!(vlm_reasoning.to_uppercase().starts_with("NO"));
        }
        other => panic!("Expected CompletionDisagreement, got {:?}", other),
    }

    let mut disagreement_payload: Option<(String, String, String)> = None;
    let mut saw_goal_complete = false;
    while let Ok(ev) = event_rx.try_recv() {
        let Some(ev) = ev.into_event() else {
            continue;
        };
        match ev {
            AgentEvent::CompletionDisagreement {
                screenshot_b64,
                vlm_reasoning,
                agent_summary,
            } => {
                disagreement_payload = Some((screenshot_b64, vlm_reasoning, agent_summary));
            }
            AgentEvent::GoalComplete { .. } => saw_goal_complete = true,
            _ => {}
        }
    }
    let (screenshot_b64, vlm_reasoning, agent_summary) =
        disagreement_payload.expect("Expected CompletionDisagreement event");
    assert!(
        !screenshot_b64.is_empty(),
        "Disagreement event must carry the screenshot bytes",
    );
    assert!(vlm_reasoning.to_uppercase().starts_with("NO"));
    assert_eq!(agent_summary, "I think it's done");
    assert!(
        !saw_goal_complete,
        "NO must not emit GoalComplete alongside the disagreement",
    );
}

#[tokio::test]
async fn vlm_check_falls_through_when_reply_is_empty() {
    use clickweave_llm::DynChatBackend;

    // Non-vision endpoints commonly return an empty content body rather
    // than erroring. The loop must treat that as a verifier failure and
    // fall through to Completed, not halt with CompletionDisagreement.
    let agent_backend = Arc::new(RoutingMockAgent::new(
        vec![MockAgent::done_response("Done")],
        vec![RoutingMockAgent::text_response("")],
    ));

    let mcp = RoutingMockMcp::new(vec![cdp_empty_page_result()], vec![screenshot_result()]);

    let config = AgentConfig {
        max_steps: 5,
        build_workflow: false,
        ..Default::default()
    };

    let vlm: Arc<dyn DynChatBackend> = agent_backend.clone();
    let runner = StateRunner::new("Do it".to_string(), config).with_vision(vlm);
    let workflow = crate::agent::trace_graph::AgentTraceGraph::new();
    let mcp_tools = mcp.tools_as_openai();

    let state = runner
        .run(
            &*agent_backend,
            &mcp,
            "Do it".to_string(),
            workflow,
            mcp_tools,
            None,
        )
        .await
        .unwrap();

    assert!(
        state.completed,
        "Empty VLM reply must fall through to Completed"
    );
    assert!(matches!(
        state.terminal_reason,
        Some(TerminalReason::Completed { .. })
    ));
}

#[tokio::test]
async fn vlm_check_falls_through_when_screenshot_fails() {
    use clickweave_llm::DynChatBackend;

    // Agent calls agent_done; take_screenshot returns an error. The loop
    // must complete normally rather than hang or halt.
    let agent_backend = Arc::new(RoutingMockAgent::new(
        vec![MockAgent::done_response("Done")],
        vec![/* vision should never be called */],
    ));

    let failing_screenshot = ToolCallResult {
        content: vec![ToolContent::Text {
            text: "No focused window".to_string(),
        }],
        is_error: Some(true),
    };

    let mcp = RoutingMockMcp::new(vec![cdp_empty_page_result()], vec![failing_screenshot]);

    let config = AgentConfig {
        max_steps: 5,
        build_workflow: false,
        ..Default::default()
    };

    let vlm: Arc<dyn DynChatBackend> = agent_backend.clone();
    let runner = StateRunner::new("Do it".to_string(), config).with_vision(vlm);
    let workflow = crate::agent::trace_graph::AgentTraceGraph::new();
    let mcp_tools = mcp.tools_as_openai();

    let state = runner
        .run(
            &*agent_backend,
            &mcp,
            "Do it".to_string(),
            workflow,
            mcp_tools,
            None,
        )
        .await
        .unwrap();

    assert!(
        state.completed,
        "Screenshot failure must NOT halt the run — fall through to Completed"
    );
    assert!(matches!(
        state.terminal_reason,
        Some(TerminalReason::Completed { .. })
    ));
}
