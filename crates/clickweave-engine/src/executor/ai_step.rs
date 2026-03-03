use super::{ExecutorCommand, WorkflowExecutor};
use clickweave_core::{AiStepParams, NodeRun};
use clickweave_llm::{
    ChatBackend, Message, analyze_images, build_step_prompt, workflow_system_prompt,
};
use clickweave_mcp::McpClient;
use serde_json::Value;
use std::time::Instant;
use tokio::sync::mpsc::Receiver;
use tracing::debug;

impl<C: ChatBackend> WorkflowExecutor<C> {
    pub(crate) async fn execute_ai_step(
        &self,
        params: &AiStepParams,
        tools: &[Value],
        mcp: &McpClient,
        timeout_ms: Option<u64>,
        command_rx: &mut Receiver<ExecutorCommand>,
        mut node_run: Option<&mut NodeRun>,
    ) -> Result<Value, String> {
        let mut messages = vec![
            Message::system(workflow_system_prompt()),
            Message::user(build_step_prompt(
                &params.prompt,
                params.button_text.as_deref(),
                params.template_image.as_deref(),
            )),
        ];

        let filtered_tools = if let Some(allowed) = &params.allowed_tools {
            let filtered: Vec<Value> = tools
                .iter()
                .filter(|t| {
                    t.pointer("/function/name")
                        .and_then(|n| n.as_str())
                        .is_some_and(|name| allowed.iter().any(|a| a == name))
                })
                .cloned()
                .collect();
            self.log(format!(
                "Filtered tools: {}/{} allowed",
                filtered.len(),
                tools.len()
            ));
            filtered
        } else {
            tools.to_vec()
        };

        let max_tool_calls = params.max_tool_calls.unwrap_or(10) as usize;
        let step_start = Instant::now();
        let mut tool_call_count = 0;
        let mut last_assistant_text = String::new();

        loop {
            if tool_call_count >= max_tool_calls {
                self.log("Max tool calls reached");
                break;
            }

            if let Some(timeout) = timeout_ms
                && step_start.elapsed().as_millis() as u64 > timeout
            {
                self.log("Timeout reached");
                break;
            }

            if self.stop_requested(command_rx) {
                return Err("Stopped by user".to_string());
            }

            let response = self
                .agent
                .chat(messages.clone(), Some(filtered_tools.clone()))
                .await
                .map_err(|e| format!("LLM error: {}", e))?;

            let choice = response
                .choices
                .first()
                .ok_or_else(|| "No response from LLM".to_string())?;

            let msg = &choice.message;

            let Some(tool_calls) = &msg.tool_calls else {
                if let Some(content) = msg.content_text() {
                    last_assistant_text = content.to_string();
                    let completed = self.check_step_complete(content);
                    self.log(if completed {
                        "Step completed"
                    } else {
                        "Step finished (no tool calls)"
                    });
                } else {
                    self.log("Step finished (no tool calls)");
                }
                break;
            };

            if tool_calls.is_empty() {
                if let Some(content) = msg.content_text() {
                    last_assistant_text = content.to_string();
                    if self.check_step_complete(content) {
                        self.log("Step completed");
                    }
                }
                break;
            }

            messages.push(Message::assistant_tool_calls(tool_calls.clone()));

            let mut pending_images: Vec<(String, String)> = Vec::new();
            let mut last_image_tool = String::new();

            for tool_call in tool_calls {
                tool_call_count += 1;
                self.log(format!("Tool call: {}", tool_call.function.name));
                debug!(
                    tool = %tool_call.function.name,
                    arguments = %tool_call.function.arguments,
                    "Tool call arguments"
                );

                let args: Option<Value> = serde_json::from_str(&tool_call.function.arguments).ok();
                let args = self.resolve_image_paths(args);

                self.record_event(
                    node_run.as_deref(),
                    "tool_call",
                    serde_json::json!({
                        "name": tool_call.function.name,
                        "index": tool_call_count - 1,
                        "args": args,
                    }),
                );

                match mcp.call_tool(&tool_call.function.name, args).await {
                    Ok(result) => {
                        let prefix = format!("toolcall_{}", tool_call_count - 1);
                        let images = self.save_result_images(&result, &prefix, &mut node_run);
                        if !images.is_empty() {
                            last_image_tool = tool_call.function.name.clone();
                        }
                        pending_images.extend(images);

                        let result_text = Self::extract_result_text(&result);

                        self.log(format!(
                            "Tool result: {} chars, {} images",
                            result_text.len(),
                            pending_images.len()
                        ));
                        debug!(
                            tool = %tool_call.function.name,
                            result = %result_text,
                            "Tool result text"
                        );

                        self.record_event(
                            node_run.as_deref(),
                            "tool_result",
                            serde_json::json!({
                                "name": tool_call.function.name,
                                "text": Self::truncate_for_trace(&result_text, 8192),
                                "text_len": result_text.len(),
                                "image_count": pending_images.len(),
                            }),
                        );

                        messages.push(Message::tool_result(&tool_call.id, result_text));
                    }
                    Err(e) => {
                        self.log(format!("Tool call failed: {}", e));
                        messages.push(Message::tool_result(&tool_call.id, format!("Error: {}", e)));
                    }
                }
            }

            if !pending_images.is_empty() {
                let image_count = pending_images.len();

                let prepared_images: Vec<(String, String)> = pending_images
                    .into_iter()
                    .filter_map(|(b64, _mime)| {
                        clickweave_llm::prepare_base64_image_for_vlm(
                            &b64,
                            clickweave_llm::DEFAULT_MAX_DIMENSION,
                        )
                    })
                    .collect();

                if prepared_images.is_empty() {
                    self.log(format!(
                        "Failed to prepare {} image(s) for VLM",
                        image_count
                    ));
                } else if let Some(vlm) = self.vision_backend() {
                    self.log(format!(
                        "Analyzing {} image(s) with VLM ({})",
                        image_count,
                        vlm.model_name()
                    ));
                    match analyze_images(vlm, &params.prompt, &last_image_tool, prepared_images)
                        .await
                    {
                        Ok(summary) => {
                            self.record_event(
                                node_run.as_deref(),
                                "vision_summary",
                                serde_json::json!({
                                    "image_count": image_count,
                                    "vlm_model": vlm.model_name(),
                                    "summary_json": summary,
                                }),
                            );
                            messages
                                .push(Message::user(format!("VLM_IMAGE_SUMMARY:\n{}", summary)));
                        }
                        Err(e) => {
                            self.log(format!("VLM analysis failed: {}", e));
                            messages.push(Message::user(
                                "(Vision analysis failed; consider using find_text or find_image for precise targeting)"
                                    .to_string(),
                            ));
                        }
                    }
                } else {
                    messages.push(Message::user_with_images(
                        "Here are the images from the tool results above.",
                        prepared_images,
                    ));
                }
            }
        }

        Ok(Value::String(last_assistant_text))
    }
}
