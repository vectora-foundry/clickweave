use super::*;

pub struct ScenarioMcp {
    tools: Vec<Value>,
    behaviors: HashMap<String, ToolBehavior>,
    state: Mutex<HashMap<String, Value>>,
    call_counts: Mutex<HashMap<String, usize>>,
    calls: Mutex<Vec<ToolTrace>>,
}

impl ScenarioMcp {
    pub fn new(scenario: &EvalScenario) -> Self {
        let tools = scenario
            .tools
            .iter()
            .map(|tool| {
                json!({
                    "type": "function",
                    "function": {
                        "name": tool.name,
                        "description": tool.description,
                        "parameters": tool.parameters.clone().unwrap_or_else(|| {
                            json!({"type": "object", "properties": {}})
                        })
                    }
                })
            })
            .collect();
        let behaviors = scenario
            .tool_behaviors
            .iter()
            .map(|b| (b.tool.clone(), b.clone()))
            .collect();
        Self {
            tools,
            behaviors,
            state: Mutex::new(HashMap::new()),
            call_counts: Mutex::new(HashMap::new()),
            calls: Mutex::new(Vec::new()),
        }
    }

    pub fn traces(&self) -> Vec<ToolTrace> {
        self.calls.lock().unwrap().clone()
    }

    fn record(&self, tool: &str, arguments: Option<Value>, success: bool, result: String) {
        self.calls.lock().unwrap().push(ToolTrace {
            tool: tool.to_string(),
            arguments: redact_value(arguments.unwrap_or(Value::Null)),
            success,
            result: redact_text(&result),
        });
    }

    fn next_response(&self, tool: &str, behavior: &ToolBehavior) -> ToolResponse {
        let mut counts = self.call_counts.lock().unwrap();
        let idx = counts.entry(tool.to_string()).or_insert(0);
        let call_idx = *idx;
        *idx += 1;

        if behavior.response_sequence.is_empty() {
            return ToolResponse {
                response: behavior.response.clone(),
                error: behavior.error,
                sets_state: behavior.sets_state.clone(),
            };
        }
        behavior.response_sequence[call_idx.min(behavior.response_sequence.len() - 1)].clone()
    }
}

impl Mcp for ScenarioMcp {
    async fn call_tool(&self, name: &str, arguments: Option<Value>) -> Result<ToolCallResult> {
        let args = arguments.clone().unwrap_or(Value::Null);
        let Some(behavior) = self.behaviors.get(name) else {
            let result = "ok".to_string();
            self.record(name, arguments, true, result.clone());
            return Ok(text_result(result, false));
        };

        for required in &behavior.required_args {
            if args.get(required).is_none_or(Value::is_null) {
                let result = format!("missing required argument: {required}");
                self.record(name, arguments, false, result.clone());
                return Ok(text_result(result, true));
            }
        }

        {
            let state = self.state.lock().unwrap();
            for (key, expected) in &behavior.requires_state {
                if state.get(key) != Some(expected) {
                    let result = format!("state requirement not met: {key}");
                    self.record(name, arguments, false, result.clone());
                    return Ok(text_result(result, true));
                }
            }
        }

        let outcome = self.next_response(name, behavior);

        if outcome.error {
            let result = outcome
                .response
                .as_ref()
                .map(response_text)
                .unwrap_or_else(|| "synthetic error".to_string());
            self.record(name, arguments, false, result.clone());
            return Ok(text_result(result, true));
        }

        if !outcome.sets_state.is_empty() {
            let mut state = self.state.lock().unwrap();
            for (key, value) in &outcome.sets_state {
                state.insert(key.clone(), value.clone());
            }
        }

        let result = outcome
            .response
            .as_ref()
            .map(response_text)
            .unwrap_or_else(|| "ok".to_string());
        self.record(name, arguments, true, result.clone());
        Ok(text_result(result, false))
    }

    fn has_tool(&self, name: &str) -> bool {
        if !self
            .tools
            .iter()
            .any(|tool| tool.pointer("/function/name").and_then(Value::as_str) == Some(name))
        {
            return false;
        }
        let Some(behavior) = self.behaviors.get(name) else {
            return true;
        };
        if behavior.requires_state.is_empty() {
            return true;
        }
        let state = self.state.lock().unwrap();
        behavior
            .requires_state
            .iter()
            .all(|(key, expected)| state.get(key) == Some(expected))
    }

    fn tools_as_openai(&self) -> Vec<Value> {
        self.tools.clone()
    }

    async fn refresh_server_tool_list(&self) -> Result<()> {
        Ok(())
    }
}

fn text_result(text: String, is_error: bool) -> ToolCallResult {
    ToolCallResult {
        content: vec![ToolContent::Text { text }],
        is_error: is_error.then_some(true),
    }
}

fn response_text(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        other => serde_json::to_string(other).unwrap_or_else(|_| "ok".to_string()),
    }
}
