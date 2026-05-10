use super::*;

/// The one action an `AgentTurn` must carry (D10).
///
/// `ToolCall` usually dispatches to MCP; harness-local observation pseudo-tools
/// such as `get_current_datetime` are intercepted by `McpToolExecutor`.
/// `AgentDone` / `AgentReplan` are harness-local pseudo-tools that never reach
/// MCP.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AgentAction {
    ToolCall {
        tool_name: String,
        arguments: serde_json::Value,
        tool_call_id: String,
    },
    AgentDone {
        summary: String,
    },
    AgentReplan {
        reason: String,
    },
    /// Replay a procedural skill listed in the previous turn's
    /// `<applicable_skills>` block. The harness expands the skill's
    /// recorded action sketch through the same dispatch helper as live
    /// tool calls so the safety surface is identical.
    InvokeSkill {
        skill_id: String,
        version: u32,
        parameters: serde_json::Value,
    },
    /// Apply a structural patch to an on-disk skill. Synthesized by
    /// `parse_agent_turn` when the LLM calls one of the three named
    /// `skill_patch_*` pseudo-tools. The harness dispatches this as a
    /// pure in-memory + disk write without going through MCP.
    SkillPatch {
        /// The synthesized patch. `None` when synthesis failed — the
        /// run_turn arm degrades to an informational `AgentReplan` rather
        /// than panicking so a malformed patch call cannot take the run
        /// down.
        patch: Option<crate::agent::skills::SkillPatch>,
        /// The original pseudo-tool name (`skill_patch_rebind_target`, etc.)
        /// carried so `run_turn` can name the operation in the result text.
        tool_name: String,
        /// Parse error when `patch` is `None`.
        parse_error: Option<String>,
    },
}

/// Batched single-pass agent output: task-state mutations followed by one
/// action. Mutations apply in order before the action dispatches.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AgentTurn {
    pub mutations: Vec<TaskStateMutation>,
    pub action: AgentAction,
}

/// Outcome of a single `StateRunner::run_turn` call — what the caller needs
/// to drive the next iteration.
#[derive(Debug, Clone)]
pub enum TurnOutcome {
    /// Tool call was dispatched; `tool_body` is the successful result text.
    ToolSuccess {
        tool_name: String,
        tool_body: String,
    },
    /// Tool call was dispatched; tool returned an error.
    ToolError { tool_name: String, error: String },
    /// Agent signaled completion.
    Done { summary: String },
    /// Agent requested replan.
    Replan { reason: String },
}

/// Executes an MCP tool call and returns either its successful body or an
/// error message. Integration tests stub this with a deterministic sequence;
/// Phase 3 cutover will bind it to the real `McpClient`.
#[async_trait::async_trait]
pub trait ToolExecutor: Send + Sync {
    async fn call_tool(
        &self,
        tool_name: &str,
        arguments: &serde_json::Value,
    ) -> Result<String, String>;
}

/// Parse a raw LLM response `Message` into an `AgentTurn` carrying
/// `0..N` task-state mutations followed by exactly one action.
///
/// We accept the turn via OpenAI-style `tool_calls`, which the LLM
/// emits as an ordered array. Each call is classified by name:
///
/// - **Mutation pseudo-tools** (`push_subgoal`, `complete_subgoal`,
///   `set_watch_slot`, `clear_watch_slot`, `record_hypothesis`,
///   `refute_hypothesis`) parse into `TaskStateMutation` values
///   regardless of position. Malformed args produce a per-call warning
///   but never abort the turn — a single bad mutation cannot poison
///   the action.
/// - **Action pseudo-tools** (`agent_done`, `agent_replan`) and any
///   other tool name become an `AgentAction`. The first action-shaped
///   call wins; subsequent action calls are dropped, since exactly one
///   action runs per turn. Mutations after the action are still
///   preserved — apply order is enforced by `apply_mutations`, not by
///   tool-call order.
///
/// If only mutations are present (the LLM forgot to choose an action),
/// the result is an `AgentReplan` with a self-describing reason so the
/// next turn re-observes instead of aborting.
///
/// Text-only replies (no `tool_calls`) also map to
/// `AgentAction::AgentReplan` with the assistant's raw text as the
/// reason — matches the legacy "no tool call" recovery hook.
pub fn parse_agent_turn(message: &Message) -> anyhow::Result<AgentTurn> {
    use crate::agent::prompt::{is_mutation_tool_name, is_skill_patch_tool_name};
    use crate::agent::skills::SkillPatch;

    if let Some(tool_calls) = message.tool_calls.as_ref()
        && !tool_calls.is_empty()
    {
        let mut mutations: Vec<TaskStateMutation> = Vec::new();
        let mut action: Option<AgentAction> = None;

        for tc in tool_calls {
            let name = tc.function.name.as_str();
            let args = &tc.function.arguments;

            if is_mutation_tool_name(name) {
                match parse_mutation_call(name, args) {
                    Ok(m) => mutations.push(m),
                    Err(reason) => tracing::warn!(
                        tool = name,
                        error = %reason,
                        "state-spine: dropping malformed mutation pseudo-tool call"
                    ),
                }
                continue;
            }

            // Action — keep only the first one; exactly one action runs per turn.
            if action.is_some() {
                tracing::warn!(
                    tool = name,
                    "state-spine: ignoring extra action call after first action was claimed"
                );
                continue;
            }

            action = Some(match name {
                "agent_done" => {
                    let summary = args
                        .get("summary")
                        .and_then(Value::as_str)
                        .unwrap_or("Goal completed")
                        .to_string();
                    AgentAction::AgentDone { summary }
                }
                "agent_replan" => {
                    let reason = args
                        .get("reason")
                        .and_then(Value::as_str)
                        .unwrap_or("Unknown reason")
                        .to_string();
                    AgentAction::AgentReplan { reason }
                }
                "invoke_skill" => {
                    let skill_id = args
                        .get("skill_id")
                        .and_then(Value::as_str)
                        .map(str::to_string);
                    let version = args.get("version").and_then(Value::as_u64);
                    match (skill_id, version) {
                        (Some(skill_id), Some(version)) => match u32::try_from(version) {
                            Ok(version) => {
                                let parameters =
                                    args.get("parameters").cloned().unwrap_or(Value::Null);
                                AgentAction::InvokeSkill {
                                    skill_id,
                                    version,
                                    parameters,
                                }
                            }
                            Err(_) => {
                                tracing::warn!("state-spine: invoke_skill version out of range");
                                AgentAction::AgentReplan {
                                    reason: "invoke_skill version out of range".to_string(),
                                }
                            }
                        },
                        _ => {
                            tracing::warn!(
                                "state-spine: invoke_skill missing required fields — replanning"
                            );
                            AgentAction::AgentReplan {
                                reason: "invoke_skill missing required fields".to_string(),
                            }
                        }
                    }
                }
                name if is_skill_patch_tool_name(name) => {
                    let (patch, parse_error) = match name {
                        "skill_patch_rebind_target" => {
                            match SkillPatch::from_rebind_target_args(args) {
                                Ok(p) => (Some(p), None),
                                Err(e) => {
                                    tracing::warn!(
                                        tool = name,
                                        error = %e,
                                        "state-spine: malformed skill_patch_rebind_target call"
                                    );
                                    (None, Some(e))
                                }
                            }
                        }
                        "skill_patch_reorder_sections" => {
                            match SkillPatch::from_reorder_sections_args(args) {
                                Ok(p) => (Some(p), None),
                                Err(e) => {
                                    tracing::warn!(
                                        tool = name,
                                        error = %e,
                                        "state-spine: malformed skill_patch_reorder_sections call"
                                    );
                                    (None, Some(e))
                                }
                            }
                        }
                        "skill_patch_promote_to_variable" => {
                            match SkillPatch::from_promote_to_variable_args(args) {
                                Ok(p) => (Some(p), None),
                                Err(e) => {
                                    tracing::warn!(
                                        tool = name,
                                        error = %e,
                                        "state-spine: malformed skill_patch_promote_to_variable call"
                                    );
                                    (None, Some(e))
                                }
                            }
                        }
                        // Safety: `is_skill_patch_tool_name` is the gate
                        // so this arm is unreachable in practice.
                        _ => unreachable!("unexpected skill_patch tool name: {name}"),
                    };
                    AgentAction::SkillPatch {
                        patch,
                        tool_name: name.to_string(),
                        parse_error,
                    }
                }
                _ => AgentAction::ToolCall {
                    tool_name: name.to_string(),
                    arguments: args.clone(),
                    tool_call_id: tc.id.clone(),
                },
            });
        }

        let action = action.unwrap_or_else(|| AgentAction::AgentReplan {
            reason: NO_ACTION_MUTATION_ONLY_REASON.to_string(),
        });

        return Ok(AgentTurn { mutations, action });
    }

    // Text-only response: treat as a replan request so the run re-observes
    // next turn instead of aborting. Mirrors the legacy "no tool call"
    // recovery hook.
    let reason = message
        .content_text()
        .map(str::to_owned)
        .unwrap_or_else(|| "LLM returned no tool call and no text".to_string());
    Ok(AgentTurn {
        mutations: Vec::new(),
        action: AgentAction::AgentReplan { reason },
    })
}

/// Parse a single mutation-shaped tool call (`push_subgoal`,
/// `complete_subgoal`, `set_watch_slot`, `clear_watch_slot`,
/// `record_hypothesis`, `refute_hypothesis`) into a `TaskStateMutation`.
///
/// Returns a human-readable reason on malformed arguments so the caller
/// can log per-call instead of aborting the whole turn. The strict
/// enforcement (e.g. "watch slot not set") happens later in
/// `TaskState::apply` and surfaces via `apply_mutations`'s warnings vec.
fn parse_mutation_call(name: &str, args: &Value) -> Result<TaskStateMutation, String> {
    use crate::agent::task_state::WatchSlotName;

    fn required_str<'a>(args: &'a Value, key: &str) -> Result<&'a str, String> {
        args.get(key)
            .and_then(Value::as_str)
            .ok_or_else(|| format!("missing required string field `{}`", key))
    }

    // Defer enum-tag validation to serde — `WatchSlotName` already
    // declares `#[serde(rename_all = "snake_case")]`, so the same
    // strings the pseudo-tool schema lists are accepted here without
    // a hand-maintained match arm.
    fn watch_slot_name(args: &Value) -> Result<WatchSlotName, String> {
        let raw = args
            .get("name")
            .ok_or_else(|| "missing required string field `name`".to_string())?;
        serde_json::from_value::<WatchSlotName>(raw.clone())
            .map_err(|e| format!("invalid watch slot name: {}", e))
    }

    match name {
        "push_subgoal" => Ok(TaskStateMutation::PushSubgoal {
            text: required_str(args, "text")?.to_string(),
        }),
        "complete_subgoal" => Ok(TaskStateMutation::CompleteSubgoal {
            summary: required_str(args, "summary")?.to_string(),
        }),
        "set_watch_slot" => Ok(TaskStateMutation::SetWatchSlot {
            name: watch_slot_name(args)?,
            note: required_str(args, "note")?.to_string(),
        }),
        "clear_watch_slot" => Ok(TaskStateMutation::ClearWatchSlot {
            name: watch_slot_name(args)?,
        }),
        "record_hypothesis" => Ok(TaskStateMutation::RecordHypothesis {
            text: required_str(args, "text")?.to_string(),
        }),
        "refute_hypothesis" => {
            let idx = args
                .get("index")
                .and_then(Value::as_u64)
                .ok_or_else(|| "missing required non-negative integer field `index`".to_string())?;
            Ok(TaskStateMutation::RefuteHypothesis {
                index: idx as usize,
            })
        }
        _ => Err(format!("not a mutation pseudo-tool: `{}`", name)),
    }
}

/// Adapter that turns any `&dyn Mcp` into the `ToolExecutor` trait expected
/// by `run_turn`. Kept private to `runner.rs` — the plan names this
/// `McpToolExecutor` so later tasks can grep for the anchor.
pub(crate) struct McpToolExecutor<'a, M: Mcp + ?Sized> {
    pub(crate) mcp: &'a M,
}

#[async_trait::async_trait]
impl<M: Mcp + ?Sized> ToolExecutor for McpToolExecutor<'_, M> {
    async fn call_tool(
        &self,
        tool_name: &str,
        arguments: &serde_json::Value,
    ) -> Result<String, String> {
        if tool_name == crate::agent::time_oracle::TOOL_NAME {
            return Ok(crate::agent::time_oracle::current_datetime_json());
        }

        let result = self
            .mcp
            .call_tool(tool_name, Some(arguments.clone()))
            .await
            .map_err(|e| e.to_string())?;
        let text = result
            .content
            .iter()
            .filter_map(|c| c.as_text())
            .collect::<Vec<_>>()
            .join("\n");
        if result.is_error == Some(true) {
            Err(text)
        } else {
            Ok(text)
        }
    }
}

/// Append the assistant's response and its tool result onto the transcript,
/// mirroring the legacy `AgentRunner::append_assistant_message`.
///
/// When the assistant returned `tool_calls`, the transcript gets the
/// assistant message (tool_calls only) plus a matching `tool_result`. When
/// the assistant returned plain text, only the assistant message is
/// appended.
/// Append an assistant tool-call + matching tool-result onto the
/// transcript so the next iteration's LLM call sees what was
/// dispatched. Synthesises the assistant message from the action's own
/// `(tool_call_id, tool_name, arguments)` rather than picking
/// `tool_calls.first()`: when a turn's `tool_calls` array starts with
/// mutation pseudo-tools (e.g. `push_subgoal` then `cdp_click`), the
/// "first call" is a mutation, not the action that actually ran, and
/// attaching the dispatched result to that id breaks action / result
/// causality from the LLM's point of view. Mutations are already
/// reflected in `<task_state>` at the next turn; they do not appear in
/// the transcript here.
///
/// The tool-result's `name` is stamped so `context::compact` can
/// identify stale snapshot-family bodies by the `SNAPSHOT_TOOL_NAMES`
/// set. Without this stamp, production tool-result messages leave
/// `name` unset and the snapshot-drop branch never fires for live
/// runs.
pub(crate) fn append_assistant_and_tool_result(
    messages: &mut Vec<Message>,
    tool_name: &str,
    arguments: &Value,
    tool_call_id: &str,
    previous_result: Option<&str>,
) {
    let tc = clickweave_llm::ToolCall {
        id: tool_call_id.to_string(),
        call_type: clickweave_llm::CallType::Function,
        function: clickweave_llm::FunctionCall {
            name: tool_name.to_string(),
            arguments: arguments.clone(),
        },
    };
    messages.push(Message::assistant_tool_calls(vec![tc]));
    let mut tool_msg = Message::tool_result(tool_call_id, previous_result.unwrap_or("ok"));
    tool_msg.name = Some(tool_name.to_string());
    messages.push(tool_msg);
}
