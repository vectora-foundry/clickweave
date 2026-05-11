use crate::agent::StateRunner;
use crate::agent::types::*;
use crate::executor::Mcp;
use anyhow::Result;
use clickweave_llm::{
    CallType, ChatBackend, ChatOptions, ChatResponse, Choice, FunctionCall, Message, ModelInfo,
    Role, ToolCall,
};
use clickweave_mcp::{ToolCallResult, ToolContent};
use serde_json::Value;
use std::sync::{Arc, Mutex};

// ---------------------------------------------------------------------------
// Mock LLM backend
// ---------------------------------------------------------------------------

struct MockAgent {
    responses: Mutex<Vec<ChatResponse>>,
}

impl MockAgent {
    fn new(responses: Vec<ChatResponse>) -> Self {
        Self {
            responses: Mutex::new(responses),
        }
    }

    /// Convenience: build a ChatResponse with a single tool call.
    fn tool_call_response(tool_name: &str, arguments: &str, tool_call_id: &str) -> ChatResponse {
        let parsed: Value = serde_json::from_str(arguments)
            .unwrap_or_else(|_| Value::String(arguments.to_string()));
        ChatResponse {
            id: "mock-resp".to_string(),
            choices: vec![Choice {
                index: 0,
                message: Message::assistant_tool_calls(vec![ToolCall {
                    id: tool_call_id.to_string(),
                    call_type: CallType::Function,
                    function: FunctionCall {
                        name: tool_name.to_string(),
                        arguments: parsed,
                    },
                }]),
                finish_reason: Some("tool_calls".to_string()),
            }],
            usage: None,
        }
    }

    /// Convenience: build a ChatResponse with the agent_done pseudo-tool.
    fn done_response(summary: &str) -> ChatResponse {
        Self::tool_call_response(
            "agent_done",
            &serde_json::json!({ "summary": summary }).to_string(),
            "call_done",
        )
    }
}

impl ChatBackend for MockAgent {
    fn model_name(&self) -> &str {
        "mock-agent"
    }

    async fn chat_with_options(
        &self,
        _messages: &[Message],
        _tools: Option<&[Value]>,
        _options: &ChatOptions,
    ) -> Result<ChatResponse> {
        let mut responses = self.responses.lock().unwrap();
        if responses.is_empty() {
            // Fallback: return agent_done so tests don't hang
            Ok(Self::done_response("No more responses"))
        } else {
            Ok(responses.remove(0))
        }
    }

    async fn fetch_model_info(&self) -> Result<Option<ModelInfo>> {
        Ok(None)
    }
}

// ---------------------------------------------------------------------------
// Mock MCP client
// ---------------------------------------------------------------------------

struct MockMcp {
    tool_results: Mutex<Vec<ToolCallResult>>,
    tools: Vec<Value>,
}

impl MockMcp {
    fn new(tool_results: Vec<ToolCallResult>, tools: Vec<Value>) -> Self {
        Self {
            tool_results: Mutex::new(tool_results),
            tools,
        }
    }

    fn with_click_tool() -> Self {
        let tools = vec![serde_json::json!({
            "type": "function",
            "function": {
                "name": "click",
                "description": "Click at coordinates",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "x": {"type": "number"},
                        "y": {"type": "number"}
                    },
                    "required": ["x", "y"]
                }
            }
        })];

        // First call: cdp_find_elements returns empty (no elements)
        // Second call: click returns success
        let results = vec![
            // cdp_find_elements result
            ToolCallResult {
                content: vec![ToolContent::Text {
                    text: serde_json::json!({
                        "page_url": "https://example.com",
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
            },
            // click result
            ToolCallResult {
                content: vec![ToolContent::Text {
                    text: "Clicked at (100, 200)".to_string(),
                }],
                is_error: None,
            },
            // cdp_find_elements for second observation
            ToolCallResult {
                content: vec![ToolContent::Text {
                    text: serde_json::json!({
                        "page_url": "https://example.com/success",
                        "source": "cdp",
                        "matches": [{
                            "uid": "2_0",
                            "role": "heading",
                            "label": "Success",
                            "tag": "h1"
                        }]
                    })
                    .to_string(),
                }],
                is_error: None,
            },
        ];

        Self::new(results, tools)
    }
}

impl Mcp for MockMcp {
    async fn call_tool(
        &self,
        _name: &str,
        _arguments: Option<Value>,
    ) -> anyhow::Result<ToolCallResult> {
        let mut results = self.tool_results.lock().unwrap();
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
        // Support CDP observation helpers and any tools in the list.
        if name == "cdp_find_elements" || name == "cdp_summarize_page" {
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

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

mod basic_run_tests;
mod completion_verification_tests;
mod conversational_extend;
mod permission_guard_tests;
mod runner_integration_tests;
mod tool_surface_tests;
mod transcript_tests;

// ---------------------------------------------------------------------------
// Cross-task / cross-process coordination tests
//
// These exercise the full `AgentChannels` contract end-to-end: a harness
// task plays the role the Tauri forwarders play in production, and the
// assertions verify the engine responds to realistic sequences of events
// (stop-during-approval, empty-elements native paths, tool-mapping misses,
// cross-run event draining).
// ---------------------------------------------------------------------------

mod coordination;

// ---------------------------------------------------------------------------
// CDP lifecycle parity tests
//
// These assert that the agent runner now maintains the same
// `cdp_selected_pages` bookkeeping the executor has long relied on —
// the correctness gap that motivated the `cdp_lifecycle` consolidation.
// Before the refactor, the agent observed a lost tab only on the next
// observation step, and any action dispatched in between was committed
// against the wrong tab.
// ---------------------------------------------------------------------------

mod cdp_lifecycle_parity;
