use super::*;

// ---------------------------------------------------------------------------
// Tool exposure stability: the tool list passed to the LLM must not mutate
// across steps, even when an auto-connect CDP sub-action runs between them.
// Mid-conversation tool-list changes invalidate every prior prompt-cache
// prefix; see the "Tool Exposure" policy in docs/reference/engine/execution.md.
// ---------------------------------------------------------------------------

/// Mock agent that captures the tool list received on every LLM call.
struct ToolCapturingAgent {
    responses: Mutex<Vec<ChatResponse>>,
    captured_tools: Arc<Mutex<Vec<Vec<Value>>>>,
}

impl ToolCapturingAgent {
    fn new(responses: Vec<ChatResponse>, captured: Arc<Mutex<Vec<Vec<Value>>>>) -> Self {
        Self {
            responses: Mutex::new(responses),
            captured_tools: captured,
        }
    }
}

impl ChatBackend for ToolCapturingAgent {
    fn model_name(&self) -> &str {
        "tool-capturing-mock-agent"
    }

    async fn chat_with_options(
        &self,
        _messages: &[Message],
        tools: Option<&[Value]>,
        _options: &ChatOptions,
    ) -> Result<ChatResponse> {
        self.captured_tools
            .lock()
            .unwrap()
            .push(tools.map(|t| t.to_vec()).unwrap_or_default());
        let mut responses = self.responses.lock().unwrap();
        if responses.is_empty() {
            Ok(MockAgent::done_response("No more responses"))
        } else {
            Ok(responses.remove(0))
        }
    }

    async fn fetch_model_info(&self) -> Result<Option<ModelInfo>> {
        Ok(None)
    }
}

/// Mock MCP that models the real `McpClient` cache semantics: a single
/// tool snapshot backs both `has_tool` and `tools_as_openai`, and it only
/// updates when `refresh_server_tool_list` is called. The server's "true" tool set
/// grows after `cdp_connect` (the extras become available), but the mock
/// will keep returning the stale snapshot until refreshed — matching what
/// the production client does.
struct ShiftingToolsMcp {
    results: Mutex<Vec<ToolCallResult>>,
    base_tools: Vec<Value>,
    extra_tools: Vec<Value>,
    /// Server-side visibility: flips to true on `cdp_connect`.
    cdp_connected: std::sync::atomic::AtomicBool,
    /// Client-side cached snapshot of tools; only updated by `refresh_server_tool_list`.
    cached_tools: Mutex<Vec<Value>>,
}

impl ShiftingToolsMcp {
    fn new(results: Vec<ToolCallResult>, base_tools: Vec<Value>, extra_tools: Vec<Value>) -> Self {
        Self {
            results: Mutex::new(results),
            cached_tools: Mutex::new(base_tools.clone()),
            base_tools,
            extra_tools,
            cdp_connected: std::sync::atomic::AtomicBool::new(false),
        }
    }

    /// What the server reports in `tools/list` right now. Grows after
    /// `cdp_connect` succeeds.
    fn server_visible_tools(&self) -> Vec<Value> {
        if self.cdp_connected.load(std::sync::atomic::Ordering::SeqCst) {
            self.base_tools
                .iter()
                .chain(self.extra_tools.iter())
                .cloned()
                .collect()
        } else {
            self.base_tools.clone()
        }
    }
}

impl Mcp for ShiftingToolsMcp {
    async fn call_tool(
        &self,
        name: &str,
        _arguments: Option<Value>,
    ) -> anyhow::Result<ToolCallResult> {
        if name == "cdp_connect" {
            self.cdp_connected
                .store(true, std::sync::atomic::Ordering::SeqCst);
        }
        let mut results = self.results.lock().unwrap();
        if results.is_empty() {
            Ok(ToolCallResult {
                content: vec![ToolContent::Text {
                    text: "ok".to_string(),
                }],
                is_error: None,
            })
        } else {
            Ok(results.remove(0))
        }
    }

    fn has_tool(&self, name: &str) -> bool {
        self.cached_tools
            .lock()
            .unwrap()
            .iter()
            .any(|t| t["function"]["name"].as_str() == Some(name))
    }

    fn tools_as_openai(&self) -> Vec<Value> {
        self.cached_tools.lock().unwrap().clone()
    }

    async fn refresh_server_tool_list(&self) -> anyhow::Result<()> {
        *self.cached_tools.lock().unwrap() = self.server_visible_tools();
        Ok(())
    }
}

#[tokio::test]
async fn tool_list_is_stable_across_cdp_connect_boundary() {
    // The LLM picks launch_app first (which triggers the auto CDP connect
    // sub-actions), then click on the next step, then declares done.
    let captured: Arc<Mutex<Vec<Vec<Value>>>> = Arc::new(Mutex::new(Vec::new()));
    let agent_llm = ToolCapturingAgent::new(
        vec![
            MockAgent::tool_call_response(
                "launch_app",
                r#"{"app_name": "Some Electron App"}"#,
                "call_launch",
            ),
            MockAgent::tool_call_response("click", r#"{"x": 10, "y": 20}"#, "call_click"),
            MockAgent::done_response("All done after launch + click"),
        ],
        captured.clone(),
    );

    // MCP results queue matches the expected call sequence. Pre-connect,
    // `cdp_summarize_page` is not in the client's tool cache, so step 0's
    // observation is a no-op (empty elements) and consumes no result.
    //   step 0 act      -> launch_app
    //   post-hook probe -> probe_app (must say ElectronApp to trigger CDP)
    //   post-hook quit  -> quit_app
    //   post-hook list  -> list_apps (empty so quit is considered done)
    //   post-hook relaunch -> launch_app
    //   post-hook connect  -> cdp_connect (flips the server's tool set)
    //   (refresh_server_tool_list reloads the client cache after connect)
    //   step 1 observe  -> cdp_summarize_page
    //   step 1 act      -> click
    //   step 2 observe  -> cdp_summarize_page
    let cdp_page = |url: &str| ToolCallResult {
        content: vec![ToolContent::Text {
            text: serde_json::json!({
                "page_url": url,
                "source": "cdp",
                "matches": [{
                    "uid": "1_0",
                    "role": "button",
                    "label": "Submit",
                    "tag": "button"
                }]
            })
            .to_string(),
        }],
        is_error: None,
    };
    let text = |s: &str| ToolCallResult {
        content: vec![ToolContent::Text {
            text: s.to_string(),
        }],
        is_error: None,
    };
    let results = vec![
        text("Launched"),         // launch_app
        text("ElectronApp"),      // probe_app
        text("ok"),               // quit_app
        text("[]"),               // list_apps: confirms quit
        text("Launched on port"), // relaunch launch_app
        text("connected"),        // cdp_connect
        // Post-connect selected-page snapshot (agent now tracks the
        // remembered tab like the executor does).
        text("Pages (1 total):\n  [0]* https://example.com/initial\n"),
        cdp_page("https://example.com/after"),
        text("Clicked"), // click
        cdp_page("https://example.com/final"),
    ];

    let base_tools = vec![
        serde_json::json!({
            "type": "function",
            "function": {
                "name": "launch_app",
                "description": "Launch an app",
                "parameters": {
                    "type": "object",
                    "properties": {"app_name": {"type": "string"}},
                    "required": ["app_name"]
                }
            }
        }),
        serde_json::json!({
            "type": "function",
            "function": {
                "name": "click",
                "description": "Click at coordinates",
                "parameters": {
                    "type": "object",
                    "properties": {"x": {"type": "number"}, "y": {"type": "number"}},
                    "required": ["x", "y"]
                }
            }
        }),
        serde_json::json!({
            "type": "function",
            "function": {
                "name": "probe_app",
                "description": "Probe an app",
                "parameters": {
                    "type": "object",
                    "properties": {"app_name": {"type": "string"}},
                    "required": ["app_name"]
                }
            }
        }),
        serde_json::json!({
            "type": "function",
            "function": {
                "name": "cdp_connect",
                "description": "Connect to CDP",
                "parameters": {
                    "type": "object",
                    "properties": {"port": {"type": "number"}},
                    "required": ["port"]
                }
            }
        }),
    ];
    // Extras model CDP tools the server only surfaces after `cdp_connect`:
    //   - `cdp_summarize_page` is what the agent's observation gate checks
    //     (`has_tool(...)` in `fetch_cdp_page_summary`), so it must become visible
    //     on the *client-side cache* after the post-hook runs, or every
    //     later observation will return empty.
    //   - `cdp_click` stands in for any CDP tool that must NOT silently
    //     show up in the agent's LLM-visible tool list mid-run.
    let extra_tools = vec![
        serde_json::json!({
            "type": "function",
            "function": {
                "name": "cdp_summarize_page",
                "description": "Summarize page via CDP",
                "parameters": {
                    "type": "object",
                    "properties": {}
                }
            }
        }),
        serde_json::json!({
            "type": "function",
            "function": {
                "name": "cdp_find_elements",
                "description": "Find elements via CDP",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "query": {"type": "string"},
                        "max_results": {"type": "number"}
                    }
                }
            }
        }),
        serde_json::json!({
            "type": "function",
            "function": {
                "name": "cdp_click",
                "description": "Click via CDP",
                "parameters": {
                    "type": "object",
                    "properties": {"uid": {"type": "string"}},
                    "required": ["uid"]
                }
            }
        }),
    ];

    let mcp = ShiftingToolsMcp::new(results, base_tools, extra_tools);

    let config = AgentConfig {
        max_steps: 5,
        build_workflow: false,
        ..Default::default()
    };

    let runner = StateRunner::new("Launch and click".to_string(), config);
    let workflow = crate::agent::trace_graph::AgentTraceGraph::new();
    // Seed the tools vec from the MCP client once at run start — mirrors
    // how `run_agent_workflow` wires it up.
    let mcp_tools = mcp.tools_as_openai();
    let tool_count_at_start = mcp_tools.len();

    let state = runner
        .run(
            &agent_llm,
            &mcp,
            "Launch and click".to_string(),
            workflow,
            mcp_tools,
            None,
        )
        .await
        .unwrap();

    assert!(state.completed);

    // Sanity: the MCP server's view of its own tools did grow after cdp_connect.
    assert!(
        mcp.tools_as_openai().len() > tool_count_at_start,
        "Test setup broken: ShiftingToolsMcp should expose more tools post-connect"
    );

    // The client-side tool cache must have been refreshed after cdp_connect
    // — otherwise later observation steps would see `has_tool("cdp_summarize_page")`
    // return false and degrade to empty CDP page paths.
    assert!(
        mcp.has_tool("cdp_summarize_page"),
        "Post-CDP-connect refresh did not run: cdp_summarize_page is still \
         absent from the client tool cache, so fetch_cdp_page_summary would \
         return empty on every later observation."
    );

    // And the agent's recorded step for the post-connect click should carry
    // a CDP-sourced page_url, which only happens if fetch_cdp_page_summary
    // actually dispatched `cdp_summarize_page` — i.e. the gate in
    // fetch_cdp_page_summary saw the refreshed cache.
    let click_step = state
        .steps
        .iter()
        .find(|s| matches!(&s.command, AgentCommand::ToolCall { tool_name, .. } if tool_name == "click"))
        .expect("click step should be present");
    assert_eq!(
        click_step.page_url, "https://example.com/after",
        "Expected the click step to observe via CDP after the connect boundary"
    );

    let calls = captured.lock().unwrap();
    assert!(
        calls.len() >= 2,
        "Need at least two LLM calls to compare across a CDP connect boundary"
    );

    // Every LLM call within a single run must see an identical tool list.
    let first = &calls[0];
    for (i, later) in calls.iter().enumerate().skip(1) {
        assert_eq!(
            first, later,
            "Tool list diverged between LLM call 0 and call {i}; \
             mid-run tool mutation invalidates the prompt cache prefix"
        );
    }

    // And the CDP-only tool must *not* have been smuggled into the agent's
    // tool list after the post-hook connect.
    let has_cdp_click = first.iter().any(|t| {
        t.get("function")
            .and_then(|f| f.get("name"))
            .and_then(|n| n.as_str())
            == Some("cdp_click")
    });
    assert!(
        !has_cdp_click,
        "cdp_click leaked into the agent's tool list after auto CDP connect; \
         run-start seed must be the stable contract"
    );
}
