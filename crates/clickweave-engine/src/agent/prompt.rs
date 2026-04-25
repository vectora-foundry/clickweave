//! Prompt construction for the state-spine agent runner.
//!
//! The system prompt is built once per run and never re-rendered, preserving
//! prompt-cache hits on the stable prefix (D6). The per-turn user message
//! composes the harness-rendered state block with the current observation.
//!
//! `truncate_summary` is preserved for VLM completion check paths.
//!
//! Phase 2a: this module is dormant — nothing in the live runner imports it.
//! Wiring lands in Phase 3 (cutover), at which point the old `prompt.rs` is
//! deleted and this file is renamed `prompt.rs`.

#![allow(dead_code)] // Phase 2a: module is dormant; live consumers land in Phase 3 cutover.

use clickweave_mcp::Tool;
use serde_json::{Value, json};

use crate::agent::render::render_step_input;
use crate::agent::task_state::TaskState;
use crate::agent::world_model::WorldModel;

const SYSTEM_PROMPT_HEADER: &str = r#"You are Clickweave, an agent that automates desktop and browser workflows via MCP tools.

You operate on a harness-owned world model and task state. Each turn you receive:
1. A `<world_model>` block describing the environment (apps, windows, pages, elements, snapshots, uncertainty).
2. A `<task_state>` block describing your current goal, subgoal stack, active watch slots, and recorded hypotheses.
3. An optional observation returned by the previous tool.

Each turn you respond with a structured JSON object containing:
- `mutations`: zero or more task-state mutations (`push_subgoal`, `complete_subgoal`, `set_watch_slot`, `clear_watch_slot`, `record_hypothesis`, `refute_hypothesis`).
- `action`: exactly one of:
  - `{ "kind": "tool_call", "tool_name": "...", "arguments": {...}, "tool_call_id": "..." }`
  - `{ "kind": "agent_done", "summary": "..." }`
  - `{ "kind": "agent_replan", "reason": "..." }`

Rules:
- The `phase` field in `<task_state>` is harness-inferred. Do not try to set it yourself.
- Uid prefixes signal dispatch family: `a<N>` -> native AX (use `ax_click`/`ax_set_value`/`ax_select`); `d<N>` -> CDP (use `cdp_click`/`cdp_fill`).
- Prefer `cdp_find_elements` for targeted CDP discovery; use `cdp_take_dom_snapshot` only when you need the full page structure.
- When CDP is unavailable (native apps), use `take_ax_snapshot` and native action tools.
- Observation-only tools do not require approval; destructive tools may require approval from the operator.
"#;

/// Build the stable system prompt for the state-spine runner.
///
/// Stability is critical: this string is the prompt-cache prefix for every
/// turn of every run, so it must not embed run-specific data (goal, variant
/// context, timestamps). Variant context lands in `messages[1]` at the user
/// layer (D18).
pub fn build_system_prompt(tools: &[Tool]) -> String {
    let mut out = String::from(SYSTEM_PROMPT_HEADER);
    out.push_str("\n\nAvailable tools:\n");
    for t in tools {
        out.push_str("- ");
        out.push_str(&t.name);
        if let Some(desc) = &t.description
            && !desc.is_empty()
        {
            out.push_str(": ");
            out.push_str(desc);
        }
        out.push('\n');
    }
    out
}

/// Build the per-turn user message. State block first (above the observation),
/// so the LLM reads world + task state before reacting to the observation.
///
/// `retrieved` is the optional Spec 2 episodic-memory result list. When
/// non-empty, a `<retrieved_recoveries>` sibling block is spliced in
/// after the state block and before the observation so the LLM sees
/// remembered recoveries before reacting to the new observation (D23).
pub fn build_user_turn_message(
    wm: &WorldModel,
    ts: &TaskState,
    current_step: usize,
    observation_text: &str,
    retrieved: &[crate::agent::episodic::RetrievedEpisode],
) -> String {
    let mut out = render_step_input(wm, ts, current_step);

    let recoveries_block =
        crate::agent::episodic::render::render_retrieved_recoveries_block(retrieved);
    if !recoveries_block.is_empty() {
        out.push_str(&recoveries_block);
        out.push('\n');
    }

    if !observation_text.is_empty() {
        out.push_str("\n<observation>\n");
        out.push_str(observation_text);
        out.push_str("\n</observation>\n");
    }
    out
}

// --- ported from prompt.rs ---

/// Truncate text to `max_chars`, snapping to a character boundary.
/// Returns the original text if it fits within the limit.
///
/// Copied verbatim from `prompt.rs` so the VLM completion path can switch
/// over cleanly at Phase 3 cutover. The original copy remains in `prompt.rs`
/// as long as the old runner imports it.
pub fn truncate_summary(text: &str, max_chars: usize) -> String {
    if text.len() <= max_chars {
        return text.to_string();
    }
    let end = text.floor_char_boundary(max_chars);
    format!("{}...", &text[..end])
}

/// Tool definition for the agent_done pseudo-tool.
///
/// Ported verbatim from the legacy `prompt.rs` — the state-spine runner
/// appends this (and `agent_replan_tool`) to the MCP tool list each turn so
/// the LLM sees the completion / replan actions as callable tools.
pub fn agent_done_tool() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": "agent_done",
            "description": "Declare the goal as complete. Call this when you have successfully achieved the objective.",
            "parameters": {
                "type": "object",
                "properties": {
                    "summary": {
                        "type": "string",
                        "description": "A brief summary of what was accomplished."
                    }
                },
                "required": ["summary"]
            }
        }
    })
}

/// Tool definition for the agent_replan pseudo-tool.
///
/// Ported verbatim from the legacy `prompt.rs`.
pub fn agent_replan_tool() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": "agent_replan",
            "description": "Request a re-plan when the current approach seems stuck or the goal appears unreachable.",
            "parameters": {
                "type": "object",
                "properties": {
                    "reason": {
                        "type": "string",
                        "description": "Why the current approach is not working."
                    }
                },
                "required": ["reason"]
            }
        }
    })
}

#[cfg(test)]
mod state_spine_prompt_tests {
    use super::*;
    use crate::agent::task_state::TaskState;
    use crate::agent::world_model::WorldModel;

    #[test]
    fn system_prompt_is_stable_across_calls_with_same_tool_list() {
        let tools: Vec<Tool> = vec![];
        let a = build_system_prompt(&tools);
        let b = build_system_prompt(&tools);
        assert_eq!(
            a, b,
            "system prompt must be deterministic for cache stability"
        );
    }

    #[test]
    fn system_prompt_does_not_contain_variant_context() {
        // D18: variant context now lives in messages[1], not messages[0].
        let tools: Vec<Tool> = vec![];
        let s = build_system_prompt(&tools);
        assert!(!s.contains("Variant context"));
    }

    #[test]
    fn system_prompt_lists_tools_with_descriptions() {
        let tools = vec![
            Tool {
                name: "cdp_click".to_string(),
                description: Some("Click a CDP-backed element".to_string()),
                input_schema: serde_json::json!({}),
                annotations: None,
            },
            Tool {
                name: "ax_click".to_string(),
                description: None,
                input_schema: serde_json::json!({}),
                annotations: None,
            },
        ];
        let s = build_system_prompt(&tools);
        assert!(s.contains("- cdp_click: Click a CDP-backed element"));
        assert!(s.contains("- ax_click\n"));
    }

    #[test]
    fn user_turn_contains_state_block_and_observation() {
        let wm = WorldModel::default();
        let ts = TaskState::new("ship it".to_string());
        let out = build_user_turn_message(&wm, &ts, 3, "observation text here", &[]);
        assert!(out.contains("<world_model>"));
        assert!(out.contains("<task_state>"));
        assert!(out.contains("observation text here"));
        // State block must appear before the observation.
        let wm_end = out.find("</world_model>").unwrap();
        let obs_start = out.find("observation text here").unwrap();
        assert!(
            wm_end < obs_start,
            "state block must precede the observation"
        );
    }

    #[test]
    fn user_turn_without_observation_omits_observation_tag() {
        let wm = WorldModel::default();
        let ts = TaskState::new("ship it".to_string());
        let out = build_user_turn_message(&wm, &ts, 0, "", &[]);
        assert!(out.contains("<world_model>"));
        assert!(!out.contains("<observation>"));
    }

    #[test]
    fn truncate_summary_short_text_unchanged() {
        assert_eq!(truncate_summary("hello", 10), "hello");
    }

    #[test]
    fn truncate_summary_long_text_truncated() {
        let long = "a".repeat(200);
        let result = truncate_summary(&long, 50);
        assert!(result.len() < 60);
        assert!(result.ends_with("..."));
    }

    #[test]
    fn truncate_summary_multibyte_snaps_to_boundary() {
        // Multi-byte char must not be split mid-sequence.
        let text = "café!";
        let result = truncate_summary(text, 4);
        assert!(result.ends_with("..."));
    }

    #[test]
    fn agent_done_tool_has_required_fields() {
        let tool = agent_done_tool();
        assert_eq!(tool["function"]["name"], "agent_done");
        let required = tool["function"]["parameters"]["required"]
            .as_array()
            .unwrap();
        assert!(required.iter().any(|r| r == "summary"));
    }

    #[test]
    fn agent_replan_tool_has_required_fields() {
        let tool = agent_replan_tool();
        assert_eq!(tool["function"]["name"], "agent_replan");
        let required = tool["function"]["parameters"]["required"]
            .as_array()
            .unwrap();
        assert!(required.iter().any(|r| r == "reason"));
    }
}
