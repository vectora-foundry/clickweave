use serde::{Deserialize, Serialize};
use serde_json::Value;

/// JSON-RPC 2.0 request
#[derive(Debug, Serialize)]
pub struct JsonRpcRequest {
    pub jsonrpc: &'static str,
    pub id: u64,
    pub method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

impl JsonRpcRequest {
    pub fn new(id: u64, method: impl Into<String>, params: Option<Value>) -> Self {
        Self {
            jsonrpc: "2.0",
            id,
            method: method.into(),
            params,
        }
    }
}

/// JSON-RPC 2.0 response
#[derive(Debug, Deserialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: String,
    pub id: Option<u64>,
    #[serde(default)]
    pub result: Option<Value>,
    #[serde(default)]
    pub error: Option<JsonRpcError>,
}

#[derive(Debug, Deserialize)]
pub struct JsonRpcError {
    pub code: i64,
    pub message: String,
    pub data: Option<Value>,
}

/// MCP Initialize request params
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InitializeParams {
    pub protocol_version: String,
    pub capabilities: ClientCapabilities,
    pub client_info: ClientInfo,
}

#[derive(Debug, Serialize, Default)]
pub struct ClientCapabilities {}

#[derive(Debug, Serialize)]
pub struct ClientInfo {
    pub name: String,
    pub version: String,
}

/// MCP Initialize response
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InitializeResult {
    pub protocol_version: String,
    pub capabilities: ServerCapabilities,
    pub server_info: Option<ServerInfo>,
}

#[derive(Debug, Deserialize, Default)]
pub struct ServerCapabilities {
    #[serde(default)]
    pub tools: Option<ToolsCapability>,
}

#[derive(Debug, Deserialize)]
pub struct ToolsCapability {
    #[serde(default)]
    pub list_changed: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct ServerInfo {
    pub name: String,
    pub version: Option<String>,
}

/// MCP Tool definition
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Tool {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    pub input_schema: Value,
}

/// Convert a slice of MCP tools to OpenAI-compatible function-calling format.
pub fn tools_to_openai(tools: &[Tool]) -> Vec<Value> {
    tools
        .iter()
        .map(|tool| {
            serde_json::json!({
                "type": "function",
                "function": {
                    "name": tool.name,
                    "description": tool.description,
                    "parameters": tool.input_schema
                }
            })
        })
        .collect()
}

/// MCP tools/list response
#[derive(Debug, Deserialize)]
pub struct ToolsListResult {
    pub tools: Vec<Tool>,
}

/// MCP tools/call request params
#[derive(Debug, Serialize)]
pub struct ToolCallParams {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub arguments: Option<Value>,
}

/// MCP tools/call response
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolCallResult {
    pub content: Vec<ToolContent>,
    #[serde(default)]
    pub is_error: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum ToolContent {
    Text {
        text: String,
    },
    Image {
        data: String,
        #[serde(rename = "mimeType")]
        mime_type: String,
    },
    #[serde(other)]
    Unknown,
}

impl ToolContent {
    pub fn as_text(&self) -> Option<&str> {
        match self {
            ToolContent::Text { text } => Some(text),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // ── JsonRpcRequest serialization ────────────────────────────────────

    #[test]
    fn jsonrpc_request_serializes_with_params() {
        let req = JsonRpcRequest::new(1, "tools/list", Some(json!({"key": "value"})));
        let serialized = serde_json::to_value(&req).unwrap();

        assert_eq!(serialized["jsonrpc"], "2.0");
        assert_eq!(serialized["id"], 1);
        assert_eq!(serialized["method"], "tools/list");
        assert_eq!(serialized["params"], json!({"key": "value"}));
    }

    #[test]
    fn jsonrpc_request_omits_params_when_none() {
        let req = JsonRpcRequest::new(42, "notifications/initialized", None);
        let serialized = serde_json::to_value(&req).unwrap();

        assert_eq!(serialized["jsonrpc"], "2.0");
        assert_eq!(serialized["id"], 42);
        assert_eq!(serialized["method"], "notifications/initialized");
        assert!(serialized.get("params").is_none());
    }

    // ── JsonRpcResponse deserialization ─────────────────────────────────

    #[test]
    fn jsonrpc_response_success_deserialization() {
        let raw = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": {"protocolVersion": "2024-11-05"}
        });

        let resp: JsonRpcResponse = serde_json::from_value(raw).unwrap();
        assert_eq!(resp.jsonrpc, "2.0");
        assert_eq!(resp.id, Some(1));
        assert!(resp.error.is_none());

        let result = resp.result.unwrap();
        assert_eq!(result["protocolVersion"], "2024-11-05");
    }

    #[test]
    fn jsonrpc_response_error_deserialization() {
        let raw = json!({
            "jsonrpc": "2.0",
            "id": 5,
            "error": {
                "code": -32601,
                "message": "Method not found",
                "data": null
            }
        });

        let resp: JsonRpcResponse = serde_json::from_value(raw).unwrap();
        assert_eq!(resp.id, Some(5));
        assert!(resp.result.is_none());

        let err = resp.error.unwrap();
        assert_eq!(err.code, -32601);
        assert_eq!(err.message, "Method not found");
    }

    // ── InitializeParams serialization ──────────────────────────────────

    #[test]
    fn initialize_params_serializes_camel_case() {
        let params = InitializeParams {
            protocol_version: "2024-11-05".to_string(),
            capabilities: ClientCapabilities::default(),
            client_info: ClientInfo {
                name: "test-client".to_string(),
                version: "0.1.0".to_string(),
            },
        };

        let serialized = serde_json::to_value(&params).unwrap();

        assert_eq!(serialized["protocolVersion"], "2024-11-05");
        assert!(serialized.get("capabilities").is_some());
        assert_eq!(serialized["clientInfo"]["name"], "test-client");
        assert_eq!(serialized["clientInfo"]["version"], "0.1.0");
        // Verify camelCase — snake_case keys must not appear
        assert!(serialized.get("protocol_version").is_none());
        assert!(serialized.get("client_info").is_none());
    }

    // ── ToolsListResult deserialization ──────────────────────────────────

    #[test]
    fn tools_list_result_deserialization() {
        let raw = json!({
            "tools": [
                {
                    "name": "click",
                    "description": "Click at coordinates",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "x": {"type": "number"},
                            "y": {"type": "number"}
                        },
                        "required": ["x", "y"]
                    }
                },
                {
                    "name": "take_screenshot",
                    "description": "Capture the screen",
                    "inputSchema": {"type": "object", "properties": {}}
                }
            ]
        });

        let result: ToolsListResult = serde_json::from_value(raw).unwrap();
        assert_eq!(result.tools.len(), 2);
        assert_eq!(result.tools[0].name, "click");
        assert_eq!(
            result.tools[0].description.as_deref(),
            Some("Click at coordinates")
        );
        assert_eq!(result.tools[1].name, "take_screenshot");
        assert_eq!(result.tools[0].input_schema["required"], json!(["x", "y"]));
    }

    // ── ToolCallResult deserialization ───────────────────────────────────

    #[test]
    fn tool_call_result_success_with_text_content() {
        let raw = json!({
            "content": [
                {"type": "text", "text": "Clicked at (100, 200)"}
            ],
            "isError": false
        });

        let result: ToolCallResult = serde_json::from_value(raw).unwrap();
        assert_eq!(result.is_error, Some(false));
        assert_eq!(result.content.len(), 1);
        assert_eq!(result.content[0].as_text(), Some("Clicked at (100, 200)"));
    }

    #[test]
    fn tool_call_result_error_flag() {
        let raw = json!({
            "content": [
                {"type": "text", "text": "Element not found"}
            ],
            "isError": true
        });

        let result: ToolCallResult = serde_json::from_value(raw).unwrap();
        assert_eq!(result.is_error, Some(true));
        assert_eq!(result.content[0].as_text(), Some("Element not found"));
    }

    #[test]
    fn tool_call_result_with_image_content() {
        let raw = json!({
            "content": [
                {
                    "type": "image",
                    "data": "iVBORw0KGgo=",
                    "mimeType": "image/png"
                }
            ]
        });

        let result: ToolCallResult = serde_json::from_value(raw).unwrap();
        assert_eq!(result.is_error, None);
        assert_eq!(result.content.len(), 1);
        match &result.content[0] {
            ToolContent::Image { data, mime_type } => {
                assert_eq!(data, "iVBORw0KGgo=");
                assert_eq!(mime_type, "image/png");
            }
            other => panic!("Expected Image variant, got {:?}", other),
        }
    }

    #[test]
    fn tool_call_result_mixed_content() {
        let raw = json!({
            "content": [
                {"type": "text", "text": "Screenshot captured"},
                {"type": "image", "data": "abc=", "mimeType": "image/jpeg"}
            ]
        });

        let result: ToolCallResult = serde_json::from_value(raw).unwrap();
        assert_eq!(result.content.len(), 2);
        assert_eq!(result.content[0].as_text(), Some("Screenshot captured"));
        assert!(matches!(&result.content[1], ToolContent::Image { .. }));
    }

    #[test]
    fn tool_content_unknown_type_deserializes_as_unknown() {
        let raw = json!({
            "content": [
                {"type": "resource", "uri": "file:///tmp/data.json"}
            ]
        });

        let result: ToolCallResult = serde_json::from_value(raw).unwrap();
        assert_eq!(result.content.len(), 1);
        assert!(matches!(&result.content[0], ToolContent::Unknown));
        assert!(result.content[0].as_text().is_none());
    }

    // ── tools_to_openai conversion ──────────────────────────────────────

    #[test]
    fn tools_to_openai_converts_to_function_format() {
        let tools = vec![
            Tool {
                name: "click".to_string(),
                description: Some("Click at coordinates".to_string()),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "x": {"type": "number"},
                        "y": {"type": "number"}
                    }
                }),
            },
            Tool {
                name: "type_text".to_string(),
                description: Some("Type text into element".to_string()),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "text": {"type": "string"}
                    }
                }),
            },
        ];

        let openai = tools_to_openai(&tools);
        assert_eq!(openai.len(), 2);

        // First tool
        assert_eq!(openai[0]["type"], "function");
        assert_eq!(openai[0]["function"]["name"], "click");
        assert_eq!(openai[0]["function"]["description"], "Click at coordinates");
        assert_eq!(openai[0]["function"]["parameters"]["type"], "object");

        // Second tool
        assert_eq!(openai[1]["function"]["name"], "type_text");
        assert_eq!(
            openai[1]["function"]["description"],
            "Type text into element"
        );
    }

    #[test]
    fn tools_to_openai_empty_input_returns_empty_vec() {
        let openai = tools_to_openai(&[]);
        assert!(openai.is_empty());
    }

    #[test]
    fn tools_to_openai_tool_with_no_description() {
        let tools = vec![Tool {
            name: "ping".to_string(),
            description: None,
            input_schema: json!({"type": "object"}),
        }];

        let openai = tools_to_openai(&tools);
        assert_eq!(openai.len(), 1);
        assert_eq!(openai[0]["function"]["name"], "ping");
        assert!(openai[0]["function"]["description"].is_null());
        assert_eq!(
            openai[0]["function"]["parameters"],
            json!({"type": "object"})
        );
    }

    // ── ToolCallParams serialization ────────────────────────────────────

    #[test]
    fn tool_call_params_serializes_with_arguments() {
        let params = ToolCallParams {
            name: "click".to_string(),
            arguments: Some(json!({"x": 100, "y": 200})),
        };

        let serialized = serde_json::to_value(&params).unwrap();
        assert_eq!(serialized["name"], "click");
        assert_eq!(serialized["arguments"], json!({"x": 100, "y": 200}));
    }

    #[test]
    fn tool_call_params_omits_arguments_when_none() {
        let params = ToolCallParams {
            name: "list_windows".to_string(),
            arguments: None,
        };

        let serialized = serde_json::to_value(&params).unwrap();
        assert_eq!(serialized["name"], "list_windows");
        assert!(serialized.get("arguments").is_none());
    }
}
