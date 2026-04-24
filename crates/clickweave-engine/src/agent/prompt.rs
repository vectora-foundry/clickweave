// Task 3a.1: the legacy prompt builders stay live through `AgentRunner` in
// the legacy integration tests; the state-spine runner uses `prompt_spine`
// instead. `prompt::{agent_done_tool, agent_replan_tool}` remain live
// consumers, but the larger helpers (`system_prompt`, `goal_message`,
// `step_message`) only surface through the legacy path until Task 3a.7
// migrates it away.
#![allow(dead_code)]

use clickweave_core::cdp::CdpFindElementMatch;
use serde_json::{Value, json};

use super::types::AgentStep;

/// Maximum character length for previous tool results injected into prompts.
/// Results longer than this are truncated to avoid dominating prompt context.
const MAX_PREVIOUS_RESULT_CHARS: usize = 2000;

/// Build the system prompt for the agent LLM.
///
/// The goal is placed in a dedicated user message (see `goal_message`) rather
/// than here, so that user-controlled text does not occupy the system-level
/// instruction layer.
pub fn system_prompt() -> String {
    r#"You are an autonomous desktop automation agent.

You operate in an observe-act loop:
1. You receive a list of interactive UI elements on the current page.
2. You choose ONE tool to call (click, type_text, press_key, scroll, etc.) or declare done.
3. You receive the tool result and a new observation.

## Rules
- Call exactly ONE tool per turn. Never call multiple tools.
- If the goal is complete, call the `agent_done` tool with a summary.
- If the goal seems unreachable, call the `agent_replan` tool with a reason.
- Be concise in your reasoning. Focus on the next immediate action.

## Launching Apps
When calling `launch_app`, set `background=true` whenever the next planned
action will use a focus-preserving dispatch path:
- **CDP path** (Electron / Chrome apps): `cdp_connect` → `cdp_click` / `cdp_fill` / `cdp_press_key` — these work in the background.
- **AX path** (native macOS apps): `take_ax_snapshot` → `ax_click` / `ax_set_value` / `ax_select` — these also work in the background.

Set `background=false` (or omit it) only when the very next action genuinely
requires the app to be frontmost — e.g. coordinate-based `click`, `type_text`
(without AX), or `find_text` OCR, all of which steal focus.

## How to Interact with Elements
When the observation includes interactive elements with UIDs (like [1_0], [1_1], [2_3], etc.),
use CDP tools to interact with them directly by UID:

- To click: `cdp_click` with `uid` (e.g. uid="1_0")
- To type: `cdp_type_text` with `text`
- To press a key: `cdp_press_key` with `key`

Example: to click a button labeled "Submit" with uid [2_3]:
  → call cdp_click with uid="2_3"

If CDP tools are NOT available, prefer the macOS accessibility path over
coordinate-based clicking whenever both are available:

1. Call `take_ax_snapshot` with the target `app_name`. The response tags
   every element with a uid like `a42g3`.
2. Dispatch against those uids with `ax_click` (buttons, menu items),
   `ax_set_value` (text fields), or `ax_select` (list / outline rows).

The AX dispatch tools do NOT move the mouse cursor and do NOT steal focus
from the frontmost app. Because of that, you must NOT call `focus_window`
before `take_ax_snapshot` / `ax_click` / `ax_set_value` / `ax_select` — the
snapshot and dispatch work against background windows just as well as the
foreground one, and focusing the app only creates an unnecessary visual
disruption. Snapshot uids are generation-scoped, so always re-snapshot
immediately before each AX dispatch call.

Only fall back to cursor-based `find_text` + `click` when AX is unavailable
(e.g. a cross-platform tool call, Windows target, or an app whose AX tree
does not expose the required element). That path IS focus-sensitive, so
call `focus_window` first before using coordinate-based click/type.

Example (AX path, no focus): take_ax_snapshot(app_name="Calculator") →
  ax_click(uid="a17g2")  — button "5"

Example (coordinate path): find_text returns `{"x": 553, "y": 1082, ...}`
  for "Message" → focus_window(app_name="...") → click(x=553, y=1082).

NEVER call `click` with empty arguments or guessed coordinates. If find_text
returned coordinates, pass those exact numbers through to `click`. NEVER use
`take_screenshot` when elements are listed in the observation.

When NO elements are listed, prefer `take_ax_snapshot` on macOS, or fall
back to `take_screenshot` + `find_text` to locate elements by name."#
        .to_string()
}

/// Build a user message containing the goal.
///
/// Separated from the system prompt so that user-controlled text stays in the
/// user-message layer rather than the system-instruction layer.
pub fn goal_message(goal: &str) -> String {
    format!("## Goal\n{}", goal)
}

/// Build a user message for a single observation step.
pub fn step_message(
    step_index: usize,
    elements: &[CdpFindElementMatch],
    page_url: &str,
    previous_result: Option<&str>,
) -> String {
    let mut msg = String::new();

    if let Some(result) = previous_result {
        let truncated = if result.len() > MAX_PREVIOUS_RESULT_CHARS {
            let end = result.floor_char_boundary(MAX_PREVIOUS_RESULT_CHARS);
            format!(
                "{}... [truncated, {} chars total]",
                &result[..end],
                result.len()
            )
        } else {
            result.to_string()
        };
        msg.push_str(&format!("## Previous Action Result\n{}\n\n", truncated));
    }

    msg.push_str(&format!(
        "## Observation (Step {})\nPage: {}\n\n",
        step_index, page_url
    ));

    if elements.is_empty() {
        msg.push_str("No interactive elements found on the page.\n");
    } else {
        msg.push_str(&format_elements(elements));
    }

    msg.push_str("\nChoose your next action.");
    msg
}

/// Format a list of page elements into a readable text block.
pub fn format_elements(elements: &[CdpFindElementMatch]) -> String {
    let mut out = String::new();
    out.push_str("### Interactive Elements\n");

    for el in elements {
        let disabled_marker = if el.disabled { " [disabled]" } else { "" };
        let parent_info = match (&el.parent_role, &el.parent_name) {
            (Some(role), Some(name)) => format!(" (in {} \"{}\")", role, name),
            (Some(role), None) => format!(" (in {})", role),
            _ => String::new(),
        };

        out.push_str(&format!(
            "- [{}] {} \"{}\" <{}>{}{}\n",
            el.uid, el.role, el.label, el.tag, parent_info, disabled_marker
        ));
    }

    out
}

/// Tool definition for the agent_done pseudo-tool.
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

/// Build a compact summary of previous steps for context window management.
pub fn summarize_steps(steps: &[AgentStep]) -> String {
    let mut out = String::new();
    out.push_str("## Previous Steps Summary\n");
    for step in steps {
        let action = match &step.command {
            super::types::AgentCommand::ToolCall { tool_name, .. } => {
                format!("called {}", tool_name)
            }
            super::types::AgentCommand::Done { summary } => {
                format!("done: {}", summary)
            }
            super::types::AgentCommand::Replan { reason } => {
                format!("replan: {}", reason)
            }
            super::types::AgentCommand::TextOnly { .. } => "text response".to_string(),
        };
        let outcome = match &step.outcome {
            super::types::StepOutcome::Success(text) => {
                let truncated = if text.len() > 100 {
                    let end = text.floor_char_boundary(100);
                    format!("{}...", &text[..end])
                } else {
                    text.clone()
                };
                format!("ok: {}", truncated)
            }
            super::types::StepOutcome::Error(e) => format!("error: {}", e),
            super::types::StepOutcome::Done(s) => format!("done: {}", s),
            super::types::StepOutcome::Replan(r) => format!("replan: {}", r),
        };
        out.push_str(&format!("Step {}: {} -> {}\n", step.index, action, outcome));
    }
    out
}

/// Truncate text to `max_chars`, snapping to a character boundary.
/// Returns the original text if it fits within the limit.
pub fn truncate_summary(text: &str, max_chars: usize) -> String {
    if text.len() <= max_chars {
        return text.to_string();
    }
    let end = text.floor_char_boundary(max_chars);
    format!("{}...", &text[..end])
}

#[cfg(test)]
mod tests {
    use super::*;

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
        // 3 bytes per char × 4 = 12 bytes; truncate at 5 snaps to char boundary
        let text = "café!"; // 'é' is 2 bytes
        let result = truncate_summary(text, 4);
        assert!(result.ends_with("..."));
        // Should not panic or split a multibyte char
    }

    #[test]
    fn system_prompt_contains_instructions() {
        let prompt = system_prompt();
        assert!(prompt.contains("autonomous desktop automation agent"));
        assert!(prompt.contains("agent_done"));
        assert!(prompt.contains("agent_replan"));
    }

    #[test]
    fn goal_message_contains_goal_text() {
        let msg = goal_message("Open the calculator app");
        assert!(msg.contains("Open the calculator app"));
        assert!(msg.contains("Goal"));
    }

    #[test]
    fn step_message_truncates_large_previous_result() {
        let large_result = "x".repeat(5000);
        let msg = step_message(0, &[], "https://example.com", Some(&large_result));
        assert!(msg.contains("[truncated, 5000 chars total]"));
        assert!(msg.len() < 5000);
    }

    #[test]
    fn format_elements_handles_empty() {
        let result = format_elements(&[]);
        assert!(result.contains("Interactive Elements"));
    }

    #[test]
    fn format_elements_renders_entries() {
        let elements = vec![
            CdpFindElementMatch {
                uid: "1_0".to_string(),
                role: "button".to_string(),
                label: "Submit".to_string(),
                tag: "button".to_string(),
                disabled: false,
                parent_role: None,
                parent_name: None,
            },
            CdpFindElementMatch {
                uid: "1_1".to_string(),
                role: "textbox".to_string(),
                label: "Email".to_string(),
                tag: "input".to_string(),
                disabled: true,
                parent_role: Some("form".to_string()),
                parent_name: Some("Login".to_string()),
            },
        ];
        let result = format_elements(&elements);
        assert!(result.contains("[1_0] button \"Submit\""));
        assert!(result.contains("[disabled]"));
        assert!(result.contains("(in form \"Login\")"));
    }

    #[test]
    fn step_message_includes_previous_result() {
        let msg = step_message(1, &[], "https://example.com", Some("Clicked button"));
        assert!(msg.contains("Previous Action Result"));
        assert!(msg.contains("Clicked button"));
        assert!(msg.contains("Step 1"));
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
