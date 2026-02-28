use crate::{ChatBackend, Message};
use clickweave_core::walkthrough::{
    ActionNodeEntry, TargetCandidate, WalkthroughAction, WalkthroughActionKind,
};
use clickweave_core::{NodeType, Workflow};
use serde_json::Value;
use tracing::info;

/// Result of the walkthrough generalization pass.
pub struct WalkthroughPlanResult {
    pub workflow: Workflow,
    pub warnings: Vec<String>,
    pub action_node_map: Vec<ActionNodeEntry>,
    pub used_fallback: bool,
}

/// Run the LLM generalization pass over a deterministic walkthrough draft.
///
/// On success, returns an enhanced workflow with better names, text-based
/// click targets, and verification nodes. On any failure (LLM, parsing,
/// validation), returns the deterministic draft unchanged.
pub async fn generalize_walkthrough(
    backend: &impl ChatBackend,
    deterministic_draft: &Workflow,
    actions: &[WalkthroughAction],
    mcp_tools_openai: &[Value],
) -> WalkthroughPlanResult {
    use clickweave_core::walkthrough::build_action_node_map;

    // Empty actions → nothing to generalize.
    if actions.is_empty() {
        return WalkthroughPlanResult {
            workflow: deterministic_draft.clone(),
            warnings: vec![],
            action_node_map: vec![],
            used_fallback: true,
        };
    }

    let fallback = || WalkthroughPlanResult {
        workflow: deterministic_draft.clone(),
        warnings: vec!["LLM generalization failed; using deterministic draft.".into()],
        action_node_map: build_action_node_map(actions, deterministic_draft),
        used_fallback: true,
    };

    let system = walkthrough_system_prompt(mcp_tools_openai, actions);
    let user_msg = "Plan a workflow that replays the demonstrated walkthrough.".to_string();

    info!("Generalizing walkthrough: {} actions", actions.len());

    let messages = vec![Message::system(&system), Message::user(&user_msg)];

    let plan_result =
        super::repair::chat_with_repair(backend, "Walkthrough", messages, |content| {
            super::plan::parse_and_build_walkthrough(
                content,
                &deterministic_draft.name,
                mcp_tools_openai,
            )
        })
        .await;

    match plan_result {
        Ok(result) => {
            info!(
                "Walkthrough generalization: {} nodes, {} warnings",
                result.workflow.nodes.len(),
                result.warnings.len(),
            );

            // Build the action_node_map by matching original action order
            // to the new workflow nodes. The LLM should preserve the order,
            // but may add/remove nodes. We match greedily by position.
            let action_node_map = build_enhanced_action_node_map(actions, &result.workflow);

            WalkthroughPlanResult {
                workflow: result.workflow,
                warnings: result.warnings,
                action_node_map,
                used_fallback: false,
            }
        }
        Err(e) => {
            info!("Walkthrough generalization failed: {e}");
            let mut f = fallback();
            f.warnings.push(format!("LLM error: {e}"));
            f
        }
    }
}

// ── Prompt building ─────────────────────────────────────────────

/// Build the action trace section for the walkthrough prompt.
fn format_action_trace(actions: &[WalkthroughAction]) -> String {
    let mut trace = String::new();
    for (i, action) in actions.iter().enumerate() {
        trace.push_str(&format!("Step {}:", i + 1));

        match &action.kind {
            WalkthroughActionKind::LaunchApp { app_name } => {
                trace.push_str(&format!(" Launch \"{app_name}\"\n"));
            }
            WalkthroughActionKind::FocusWindow {
                app_name,
                window_title,
            } => match window_title {
                Some(t) => trace.push_str(&format!(" Focus window \"{t}\" ({app_name})\n")),
                None => trace.push_str(&format!(" Focus \"{app_name}\"\n")),
            },
            WalkthroughActionKind::Click { x, y, .. } => {
                let target_desc = action.target_candidates.iter().find_map(|c| match c {
                    TargetCandidate::AccessibilityLabel { label, .. } => {
                        Some(format!("target=\"{label}\""))
                    }
                    TargetCandidate::OcrText { text } => Some(format!("target=\"{text}\" (OCR)")),
                    _ => None,
                });
                match target_desc {
                    Some(desc) => trace.push_str(&format!(" Click {desc}\n")),
                    None => {
                        trace.push_str(&format!(" Click at ({x:.0}, {y:.0}) — no text target\n"))
                    }
                }
            }
            WalkthroughActionKind::TypeText { text } => {
                trace.push_str(&format!(" Type \"{text}\"\n"));
            }
            WalkthroughActionKind::PressKey { key, modifiers } => {
                if modifiers.is_empty() {
                    trace.push_str(&format!(" Press {key}\n"));
                } else {
                    trace.push_str(&format!(" Press {}+{key}\n", modifiers.join("+")));
                }
            }
            WalkthroughActionKind::Scroll { delta_y } => {
                let dir = if *delta_y < 0.0 { "up" } else { "down" };
                trace.push_str(&format!(" Scroll {dir}\n"));
            }
        }

        if let Some(app) = &action.app_name {
            trace.push_str(&format!("  App: {app}\n"));
        }

        // List all target candidates for clicks.
        if !action.target_candidates.is_empty() {
            let descs: Vec<String> = action
                .target_candidates
                .iter()
                .map(|c| match c {
                    TargetCandidate::AccessibilityLabel { label, role } => match role {
                        Some(r) => format!("AccessibilityLabel \"{label}\" role={r}"),
                        None => format!("AccessibilityLabel \"{label}\""),
                    },
                    TargetCandidate::OcrText { text } => format!("OcrText \"{text}\""),
                    TargetCandidate::ImageCrop { path } => format!("ImageCrop \"{path}\""),
                    TargetCandidate::Coordinates { x, y } => {
                        format!("Coordinates ({x:.0}, {y:.0})")
                    }
                })
                .collect();
            trace.push_str(&format!("  Candidates: [{}]\n", descs.join(", ")));
        }

        trace.push_str(&format!("  Confidence: {:?}\n", action.confidence));

        for w in &action.warnings {
            trace.push_str(&format!("  Warning: {w}\n"));
        }

        trace.push('\n');
    }
    trace
}

/// Build the walkthrough system prompt.
fn walkthrough_system_prompt(tools_json: &[Value], actions: &[WalkthroughAction]) -> String {
    let tool_list = serde_json::to_string_pretty(tools_json).unwrap_or_default();
    let step_types_block = build_step_types();
    let action_trace = format_action_trace(actions);

    let template = include_str!("../../prompts/walkthrough.md");

    template
        .replace("{{tool_list}}", &tool_list)
        .replace("{{step_types}}", &step_types_block)
        .replace("{{action_trace}}", &action_trace)
}

/// Build the step types reference section for the walkthrough prompt.
/// Only includes Tool steps (no AiStep, no control flow for walkthrough v1).
fn build_step_types() -> String {
    r#"Available step types:

1. **Tool** — calls exactly one MCP tool:
   ```json
   {"step_type": "Tool", "tool_name": "<name>", "arguments": {...}, "name": "descriptive label"}
   ```
   The arguments must be valid according to the tool's input schema.

## Verification role

Any read-only Tool step (take_screenshot, find_text) can be marked as a **verification** by adding `"role": "Verification"` and `"expected_outcome": "<description>"` to the step. Use this after key state transitions to assert that the expected result is visible."#
        .to_string()
}

// ── Action → Node mapping ───────────────────────────────────────

/// Build an action→node map for an LLM-enhanced workflow.
///
/// The LLM may add verification nodes or remove redundant steps, so the
/// mapping is not necessarily 1:1. We match by walking both lists and
/// skipping LLM-added nodes (those that don't correspond to any action).
///
/// Heuristic: the LLM preserves action order, so we greedily assign each
/// action to the next node whose tool_name matches the action kind. Nodes
/// that don't match any action are LLM-added (verification, etc).
fn build_enhanced_action_node_map(
    actions: &[WalkthroughAction],
    workflow: &Workflow,
) -> Vec<ActionNodeEntry> {
    let mut map = Vec::new();
    let mut node_idx = 0;

    for action in actions {
        while node_idx < workflow.nodes.len() {
            let node = &workflow.nodes[node_idx];
            if action_matches_node(&action.kind, &node.node_type) {
                map.push(ActionNodeEntry {
                    action_id: action.id,
                    node_id: node.id,
                });
                node_idx += 1;
                break;
            }
            // Skip this node — it's LLM-added (e.g., verification screenshot).
            node_idx += 1;
        }
    }

    map
}

/// Check if a walkthrough action kind matches a workflow node type.
fn action_matches_node(action: &WalkthroughActionKind, node_type: &NodeType) -> bool {
    matches!(
        (action, node_type),
        (
            WalkthroughActionKind::LaunchApp { .. },
            NodeType::FocusWindow(_)
        ) | (
            WalkthroughActionKind::FocusWindow { .. },
            NodeType::FocusWindow(_)
        ) | (WalkthroughActionKind::Click { .. }, NodeType::Click(_))
            | (
                WalkthroughActionKind::TypeText { .. },
                NodeType::TypeText(_)
            )
            | (
                WalkthroughActionKind::PressKey { .. },
                NodeType::PressKey(_)
            )
            | (WalkthroughActionKind::Scroll { .. }, NodeType::Scroll(_))
    )
}

// ── Tests ───────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ChatResponse, Choice};
    use anyhow::Result;
    use clickweave_core::MouseButton;
    use clickweave_core::validate_workflow;
    use clickweave_core::walkthrough::ActionConfidence;
    use std::sync::Mutex;

    struct MockBackend {
        response: Mutex<String>,
    }

    impl MockBackend {
        fn new(response: &str) -> Self {
            Self {
                response: Mutex::new(response.to_string()),
            }
        }
    }

    impl ChatBackend for MockBackend {
        fn model_name(&self) -> &str {
            "mock"
        }

        async fn chat(
            &self,
            _messages: Vec<Message>,
            _tools: Option<Vec<Value>>,
        ) -> Result<ChatResponse> {
            let text = self.response.lock().unwrap().clone();
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

    fn sample_actions() -> Vec<WalkthroughAction> {
        vec![
            WalkthroughAction {
                id: uuid::Uuid::new_v4(),
                kind: WalkthroughActionKind::LaunchApp {
                    app_name: "Calculator".into(),
                },
                app_name: Some("Calculator".into()),
                window_title: None,
                target_candidates: vec![],
                artifact_paths: vec![],
                source_event_ids: vec![],
                confidence: ActionConfidence::High,
                warnings: vec![],
            },
            WalkthroughAction {
                id: uuid::Uuid::new_v4(),
                kind: WalkthroughActionKind::Click {
                    x: 200.0,
                    y: 300.0,
                    button: MouseButton::Left,
                    click_count: 1,
                },
                app_name: Some("Calculator".into()),
                window_title: None,
                target_candidates: vec![
                    TargetCandidate::OcrText { text: "5".into() },
                    TargetCandidate::Coordinates { x: 200.0, y: 300.0 },
                ],
                artifact_paths: vec![],
                source_event_ids: vec![],
                confidence: ActionConfidence::Medium,
                warnings: vec![],
            },
        ]
    }

    fn sample_tools() -> Vec<Value> {
        serde_json::from_str(
            r#"[
            {"type": "function", "function": {"name": "launch_app", "description": "Launch app", "parameters": {"type": "object", "properties": {"app_name": {"type": "string"}}}}},
            {"type": "function", "function": {"name": "click", "description": "Click", "parameters": {"type": "object", "properties": {"target": {"type": "string"}, "x": {"type": "number"}, "y": {"type": "number"}}}}},
            {"type": "function", "function": {"name": "take_screenshot", "description": "Screenshot", "parameters": {"type": "object", "properties": {"app_name": {"type": "string"}}}}}
        ]"#,
        )
        .unwrap()
    }

    #[tokio::test]
    async fn valid_llm_output_produces_enhanced_workflow() {
        let llm_output = r#"{"steps": [
            {"step_type": "Tool", "tool_name": "launch_app", "arguments": {"app_name": "Calculator"}, "name": "Open Calculator"},
            {"step_type": "Tool", "tool_name": "click", "arguments": {"target": "5"}, "name": "Enter first operand"},
            {"step_type": "Tool", "tool_name": "take_screenshot", "arguments": {"app_name": "Calculator"}, "name": "Verify calculator state", "role": "Verification", "expected_outcome": "Calculator displays 5"}
        ]}"#;

        let backend = MockBackend::new(llm_output);
        let actions = sample_actions();
        let deterministic =
            clickweave_core::walkthrough::synthesize_draft(&actions, uuid::Uuid::new_v4(), "test");
        let tools = sample_tools();

        let result = generalize_walkthrough(&backend, &deterministic, &actions, &tools).await;

        assert!(!result.used_fallback);
        assert!(result.workflow.nodes.len() >= 2);
        assert!(validate_workflow(&result.workflow).is_ok());
        // LLM-enhanced names should be used, not the raw action names.
        assert_eq!(result.workflow.nodes[0].name, "Open Calculator");
    }

    #[tokio::test]
    async fn invalid_llm_output_falls_back_to_deterministic_draft() {
        let backend = MockBackend::new("this is not json at all");
        let actions = sample_actions();
        let deterministic =
            clickweave_core::walkthrough::synthesize_draft(&actions, uuid::Uuid::new_v4(), "test");
        let tools = sample_tools();

        let result = generalize_walkthrough(&backend, &deterministic, &actions, &tools).await;

        assert!(result.used_fallback);
        assert_eq!(result.workflow.nodes.len(), deterministic.nodes.len());
        assert!(!result.warnings.is_empty());
    }

    #[tokio::test]
    async fn empty_actions_returns_empty_fallback() {
        let backend = MockBackend::new("{}");
        let deterministic = Workflow::default();
        let tools = sample_tools();

        let result = generalize_walkthrough(&backend, &deterministic, &[], &tools).await;

        assert!(result.used_fallback);
        assert!(result.workflow.nodes.is_empty());
    }

    #[tokio::test]
    async fn action_node_map_maps_actions_to_original_nodes_on_fallback() {
        let backend = MockBackend::new("garbage");
        let actions = sample_actions();
        let deterministic =
            clickweave_core::walkthrough::synthesize_draft(&actions, uuid::Uuid::new_v4(), "test");
        let tools = sample_tools();

        let result = generalize_walkthrough(&backend, &deterministic, &actions, &tools).await;

        assert!(result.used_fallback);
        assert_eq!(result.action_node_map.len(), 2);
        assert_eq!(result.action_node_map[0].action_id, actions[0].id);
        assert_eq!(result.action_node_map[0].node_id, deterministic.nodes[0].id);
    }
}
