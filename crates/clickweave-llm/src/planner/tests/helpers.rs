use crate::planner::*;
use crate::{ChatBackend, ChatResponse, Choice, Message};
use std::sync::Mutex;

/// Mock backend that returns a sequence of responses (for testing repair pass).
pub(super) struct MockBackend {
    responses: Mutex<Vec<String>>,
    calls: Mutex<Vec<Vec<Message>>>,
}

impl MockBackend {
    pub(super) fn new(responses: Vec<&str>) -> Self {
        Self {
            responses: Mutex::new(responses.into_iter().map(String::from).collect()),
            calls: Mutex::new(Vec::new()),
        }
    }

    pub(super) fn single(response: &str) -> Self {
        Self::new(vec![response])
    }

    pub(super) fn call_count(&self) -> usize {
        self.calls.lock().unwrap().len()
    }
}

impl ChatBackend for MockBackend {
    fn model_name(&self) -> &str {
        "mock"
    }

    async fn chat(
        &self,
        messages: Vec<Message>,
        _tools: Option<Vec<serde_json::Value>>,
    ) -> anyhow::Result<ChatResponse> {
        self.calls.lock().unwrap().push(messages);
        let mut responses = self.responses.lock().unwrap();
        let text = if responses.is_empty() {
            r#"{"steps": []}"#.to_string()
        } else {
            responses.remove(0)
        };
        Ok(ChatResponse {
            id: "mock".to_string(),
            choices: vec![Choice {
                index: 0,
                message: Message::assistant(&text),
                finish_reason: Some("stop".to_string()),
            }],
            usage: None,
        })
    }
}

/// Build a simple `Variable == Bool(true)` condition for tests.
pub(super) fn bool_condition(var_name: &str) -> clickweave_core::Condition {
    clickweave_core::Condition {
        left: clickweave_core::ValueRef::Variable {
            name: var_name.to_string(),
        },
        operator: clickweave_core::Operator::Equals,
        right: clickweave_core::ValueRef::Literal {
            value: clickweave_core::LiteralValue::Bool { value: true },
        },
    }
}

pub(super) fn sample_tools() -> Vec<serde_json::Value> {
    vec![
        serde_json::json!({
            "type": "function",
            "function": {
                "name": "focus_window",
                "description": "Focus a window",
                "parameters": {"type": "object", "properties": {"app_name": {"type": "string"}}}
            }
        }),
        serde_json::json!({
            "type": "function",
            "function": {
                "name": "take_screenshot",
                "description": "Take a screenshot",
                "parameters": {"type": "object", "properties": {"mode": {"type": "string"}}}
            }
        }),
        serde_json::json!({
            "type": "function",
            "function": {
                "name": "click",
                "description": "Click at coordinates",
                "parameters": {"type": "object", "properties": {"x": {"type": "number"}, "y": {"type": "number"}}}
            }
        }),
        serde_json::json!({
            "type": "function",
            "function": {
                "name": "find_text",
                "description": "Find text on screen",
                "parameters": {"type": "object", "properties": {"text": {"type": "string"}}}
            }
        }),
        serde_json::json!({
            "type": "function",
            "function": {
                "name": "type_text",
                "description": "Type text",
                "parameters": {"type": "object", "properties": {"text": {"type": "string"}}}
            }
        }),
    ]
}

/// Create a single-node workflow for patch tests. Returns the node ID and workflow.
pub(super) fn single_node_workflow(node_type: NodeType, name: &str) -> (uuid::Uuid, Workflow) {
    let node = Node::new(node_type, Position { x: 300.0, y: 100.0 }, name);
    let id = node.id;
    let workflow = Workflow {
        id: uuid::Uuid::new_v4(),
        name: "Test".to_string(),
        nodes: vec![node],
        edges: vec![],
        groups: vec![],
    };
    (id, workflow)
}

/// Run `patch_workflow_with_backend` with standard test defaults (no AI transforms, no agent steps).
pub(super) async fn patch_with_mock(
    mock: &MockBackend,
    workflow: &Workflow,
    prompt: &str,
) -> anyhow::Result<PatchResult> {
    patch_workflow_with_backend(mock, workflow, prompt, &sample_tools(), false, false).await
}
