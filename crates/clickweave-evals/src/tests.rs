use super::*;
use clickweave_engine::agent::test_stubs::{ScriptedLlm, llm_reply_tool};

fn scenario() -> EvalScenario {
    serde_json::from_str(include_str!("../scenarios/synthetic_electron_pre_cdp.json")).unwrap()
}

#[tokio::test]
async fn deterministic_score_penalizes_forbidden_visual_fallback() {
    let llm = ScriptedLlm::new(vec![
        llm_reply_tool("find_text", json!({"text": "Message"})),
        llm_reply_tool("agent_done", json!({"summary": "done"})),
    ]);
    let report = run_eval::<_, ScriptedLlm>(scenario(), llm, None, None)
        .await
        .unwrap();
    assert!(report.deterministic.forbidden_tool_calls >= 1);
    assert!(report.final_score < 1.0);
    assert!(
        serde_json::to_string(&report)
            .unwrap()
            .contains("[SYSTEM_PROMPT_OMITTED]")
    );
}

#[tokio::test]
async fn stop_after_agent_tool_halts_after_recording_target_action() {
    let mut scenario = scenario();
    scenario.scoring.required_tools.clear();
    scenario.scoring.required_agent_tools = vec!["agent_replan".to_string()];
    scenario.scoring.required_agent_tool_groups.clear();
    scenario.scoring.required_agent_tool_counts.clear();
    scenario.scoring.forbidden_tools.clear();
    scenario.scoring.forbidden_agent_tools.clear();
    scenario.scoring.stop_after_agent_tools = vec!["agent_replan".to_string()];
    scenario.scoring.max_agent_tool_calls = Some(1);
    scenario.scoring.max_repeated_action_warnings = Some(0);
    scenario.scoring.completion_required = false;

    let llm = ScriptedLlm::new(vec![
        llm_reply_tool("agent_replan", json!({"reason": "target absent"})),
        llm_reply_tool("agent_done", json!({"summary": "should not run"})),
    ]);

    let report = run_eval::<_, ScriptedLlm>(scenario, llm, None, None)
        .await
        .unwrap();

    let halt = report.eval_halt.as_ref().expect("eval should halt");
    assert_eq!(halt.reason, "stop_after_agent_tools");
    assert_eq!(halt.agent_tool, "agent_replan");
    assert!(report.run_error.is_none());
    assert_eq!(report.llm_trace.len(), 1);
    assert_eq!(report.deterministic.agent_tool_calls, 1);
    assert_eq!(report.deterministic.max_agent_tool_calls_excess, 0);
    assert!(report.deterministic.required_agent_tools_missing.is_empty());
    assert!(!report.deterministic.completed);
    assert!(report.final_score > 0.99);
}

#[test]
fn deterministic_score_can_hard_fail_repeated_action_warnings() {
    let mut scenario = scenario();
    scenario.scoring.required_tools.clear();
    scenario.scoring.required_agent_tools.clear();
    scenario.scoring.required_agent_tool_groups.clear();
    scenario.scoring.required_agent_tool_counts.clear();
    scenario.scoring.forbidden_tools.clear();
    scenario.scoring.forbidden_agent_tools.clear();
    scenario.scoring.max_agent_tool_calls = None;
    scenario.scoring.max_repeated_action_warnings = Some(0);
    scenario.scoring.completion_required = false;

    let score = score_deterministic(
        &scenario,
        false,
        0,
        &[],
        &[],
        &[json!({
            "type": "warning",
            "message": "no-progress: repeated action cycle `cdp_fill` -> `cdp_click`"
        })],
    );

    assert_eq!(score.repeated_action_warnings, 1);
    assert_eq!(score.max_repeated_action_warnings_excess, 1);
    assert_eq!(score.score, 0.0);
}

#[test]
fn redaction_removes_home_and_image_payloads() {
    let home = std::env::var("HOME").unwrap_or_else(|_| "synthetic-home".to_string());
    let raw = json!({
        "path": format!("{home}/private/project"),
        "image_url": "data:image/png;base64,abc",
        "api_key": "secret"
    });
    let out = redact_value(raw);
    let s = serde_json::to_string(&out).unwrap();
    assert!(!s.contains(&home));
    assert!(!s.contains("abc"));
    assert!(!s.contains("secret"));
}

#[test]
fn scenario_privacy_gate_rejects_personal_markers() {
    let mut scenario = scenario();
    scenario.goal = "Send a note to someone@example.com".to_string();
    assert!(scenario.validate_privacy().is_err());
}

#[test]
fn scenario_privacy_gate_requires_synthetic_prefix() {
    let mut scenario = scenario();
    scenario.id = "electron_pre_cdp".to_string();
    assert!(scenario.validate_privacy().is_err());
}

#[test]
fn bundled_scenarios_are_valid_synthetic_fixtures() {
    let scenarios =
        load_scenarios_dir(&Path::new(env!("CARGO_MANIFEST_DIR")).join("scenarios")).unwrap();
    assert!(scenarios.len() >= 6);
    assert!(
        scenarios
            .iter()
            .all(|scenario| scenario.id.starts_with("synthetic_"))
    );
}

#[tokio::test]
async fn cdp_tools_are_hidden_until_synthetic_connect_state() {
    let scenario = scenario();
    let mcp = ScenarioMcp::new(&scenario);
    assert!(!mcp.has_tool("cdp_find_elements"));

    mcp.call_tool("cdp_connect", Some(json!({"port": 12345})))
        .await
        .unwrap();

    assert!(mcp.has_tool("cdp_find_elements"));
}

#[test]
fn parses_judge_json_with_surrounding_text() {
    let parsed = parse_judge_report(
            r#"Here:
            {"score":0.7,"verdict":"partial","failure_class":"prompt_misroutes","root_cause":"x","prompt_feedback":["y"],"recommended_prompt_patch":"","overfit_risk":"low"}
            "#,
        )
        .unwrap();
    assert_eq!(parsed.verdict, "partial");
    assert_eq!(parsed.score, 0.7);
}
