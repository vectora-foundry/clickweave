use anyhow::{Context, Result};
use clickweave_core::cdp::{CdpFindElementMatch, CdpFindElementsResponse};
use clickweave_core::tool_mapping::tool_invocation_to_node_type;
use clickweave_core::{Edge, Node, Position, Workflow};
use clickweave_llm::{ChatBackend, Message};
use serde_json::Value;
use tracing::{debug, info, warn};

use super::context;
use super::prompt;
use super::recovery::{self, RecoveryAction};
use super::transition;
use super::types::*;
use crate::executor::Mcp;

/// Extract text content from an MCP tool call result.
fn extract_result_text(result: &clickweave_mcp::ToolCallResult) -> String {
    result
        .content
        .iter()
        .filter_map(|c| match c {
            clickweave_mcp::ToolContent::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The main agent runner that implements the observe-act loop.
pub struct AgentRunner<'a, B: ChatBackend> {
    llm: &'a B,
    config: AgentConfig,
    state: AgentState,
    messages: Vec<Message>,
    cache: AgentCache,
}

impl<'a, B: ChatBackend> AgentRunner<'a, B> {
    pub fn new(llm: &'a B, config: AgentConfig) -> Self {
        Self {
            llm,
            config,
            state: AgentState::new(Workflow::new("Agent Workflow")),
            messages: Vec::new(),
            cache: AgentCache::default(),
        }
    }

    /// Create a runner pre-loaded with a cross-run decision cache.
    pub fn with_cache(llm: &'a B, config: AgentConfig, cache: AgentCache) -> Self {
        Self {
            llm,
            config,
            state: AgentState::new(Workflow::new("Agent Workflow")),
            messages: Vec::new(),
            cache,
        }
    }

    /// Consume the runner and return the accumulated cache.
    pub fn into_cache(self) -> AgentCache {
        self.cache
    }

    /// Run the agent loop to completion or max steps.
    ///
    /// # Arguments
    /// * `goal` - The natural language goal for the agent.
    /// * `workflow` - The workflow to build nodes into.
    /// * `mcp` - MCP client for tool execution.
    /// * `variant_context` - Optional context about the current variant.
    /// * `mcp_tools` - Pre-fetched MCP tool definitions in OpenAI format.
    pub(crate) async fn run(
        &mut self,
        goal: String,
        workflow: Workflow,
        mcp: &(impl Mcp + ?Sized),
        variant_context: Option<&str>,
        mcp_tools: Vec<Value>,
    ) -> Result<AgentState> {
        self.state = AgentState::new(workflow);

        // Build the system prompt
        let mut system_text = prompt::system_prompt(&goal);
        if let Some(ctx) = variant_context {
            system_text.push_str(&format!("\n\nVariant context: {}", ctx));
        }
        self.messages = vec![Message::system(system_text)];

        // Build the tool list: MCP tools + agent_done + agent_replan
        let mut tools = mcp_tools.clone();
        tools.push(prompt::agent_done_tool());
        tools.push(prompt::agent_replan_tool());

        info!(goal = %goal, max_steps = self.config.max_steps, "Agent starting");

        let mut previous_result: Option<String> = None;

        for step_index in 0..self.config.max_steps {
            if self.state.completed {
                break;
            }

            // 1. Observe: fetch current page elements
            let elements = self.fetch_elements(mcp).await;

            // Check for page transition
            if step_index > 0 {
                let prev_elements = self
                    .state
                    .steps
                    .last()
                    .map(|s| s.elements.as_slice())
                    .unwrap_or(&[]);
                if transition::detect_transition(prev_elements, &elements) {
                    info!(step = step_index, "Page transition detected");
                }
            }

            // 2. Check cache for a previously seen decision — replay if hit
            if self.config.use_cache {
                if let Some(cached) = self.cache.lookup(&goal, &elements) {
                    let cached_tool = cached.tool_name.clone();
                    let cached_args = cached.arguments.clone();
                    debug!(
                        tool = %cached_tool,
                        hits = cached.hit_count,
                        "Cache hit — replaying cached decision"
                    );

                    match mcp.call_tool(&cached_tool, Some(cached_args.clone())).await {
                        Ok(result) if !result.is_error.unwrap_or(false) => {
                            let result_text = extract_result_text(&result);
                            let command = AgentCommand::ToolCall {
                                tool_name: cached_tool.clone(),
                                arguments: cached_args.clone(),
                                tool_call_id: format!("cache-{}", step_index),
                            };

                            if self.config.build_workflow {
                                self.add_workflow_node(&cached_tool, &cached_args, &mcp_tools);
                            }

                            let step = AgentStep {
                                index: step_index,
                                elements: elements.clone(),
                                command,
                                outcome: StepOutcome::Success(result_text.clone()),
                                page_url: self.state.current_url.clone(),
                            };
                            self.state.steps.push(step);
                            self.state.consecutive_errors = 0;
                            previous_result = Some(result_text);
                            continue;
                        }
                        Ok(result) => {
                            let err_text = extract_result_text(&result);
                            debug!(error = %err_text, "Cached decision returned error, falling through to LLM");
                        }
                        Err(e) => {
                            debug!(error = %e, "Cached decision execution failed, falling through to LLM");
                        }
                    }
                }
            }

            // 3. Build the step observation message
            let step_msg = prompt::step_message(
                step_index,
                &elements,
                &self.state.current_url,
                previous_result.as_deref(),
            );
            self.messages.push(Message::user(step_msg));

            // 4. Context compaction if needed
            let token_budget = 8000; // Conservative default
            if let Some(compacted) =
                context::compact_step_summaries(&self.messages, &self.state.steps, token_budget, 3)
            {
                debug!(
                    before = self.messages.len(),
                    after = compacted.len(),
                    "Context compacted"
                );
                self.messages = compacted;
            }

            // 5. Call the LLM
            let response = self
                .llm
                .chat(self.messages.clone(), Some(tools.clone()))
                .await
                .context("Agent LLM call failed")?;

            let choice = response
                .choices
                .into_iter()
                .next()
                .context("No choices in LLM response")?;

            // 6. Parse and execute the response
            let (command, outcome) = self
                .execute_response(&choice.message, mcp, &goal, &elements, &mcp_tools)
                .await?;

            // Update state
            let step = AgentStep {
                index: step_index,
                elements: elements.clone(),
                command: command.clone(),
                outcome: outcome.clone(),
                page_url: self.state.current_url.clone(),
            };
            self.state.steps.push(step);

            // Handle outcome
            match &outcome {
                StepOutcome::Success(result_text) => {
                    self.state.consecutive_errors = 0;
                    previous_result = Some(result_text.clone());

                    // Cache the successful decision
                    if self.config.use_cache {
                        if let AgentCommand::ToolCall {
                            tool_name,
                            arguments,
                            ..
                        } = &command
                        {
                            self.cache.store(
                                &goal,
                                &elements,
                                tool_name.clone(),
                                arguments.clone(),
                            );
                        }
                    }

                    // Build workflow node
                    if self.config.build_workflow {
                        if let AgentCommand::ToolCall {
                            tool_name,
                            arguments,
                            ..
                        } = &command
                        {
                            self.add_workflow_node(tool_name, arguments, &mcp_tools);
                        }
                    }
                }
                StepOutcome::Error(err) => {
                    self.state.consecutive_errors += 1;
                    previous_result = Some(format!("Error: {}", err));

                    let action = recovery::recovery_strategy(
                        self.state.consecutive_errors,
                        self.config.max_consecutive_errors,
                    );
                    match action {
                        RecoveryAction::Abort => {
                            warn!(
                                errors = self.state.consecutive_errors,
                                "Too many consecutive errors, aborting"
                            );
                            break;
                        }
                        RecoveryAction::Retry => {
                            debug!("Recovery: retrying same action");
                        }
                        RecoveryAction::ReObserve => {
                            debug!("Recovery: re-observing page");
                        }
                    }
                }
                StepOutcome::Done(summary) => {
                    self.state.completed = true;
                    self.state.summary = Some(summary.clone());
                    previous_result = None;
                    info!(summary = %summary, "Agent completed goal");
                }
                StepOutcome::Replan(reason) => {
                    previous_result = Some(format!("Replan requested: {}", reason));
                    warn!(reason = %reason, "Agent requested replan");
                }
            }

            // Add the assistant's response to the conversation
            if let Some(tool_calls) = &choice.message.tool_calls {
                self.messages
                    .push(Message::assistant_tool_calls(tool_calls.clone()));
                // Add tool result message
                if let Some(tc) = tool_calls.first() {
                    let result_text = previous_result.as_deref().unwrap_or("ok");
                    self.messages
                        .push(Message::tool_result(&tc.id, result_text));
                }
            } else if let Some(text) = choice.message.content_text() {
                self.messages.push(Message::assistant(text));
            }
        }

        if !self.state.completed {
            warn!(
                steps = self.state.steps.len(),
                "Agent reached max steps without completing"
            );
        }

        // Return state by swapping in a fresh one
        let final_state = std::mem::replace(
            &mut self.state,
            AgentState::new(Workflow::new("Agent Workflow")),
        );
        Ok(final_state)
    }

    /// Fetch interactive elements from the current page via MCP.
    async fn fetch_elements(&mut self, mcp: &(impl Mcp + ?Sized)) -> Vec<CdpFindElementMatch> {
        // Try cdp_find_elements first (structured data)
        if mcp.has_tool("cdp_find_elements") {
            match mcp
                .call_tool("cdp_find_elements", Some(serde_json::json!({})))
                .await
            {
                Ok(result) => {
                    let text = extract_result_text(&result);
                    if let Ok(parsed) = serde_json::from_str::<CdpFindElementsResponse>(&text) {
                        self.state.current_url = parsed.page_url;
                        return parsed.matches;
                    }
                    debug!("Failed to parse cdp_find_elements response, falling back");
                }
                Err(e) => {
                    debug!(error = %e, "cdp_find_elements failed, falling back");
                }
            }
        }

        Vec::new()
    }

    /// Parse the LLM response and execute the chosen action.
    async fn execute_response(
        &self,
        message: &clickweave_llm::Message,
        mcp: &(impl Mcp + ?Sized),
        _goal: &str,
        _elements: &[CdpFindElementMatch],
        _mcp_tools: &[Value],
    ) -> Result<(AgentCommand, StepOutcome)> {
        // Check for tool calls
        if let Some(tool_calls) = &message.tool_calls {
            if let Some(tc) = tool_calls.first() {
                let args: Value = serde_json::from_str(&tc.function.arguments)
                    .unwrap_or(Value::Object(serde_json::Map::new()));

                // Handle pseudo-tools
                match tc.function.name.as_str() {
                    "agent_done" => {
                        let summary = args["summary"]
                            .as_str()
                            .unwrap_or("Goal completed")
                            .to_string();
                        return Ok((
                            AgentCommand::Done {
                                summary: summary.clone(),
                            },
                            StepOutcome::Done(summary),
                        ));
                    }
                    "agent_replan" => {
                        let reason = args["reason"]
                            .as_str()
                            .unwrap_or("Unknown reason")
                            .to_string();
                        return Ok((
                            AgentCommand::Replan {
                                reason: reason.clone(),
                            },
                            StepOutcome::Replan(reason),
                        ));
                    }
                    _ => {}
                }

                // Execute the MCP tool
                let command = AgentCommand::ToolCall {
                    tool_name: tc.function.name.clone(),
                    arguments: args.clone(),
                    tool_call_id: tc.id.clone(),
                };

                match mcp.call_tool(&tc.function.name, Some(args)).await {
                    Ok(result) => {
                        let is_error = result.is_error.unwrap_or(false);
                        let text = extract_result_text(&result);
                        if is_error {
                            Ok((command, StepOutcome::Error(text)))
                        } else {
                            Ok((command, StepOutcome::Success(text)))
                        }
                    }
                    Err(e) => Ok((command, StepOutcome::Error(e.to_string()))),
                }
            } else {
                Ok((
                    AgentCommand::TextOnly {
                        text: "Empty tool calls".to_string(),
                    },
                    StepOutcome::Error("LLM returned empty tool calls".to_string()),
                ))
            }
        } else {
            // Text-only response (no tool call)
            let text = message.content_text().unwrap_or("No response").to_string();
            Ok((
                AgentCommand::TextOnly { text: text.clone() },
                StepOutcome::Error(format!("LLM did not call a tool: {}", text)),
            ))
        }
    }

    /// Add a workflow node for the executed tool call.
    fn add_workflow_node(&mut self, tool_name: &str, arguments: &Value, known_tools: &[Value]) {
        let node_type = match tool_invocation_to_node_type(tool_name, arguments, known_tools) {
            Ok(nt) => nt,
            Err(e) => {
                debug!(error = %e, tool = tool_name, "Could not map tool to node type");
                return;
            }
        };

        let position = Position {
            x: 0.0,
            y: (self.state.workflow.nodes.len() as f32) * 120.0,
        };
        let node = Node::new(node_type, position, tool_name, "");
        let node_id = node.id;
        self.state.workflow.nodes.push(node);

        // Connect to previous node
        if let Some(prev_id) = self.state.last_node_id {
            self.state.workflow.edges.push(Edge {
                from: prev_id,
                to: node_id,
            });
        }

        self.state.last_node_id = Some(node_id);
    }
}
