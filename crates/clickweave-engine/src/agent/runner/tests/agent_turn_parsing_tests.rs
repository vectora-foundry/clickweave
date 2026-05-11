use super::*;

#[test]
fn parses_tool_call_with_no_mutations() {
    let json = r#"{
            "mutations": [],
            "action": {"kind":"tool_call","tool_name":"cdp_click","arguments":{"uid":"d5"},"tool_call_id":"tc-1"}
        }"#;
    let turn: AgentTurn = serde_json::from_str(json).unwrap();
    assert!(turn.mutations.is_empty());
    match turn.action {
        AgentAction::ToolCall { tool_name, .. } => assert_eq!(tool_name, "cdp_click"),
        _ => panic!("expected tool_call"),
    }
}

#[test]
fn parses_agent_done() {
    let json = r#"{
            "mutations": [],
            "action": {"kind":"agent_done","summary":"completed login"}
        }"#;
    let turn: AgentTurn = serde_json::from_str(json).unwrap();
    match turn.action {
        AgentAction::AgentDone { summary } => assert_eq!(summary, "completed login"),
        _ => panic!("expected agent_done"),
    }
}

#[test]
fn parses_mutations_then_action() {
    let json = r#"{
            "mutations": [
                {"kind":"push_subgoal","text":"open login"},
                {"kind":"record_hypothesis","text":"form has 2 fields"}
            ],
            "action": {"kind":"tool_call","tool_name":"cdp_find_elements","arguments":{},"tool_call_id":"tc-2"}
        }"#;
    let turn: AgentTurn = serde_json::from_str(json).unwrap();
    assert_eq!(turn.mutations.len(), 2);
}

#[test]
fn rejects_missing_action() {
    let json = r#"{"mutations": []}"#;
    let result = serde_json::from_str::<AgentTurn>(json);
    assert!(result.is_err());
}

#[test]
fn rejects_unknown_mutation_kind() {
    let json = r#"{
            "mutations": [{"kind":"set_phase","phase":"executing"}],
            "action": {"kind":"agent_done","summary":""}
        }"#;
    let result = serde_json::from_str::<AgentTurn>(json);
    assert!(result.is_err(), "set_phase is not a valid mutation (D5)");
}

#[test]
fn rejects_malformed_json() {
    // The design's error-path table says a malformed AgentTurn
    // triggers one repair retry; the parser must surface the error
    // clearly rather than returning a default.
    let json = r#"{"mutations": [], "action":"#; // truncated
    let result = serde_json::from_str::<AgentTurn>(json);
    assert!(result.is_err());
}

#[test]
fn rejects_tool_call_without_tool_name() {
    let json = r#"{
            "mutations": [],
            "action": {"kind":"tool_call","arguments":{},"tool_call_id":"tc-1"}
        }"#;
    let result = serde_json::from_str::<AgentTurn>(json);
    assert!(result.is_err(), "tool_call must require tool_name");
}

#[test]
fn accepts_tool_call_with_empty_arguments_object() {
    // Empty arguments is valid — some tools take no args (e.g. take_ax_snapshot).
    let json = r#"{
            "mutations": [],
            "action": {"kind":"tool_call","tool_name":"take_ax_snapshot","arguments":{},"tool_call_id":"tc-1"}
        }"#;
    let turn: AgentTurn = serde_json::from_str(json).unwrap();
    assert!(matches!(turn.action, AgentAction::ToolCall { .. }));
}
