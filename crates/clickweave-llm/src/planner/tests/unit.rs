use super::helpers::*;
use crate::planner::mapping::step_to_node_type;
use crate::planner::parse::{extract_json, layout_nodes, truncate_intent};
use crate::planner::prompt::planner_system_prompt;
use crate::planner::*;
use clickweave_core::NodeType;

#[test]
fn test_extract_json_plain() {
    let input = r#"{"steps": []}"#;
    assert_eq!(extract_json(input), input);
}

#[test]
fn test_extract_json_code_fence() {
    let input = "```json\n{\"steps\": []}\n```";
    assert_eq!(extract_json(input), r#"{"steps": []}"#);
}

#[test]
fn test_extract_json_plain_fence() {
    let input = "```\n{\"steps\": []}\n```";
    assert_eq!(extract_json(input), r#"{"steps": []}"#);
}

#[test]
fn test_layout_nodes() {
    let positions = layout_nodes(3);
    assert_eq!(positions.len(), 3);
    assert!(positions[1].y > positions[0].y);
    assert!(positions[2].y > positions[1].y);
}

#[test]
fn test_truncate_intent() {
    assert_eq!(truncate_intent("short"), "short");
    let long = "a".repeat(60);
    let truncated = truncate_intent(&long);
    assert!(truncated.len() <= 50);
    assert!(truncated.ends_with("..."));
}

#[test]
fn test_truncate_intent_multibyte_utf8() {
    // Each emoji is 4 bytes; 13 emojis = 52 bytes > 50 limit
    let emojis = "🎉".repeat(13);
    let truncated = truncate_intent(&emojis);
    assert!(truncated.ends_with("..."));
    // Must not panic and must be valid UTF-8

    // Multi-byte char spanning the byte-47 boundary
    // 46 ASCII bytes + "é" (2 bytes) + padding = well over 50
    let mixed = format!("{}é{}", "a".repeat(46), "b".repeat(10));
    let truncated = truncate_intent(&mixed);
    assert!(truncated.ends_with("..."));
    // The "é" at byte 46-47 should be included or excluded cleanly
    assert!(!truncated.contains('\u{FFFD}')); // no replacement chars
}

#[test]
fn test_planner_prompt_includes_tools() {
    let tools = vec![serde_json::json!({
        "type": "function",
        "function": {
            "name": "click",
            "description": "Click at coordinates",
            "parameters": {}
        }
    })];
    let prompt = planner_system_prompt(&tools, false, false, None);
    assert!(prompt.contains("click"));
    assert!(prompt.contains("Tool"));
    assert!(!prompt.contains("step_type\": \"AiTransform\""));
    assert!(!prompt.contains("step_type\": \"AiStep\""));
}

#[test]
fn test_planner_system_prompt_with_all_features() {
    let prompt = planner_system_prompt(&[], true, true, None);
    assert!(prompt.contains("AiTransform"));
    assert!(prompt.contains("AiStep"));
}

#[test]
fn test_planner_prompt_includes_control_flow() {
    let prompt = planner_system_prompt(&[], false, false, None);
    assert!(
        prompt.contains("Loop"),
        "Prompt should mention Loop step type"
    );
    assert!(
        prompt.contains("EndLoop"),
        "Prompt should mention EndLoop step type"
    );
    assert!(prompt.contains("If"), "Prompt should mention If step type");
    assert!(
        prompt.contains("exit_condition"),
        "Prompt should describe exit_condition"
    );
    assert!(prompt.contains("loop_id"), "Prompt should describe loop_id");
    assert!(
        prompt.contains("\"nodes\""),
        "Prompt should describe graph output format"
    );
    assert!(
        prompt.contains("\"edges\""),
        "Prompt should describe graph output format"
    );
    assert!(
        prompt.contains(".found"),
        "Prompt should include variable examples"
    );
}

#[test]
fn test_step_to_node_type_click() {
    let step = PlanStep::Tool {
        tool_name: "click".to_string(),
        arguments: serde_json::json!({"x": 100.0, "y": 200.0, "button": "left"}),
        name: Some("Click button".to_string()),
    };
    let (nt, name) = step_to_node_type(&step, &[]).unwrap();
    assert_eq!(name, "Click button");
    assert!(matches!(nt, NodeType::Click(_)));
}

#[test]
fn test_step_to_node_type_unknown_tool_uses_mcp_tool_call() {
    let tools = vec![serde_json::json!({
        "type": "function",
        "function": {
            "name": "custom_tool",
            "description": "A custom tool",
            "parameters": {}
        }
    })];
    let step = PlanStep::Tool {
        tool_name: "custom_tool".to_string(),
        arguments: serde_json::json!({"foo": "bar"}),
        name: None,
    };
    let (nt, _) = step_to_node_type(&step, &tools).unwrap();
    assert!(matches!(nt, NodeType::McpToolCall(_)));
}

#[test]
fn test_step_to_node_type_unknown_tool_fails_if_not_in_schema() {
    let result = step_to_node_type(
        &PlanStep::Tool {
            tool_name: "nonexistent".to_string(),
            arguments: serde_json::json!({}),
            name: None,
        },
        &[],
    );
    assert!(result.is_err());
}

#[test]
fn test_step_to_node_type_loop() {
    let step = PlanStep::Loop {
        name: Some("Repeat".to_string()),
        exit_condition: bool_condition("check.found"),
        max_iterations: Some(20),
    };
    let (nt, name) = step_to_node_type(&step, &[]).unwrap();
    assert_eq!(name, "Repeat");
    assert!(matches!(nt, NodeType::Loop(_)));
    if let NodeType::Loop(p) = nt {
        assert_eq!(p.max_iterations, 20);
    }
}

#[test]
fn test_step_to_node_type_end_loop() {
    let step = PlanStep::EndLoop {
        name: Some("End Loop".to_string()),
        loop_id: "n2".to_string(),
    };
    let (nt, name) = step_to_node_type(&step, &[]).unwrap();
    assert_eq!(name, "End Loop");
    assert!(matches!(nt, NodeType::EndLoop(_)));
}

#[test]
fn test_step_to_node_type_if() {
    let step = PlanStep::If {
        name: Some("Check Result".to_string()),
        condition: bool_condition("find_text.found"),
    };
    let (nt, name) = step_to_node_type(&step, &[]).unwrap();
    assert_eq!(name, "Check Result");
    assert!(matches!(nt, NodeType::If(_)));
}
