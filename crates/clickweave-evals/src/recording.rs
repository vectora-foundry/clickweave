use super::*;

pub struct RecordingBackend<B> {
    inner: B,
    turns: Mutex<Vec<LlmTurnTrace>>,
    stop_after_agent_tools: HashSet<String>,
    eval_halt: Mutex<Option<EvalHalt>>,
}

impl<B> RecordingBackend<B> {
    pub fn new(inner: B) -> Self {
        Self {
            inner,
            turns: Mutex::new(Vec::new()),
            stop_after_agent_tools: HashSet::new(),
            eval_halt: Mutex::new(None),
        }
    }

    pub fn with_stop_after_agent_tools(inner: B, stop_after_agent_tools: &[String]) -> Self {
        Self {
            inner,
            turns: Mutex::new(Vec::new()),
            stop_after_agent_tools: stop_after_agent_tools.iter().cloned().collect(),
            eval_halt: Mutex::new(None),
        }
    }

    pub fn traces(&self) -> Vec<LlmTurnTrace> {
        self.turns.lock().unwrap().clone()
    }

    pub fn eval_halt(&self) -> Option<EvalHalt> {
        self.eval_halt.lock().unwrap().clone()
    }

    fn maybe_record_eval_halt(&self, assistant: &Option<AssistantTrace>) -> bool {
        if self.stop_after_agent_tools.is_empty() {
            return false;
        }
        let Some(tool) = assistant
            .as_ref()
            .and_then(|assistant| {
                assistant
                    .tool_calls
                    .iter()
                    .find(|call| self.stop_after_agent_tools.contains(&call.name))
            })
            .map(|call| call.name.clone())
        else {
            return false;
        };
        let mut halt = self.eval_halt.lock().unwrap();
        if halt.is_none() {
            *halt = Some(EvalHalt {
                reason: "stop_after_agent_tools".to_string(),
                agent_tool: tool,
            });
            return true;
        }
        false
    }
}

#[derive(Debug)]
pub(crate) struct EvalHaltTriggered;

impl fmt::Display for EvalHaltTriggered {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("eval halted after configured agent tool")
    }
}

impl Error for EvalHaltTriggered {}

impl<B: ChatBackend> ChatBackend for RecordingBackend<B> {
    fn model_name(&self) -> &str {
        self.inner.model_name()
    }

    async fn chat_with_options(
        &self,
        messages: &[Message],
        tools: Option<&[Value]>,
        options: &ChatOptions,
    ) -> Result<ChatResponse> {
        let request_messages = redact_messages(messages)?;
        match self.inner.chat_with_options(messages, tools, options).await {
            Ok(response) => {
                let assistant = response.choices.first().map(|choice| AssistantTrace {
                    content: choice.message.content_text().map(redact_text),
                    tool_calls: choice
                        .message
                        .tool_calls
                        .as_ref()
                        .map(|calls| calls.iter().map(redact_tool_call).collect())
                        .unwrap_or_default(),
                    finish_reason: choice.finish_reason.clone(),
                });
                self.turns.lock().unwrap().push(LlmTurnTrace {
                    request_messages,
                    assistant: assistant.clone(),
                    error: None,
                });
                if self.maybe_record_eval_halt(&assistant) {
                    return Err(EvalHaltTriggered.into());
                }
                Ok(response)
            }
            Err(err) => {
                self.turns.lock().unwrap().push(LlmTurnTrace {
                    request_messages,
                    assistant: None,
                    error: Some(redact_text(&err.to_string())),
                });
                Err(err)
            }
        }
    }

    async fn fetch_model_info(&self) -> Result<Option<clickweave_llm::ModelInfo>> {
        self.inner.fetch_model_info().await
    }
}
