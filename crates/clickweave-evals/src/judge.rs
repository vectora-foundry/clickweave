use super::*;

pub(crate) async fn run_semantic_judge<J: ChatBackend>(
    judge: &J,
    scenario: &EvalScenario,
    prompt_sha: &str,
    deterministic: &DeterministicScore,
    tool_trace: &[ToolTrace],
    llm_trace: &[LlmTurnTrace],
    run_error: Option<&str>,
) -> Result<SemanticJudgeReport> {
    let input = json!({
        "scenario": {
            "id": scenario.id,
            "description": scenario.description,
            "goal": redact_text(&scenario.goal),
            "scoring": scenario.scoring,
        },
        "prompt_sha": prompt_sha,
        "deterministic": deterministic,
        "tool_trace": tool_trace,
        "llm_trace": llm_trace,
        "run_error": run_error,
        "privacy": {
            "synthetic_fixture_only": true,
            "screenshots_omitted": true,
            "paths_and_secrets_redacted": true
        }
    });
    let messages = vec![
        Message::system(CODEX_JUDGE_PROMPT),
        Message::user(serde_json::to_string_pretty(&input)?),
    ];
    let response = judge
        .chat_with_options(
            &messages,
            None,
            &ChatOptions {
                temperature: Some(0.0),
                max_tokens: Some(2048),
            },
        )
        .await?;
    let text = response
        .choices
        .first()
        .and_then(|choice| choice.message.content_text())
        .context("judge returned no text")?;
    parse_judge_report(text)
}

pub fn parse_judge_report(text: &str) -> Result<SemanticJudgeReport> {
    let json_text = extract_json_object(text).context("judge response did not contain JSON")?;
    let mut report: SemanticJudgeReport =
        serde_json::from_str(json_text).context("parse judge JSON")?;
    report.score = report.score.clamp(0.0, 1.0);
    report.root_cause = redact_text(&report.root_cause);
    report.recommended_prompt_patch = redact_text(&report.recommended_prompt_patch);
    report.prompt_feedback = report
        .prompt_feedback
        .into_iter()
        .map(|s| redact_text(&s))
        .collect();
    Ok(report)
}

fn extract_json_object(text: &str) -> Option<&str> {
    let start = text.find('{')?;
    let end = text.rfind('}')?;
    (start <= end).then_some(&text[start..=end])
}
