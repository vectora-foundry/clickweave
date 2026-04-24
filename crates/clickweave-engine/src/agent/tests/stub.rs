//! Shared test doubles for the state-spine runner tests.
//!
//! These fixtures are consumed by Task 3a.1's `top_level_loop_tests` and
//! reused by Tasks 3a.2–3a.8 (cache replay, VLM, approval, loop detection,
//! CDP lifecycle, boundary writes, end-to-end). Keeping them in one module
//! prevents the later tasks from drifting their own bespoke stubs.
//!
//! | Stub          | Trait              | Behaviour |
//! |---------------|--------------------|-----------|
//! | `ScriptedLlm` | `ChatBackend`      | FIFO queue of canned `ChatResponse`s; extras fall back to a trailing `agent_done` so tests never hang. |
//! | `EchoLlm`     | `ChatBackend`      | Every call returns the same canned response (useful for max-steps tests). |
//! | `StaticMcp`   | `crate::executor::Mcp` | Fixed tool list, per-tool canned reply text. Falls back to `"ok"` when a tool has no registered reply. |
//! | `NullMcp`     | `crate::executor::Mcp` | Advertises no tools; every `call_tool` errors. |
//! | `YesVlm`      | `ChatBackend`      | Always returns a `YES` verdict (for completion-verification tests). |
//! | `NoVlm`       | `ChatBackend`      | Always returns a `NO` verdict. |
//!
//! The helper `llm_reply_tool(tool_name, arguments)` builds a `ChatResponse`
//! holding a single `assistant_tool_calls` message — matching what
//! `LlmClient` returns in real operation.

#![allow(dead_code)] // Consumed piecemeal across Tasks 3a.1–3a.8.

use std::collections::HashMap;
use std::sync::Mutex;

use anyhow::Result;
use clickweave_llm::{
    CallType, ChatBackend, ChatOptions, ChatResponse, Choice, FunctionCall, Message, ToolCall,
};
use clickweave_mcp::{ToolCallResult, ToolContent};
use serde_json::Value;

use crate::executor::Mcp;

// ---------------------------------------------------------------------------
// LLM stubs
// ---------------------------------------------------------------------------

/// Scripted `ChatBackend` that serves canned responses in FIFO order.
///
/// Once the queue is drained, subsequent calls return a trailing
/// `agent_done("scripted_llm: exhausted")` so integration tests never hang
/// on an empty script. Tests that specifically care about exhaustion should
/// assert on `call_count()` instead of relying on the fallback.
pub struct ScriptedLlm {
    responses: Mutex<Vec<ChatResponse>>,
    calls: Mutex<usize>,
}

impl ScriptedLlm {
    /// Build a scripted backend that returns each response in `responses`
    /// once, in order.
    pub fn new(responses: Vec<ChatResponse>) -> Self {
        Self {
            responses: Mutex::new(responses),
            calls: Mutex::new(0),
        }
    }

    /// Build a scripted backend that returns `response` on every call.
    ///
    /// `ChatResponse` is not `Clone` (blocked by upstream types), so this
    /// takes a factory closure that can rebuild the response on demand.
    pub fn repeat<F>(factory: F) -> RepeatLlm<F>
    where
        F: Fn() -> ChatResponse + Send + Sync,
    {
        RepeatLlm::new(factory)
    }

    pub fn call_count(&self) -> usize {
        *self.calls.lock().unwrap()
    }
}

impl ChatBackend for ScriptedLlm {
    fn model_name(&self) -> &str {
        "scripted-llm"
    }

    async fn chat_with_options(
        &self,
        _messages: &[Message],
        _tools: Option<&[Value]>,
        _options: &ChatOptions,
    ) -> Result<ChatResponse> {
        *self.calls.lock().unwrap() += 1;
        let mut q = self.responses.lock().unwrap();
        if q.is_empty() {
            Ok(build_agent_done_response("scripted_llm: exhausted"))
        } else {
            Ok(q.remove(0))
        }
    }
}

/// `ChatBackend` that re-emits the same response on every call, rebuilt
/// through the caller-supplied factory. Built by `ScriptedLlm::repeat`.
pub struct RepeatLlm<F> {
    factory: F,
    calls: Mutex<usize>,
}

impl<F> RepeatLlm<F>
where
    F: Fn() -> ChatResponse + Send + Sync,
{
    pub fn new(factory: F) -> Self {
        Self {
            factory,
            calls: Mutex::new(0),
        }
    }

    pub fn call_count(&self) -> usize {
        *self.calls.lock().unwrap()
    }
}

impl<F> ChatBackend for RepeatLlm<F>
where
    F: Fn() -> ChatResponse + Send + Sync,
{
    fn model_name(&self) -> &str {
        "repeat-llm"
    }

    async fn chat_with_options(
        &self,
        _messages: &[Message],
        _tools: Option<&[Value]>,
        _options: &ChatOptions,
    ) -> Result<ChatResponse> {
        *self.calls.lock().unwrap() += 1;
        Ok((self.factory)())
    }
}

/// `ChatBackend` that mirrors a single canned assistant reply back on every
/// call — useful for tests that only care about a fixed prompt shape being
/// issued (Task 3a.0.6-style signature checks, unit tests for the builder
/// chain).
pub struct EchoLlm {
    text: String,
    calls: Mutex<usize>,
}

impl EchoLlm {
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            calls: Mutex::new(0),
        }
    }

    pub fn call_count(&self) -> usize {
        *self.calls.lock().unwrap()
    }
}

impl ChatBackend for EchoLlm {
    fn model_name(&self) -> &str {
        "echo-llm"
    }

    async fn chat_with_options(
        &self,
        _messages: &[Message],
        _tools: Option<&[Value]>,
        _options: &ChatOptions,
    ) -> Result<ChatResponse> {
        *self.calls.lock().unwrap() += 1;
        Ok(ChatResponse {
            id: "echo".to_string(),
            choices: vec![Choice {
                index: 0,
                message: Message::assistant(&self.text),
                finish_reason: Some("stop".to_string()),
            }],
            usage: None,
        })
    }
}

/// VLM stub that always reports `YES` — the completion verification path
/// treats this as agreement.
#[derive(Default)]
pub struct YesVlm;

impl ChatBackend for YesVlm {
    fn model_name(&self) -> &str {
        "yes-vlm"
    }

    async fn chat_with_options(
        &self,
        _messages: &[Message],
        _tools: Option<&[Value]>,
        _options: &ChatOptions,
    ) -> Result<ChatResponse> {
        Ok(ChatResponse {
            id: "yes-vlm".to_string(),
            choices: vec![Choice {
                index: 0,
                message: Message::assistant("YES"),
                finish_reason: Some("stop".to_string()),
            }],
            usage: None,
        })
    }
}

/// VLM stub that always reports `NO` — the completion verification path
/// surfaces this as `CompletionDisagreement`.
#[derive(Default)]
pub struct NoVlm;

impl ChatBackend for NoVlm {
    fn model_name(&self) -> &str {
        "no-vlm"
    }

    async fn chat_with_options(
        &self,
        _messages: &[Message],
        _tools: Option<&[Value]>,
        _options: &ChatOptions,
    ) -> Result<ChatResponse> {
        Ok(ChatResponse {
            id: "no-vlm".to_string(),
            choices: vec![Choice {
                index: 0,
                message: Message::assistant("NO: the screenshot does not match the goal"),
                finish_reason: Some("stop".to_string()),
            }],
            usage: None,
        })
    }
}

// ---------------------------------------------------------------------------
// MCP stubs
// ---------------------------------------------------------------------------

/// Static `Mcp` stub.
///
/// - Advertises a fixed tool list (built with [`Self::with_tools`]).
/// - Looks up a canned reply body per tool via [`Self::with_reply`].
/// - Tools without a registered reply return `"ok"` as plain text.
/// - `refresh_server_tool_list` is a no-op.
pub struct StaticMcp {
    tools: Vec<Value>,
    replies: Mutex<HashMap<String, String>>,
    /// Image replies, keyed by tool name. When set, the tool returns a
    /// single `ToolContent::Image { data, mime_type }` block instead of the
    /// text reply. Used by completion-verification tests that need
    /// `take_screenshot` to return an image payload the VLM path can prep.
    image_replies: Mutex<HashMap<String, (String, String)>>,
}

impl StaticMcp {
    /// Build an MCP stub that advertises each tool in `names` as a bare
    /// `{"type":"function","function":{"name":"…"}}` entry. Call
    /// [`Self::with_reply`] to register a canned body per tool.
    pub fn with_tools(names: &[&str]) -> Self {
        let tools = names
            .iter()
            .map(|name| {
                serde_json::json!({
                    "type": "function",
                    "function": {
                        "name": *name,
                        "description": format!("stub: {}", name),
                        "parameters": {"type": "object", "properties": {}}
                    }
                })
            })
            .collect();
        Self {
            tools,
            replies: Mutex::new(HashMap::new()),
            image_replies: Mutex::new(HashMap::new()),
        }
    }

    /// Register a canned reply body for `tool_name`.
    pub fn with_reply(self, tool_name: &str, body: &str) -> Self {
        self.replies
            .lock()
            .unwrap()
            .insert(tool_name.to_string(), body.to_string());
        self
    }

    /// Register a canned image reply for `tool_name`. Takes the already
    /// base64-encoded image bytes plus its mime type; the stub returns a
    /// single `ToolContent::Image` block when the tool is called. Used by
    /// completion-verification tests for `take_screenshot`.
    pub fn with_image_reply(self, tool_name: &str, base64_data: &str, mime_type: &str) -> Self {
        self.image_replies.lock().unwrap().insert(
            tool_name.to_string(),
            (base64_data.to_string(), mime_type.to_string()),
        );
        self
    }
}

impl Mcp for StaticMcp {
    async fn call_tool(
        &self,
        name: &str,
        _arguments: Option<Value>,
    ) -> anyhow::Result<ToolCallResult> {
        // Image replies take precedence — they represent tools like
        // `take_screenshot` whose normal shape is image content.
        if let Some((data, mime_type)) = self.image_replies.lock().unwrap().get(name) {
            return Ok(ToolCallResult {
                content: vec![ToolContent::Image {
                    data: data.clone(),
                    mime_type: mime_type.clone(),
                }],
                is_error: None,
            });
        }
        let replies = self.replies.lock().unwrap();
        let text = replies
            .get(name)
            .cloned()
            .unwrap_or_else(|| "ok".to_string());
        Ok(ToolCallResult {
            content: vec![ToolContent::Text { text }],
            is_error: None,
        })
    }

    fn has_tool(&self, name: &str) -> bool {
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

/// `Mcp` stub that advertises no tools. Every `call_tool` errors. Useful
/// for native-only / no-CDP scenarios where `fetch_elements` must return
/// empty without hitting any stubs.
#[derive(Default)]
pub struct NullMcp;

impl Mcp for NullMcp {
    async fn call_tool(
        &self,
        name: &str,
        _arguments: Option<Value>,
    ) -> anyhow::Result<ToolCallResult> {
        anyhow::bail!("null_mcp: no tool named `{}`", name)
    }

    fn has_tool(&self, _name: &str) -> bool {
        false
    }

    fn tools_as_openai(&self) -> Vec<Value> {
        Vec::new()
    }

    async fn refresh_server_tool_list(&self) -> anyhow::Result<()> {
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Response builders
// ---------------------------------------------------------------------------

/// Build a `ChatResponse` that contains a single assistant tool call.
///
/// `tool_call_id` defaults to `"tc-<tool_name>"`; if the caller needs a
/// specific id (e.g. to pin a test assertion), use
/// [`llm_reply_tool_with_id`].
pub fn llm_reply_tool(tool_name: &str, arguments: Value) -> ChatResponse {
    llm_reply_tool_with_id(tool_name, arguments, &format!("tc-{}", tool_name))
}

/// Variant of [`llm_reply_tool`] that accepts an explicit tool-call id.
pub fn llm_reply_tool_with_id(
    tool_name: &str,
    arguments: Value,
    tool_call_id: &str,
) -> ChatResponse {
    ChatResponse {
        id: format!("scripted-{}", tool_name),
        choices: vec![Choice {
            index: 0,
            message: Message::assistant_tool_calls(vec![ToolCall {
                id: tool_call_id.to_string(),
                call_type: CallType::Function,
                function: FunctionCall {
                    name: tool_name.to_string(),
                    arguments,
                },
            }]),
            finish_reason: Some("tool_calls".to_string()),
        }],
        usage: None,
    }
}

/// Build a `ChatResponse` that contains only assistant text (no tool call).
pub fn llm_reply_text(text: &str) -> ChatResponse {
    ChatResponse {
        id: "scripted-text".to_string(),
        choices: vec![Choice {
            index: 0,
            message: Message::assistant(text),
            finish_reason: Some("stop".to_string()),
        }],
        usage: None,
    }
}

fn build_agent_done_response(summary: &str) -> ChatResponse {
    llm_reply_tool("agent_done", serde_json::json!({ "summary": summary }))
}

// ---------------------------------------------------------------------------
// Self-tests — pin the stubs' behaviour so later tasks notice regressions.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod stub_self_tests {
    use super::*;

    #[tokio::test]
    async fn scripted_llm_serves_responses_in_order_then_falls_back_to_agent_done() {
        let llm = ScriptedLlm::new(vec![llm_reply_tool(
            "cdp_click",
            serde_json::json!({"uid":"d1"}),
        )]);
        let r1 = llm.chat(&[Message::user("hi")], None).await.unwrap();
        assert_eq!(
            r1.choices[0].message.tool_calls.as_ref().unwrap()[0]
                .function
                .name,
            "cdp_click"
        );

        // Second call drains the queue and falls back to agent_done.
        let r2 = llm.chat(&[Message::user("hi")], None).await.unwrap();
        let tc = &r2.choices[0].message.tool_calls.as_ref().unwrap()[0];
        assert_eq!(tc.function.name, "agent_done");
        assert_eq!(llm.call_count(), 2);
    }

    #[tokio::test]
    async fn static_mcp_returns_canned_reply_then_falls_back_to_ok() {
        let mcp = StaticMcp::with_tools(&["cdp_click"]).with_reply("cdp_click", "clicked");
        let r = mcp
            .call_tool("cdp_click", Some(serde_json::json!({})))
            .await
            .unwrap();
        assert_eq!(r.content[0].as_text(), Some("clicked"));

        // Tool without a registered reply returns the fallback.
        let missing = mcp.call_tool("some_other_tool", None).await.unwrap();
        assert_eq!(missing.content[0].as_text(), Some("ok"));
    }

    #[test]
    fn static_mcp_advertises_its_tools() {
        let mcp = StaticMcp::with_tools(&["cdp_click", "cdp_find_elements"]);
        assert!(mcp.has_tool("cdp_click"));
        assert!(mcp.has_tool("cdp_find_elements"));
        assert!(!mcp.has_tool("not_there"));
        assert_eq!(mcp.tools_as_openai().len(), 2);
    }

    #[tokio::test]
    async fn null_mcp_advertises_nothing_and_errors_on_call() {
        let mcp = NullMcp;
        assert!(!mcp.has_tool("cdp_click"));
        assert!(mcp.tools_as_openai().is_empty());
        assert!(mcp.call_tool("cdp_click", None).await.is_err());
    }

    #[tokio::test]
    async fn yes_vlm_replies_yes() {
        let v = YesVlm;
        let r = v.chat(&[Message::user("ok?")], None).await.unwrap();
        assert_eq!(r.choices[0].message.content_text(), Some("YES"));
    }

    #[tokio::test]
    async fn no_vlm_replies_no() {
        let v = NoVlm;
        let r = v.chat(&[Message::user("ok?")], None).await.unwrap();
        let text = r.choices[0].message.content_text().unwrap();
        assert!(text.starts_with("NO"));
    }

    #[tokio::test]
    async fn repeat_llm_emits_same_reply_each_call() {
        let llm = ScriptedLlm::repeat(|| llm_reply_tool("take_ax_snapshot", serde_json::json!({})));
        let r1 = llm.chat(&[Message::user("x")], None).await.unwrap();
        let r2 = llm.chat(&[Message::user("x")], None).await.unwrap();
        assert_eq!(
            r1.choices[0].message.tool_calls.as_ref().unwrap()[0]
                .function
                .name,
            "take_ax_snapshot"
        );
        assert_eq!(
            r2.choices[0].message.tool_calls.as_ref().unwrap()[0]
                .function
                .name,
            "take_ax_snapshot"
        );
        assert_eq!(llm.call_count(), 2);
    }

    #[tokio::test]
    async fn echo_llm_returns_fixed_text() {
        let llm = EchoLlm::new("hello world");
        let r = llm.chat(&[Message::user("x")], None).await.unwrap();
        assert_eq!(r.choices[0].message.content_text(), Some("hello world"));
        assert_eq!(llm.call_count(), 1);
    }
}
