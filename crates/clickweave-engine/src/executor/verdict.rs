use clickweave_core::{CheckResult, CheckType, CheckVerdict, NodeType, NodeVerdict};
use clickweave_llm::{ChatBackend, Message};
use serde_json::Value;
use uuid::Uuid;

/// Build a `NodeVerdict` with a single `CheckResult`.
fn make_verdict(
    node_id: Uuid,
    node_name: &str,
    check_name: &str,
    check_type: CheckType,
    verdict: CheckVerdict,
    reasoning: String,
) -> NodeVerdict {
    NodeVerdict {
        node_id,
        node_name: node_name.to_string(),
        check_results: vec![CheckResult {
            check_name: check_name.to_string(),
            check_type,
            verdict,
            reasoning,
        }],
        expected_outcome_verdict: None,
    }
}

/// Create a deterministic NodeVerdict for a Verification-role node based on its
/// runtime result. Handles:
/// - Array results (FindText/FindImage with flat array)
/// - Object with `found` boolean (FindApp)
/// - Object with `matches` array (FindImage from real MCP response)
pub(crate) fn deterministic_verdict(
    node_id: Uuid,
    node_name: &str,
    node_type: &NodeType,
    result: &Value,
) -> NodeVerdict {
    let (found, count) = match result {
        Value::Array(arr) => (!arr.is_empty(), arr.len()),
        Value::Object(map) => {
            // Try `matches` array first (FindImage MCP response shape)
            if let Some(Value::Array(matches)) = map.get("matches") {
                (!matches.is_empty(), matches.len())
            } else {
                // Fall back to `found` boolean (FindApp)
                let f = map.get("found").and_then(|v| v.as_bool()).unwrap_or(false);
                (f, usize::from(f))
            }
        }
        _ => (false, 0),
    };

    let check_type = match node_type {
        NodeType::FindText(_) => CheckType::TextPresent,
        NodeType::FindImage(_) => CheckType::TemplateFound,
        NodeType::FindApp(_) => CheckType::WindowTitleMatches,
        _ => {
            tracing::warn!(
                "deterministic_verdict called for unexpected node type: {}",
                node_type.display_name()
            );
            CheckType::TextPresent
        }
    };

    let (verdict, reasoning) = if found {
        let suffix = if count == 1 { "" } else { "es" };
        (
            CheckVerdict::Pass,
            format!("Found {} match{}", count, suffix),
        )
    } else {
        (CheckVerdict::Fail, "No matches found".to_string())
    };

    make_verdict(
        node_id, node_name, node_name, check_type, verdict, reasoning,
    )
}

fn screenshot_verification_prompt() -> String {
    format!(
        "You are verifying whether a UI automation step produced the expected visual result. \
You will receive a screenshot taken after the step completed and a description of \
what should be visible.\n\n\
{}\n\
Schema: {{\"verdict\": \"pass\" or \"fail\", \"reasoning\": \"...\"}}\n\n\
Be precise: only mark 'pass' if the screenshot clearly shows what was expected.",
        super::prompts::JSON_ONLY_INSTRUCTION,
    )
}

#[derive(serde::Deserialize)]
struct VlmVerdict {
    verdict: String,
    reasoning: String,
}

/// Create a Warn verdict for a TakeScreenshot Verification node missing `expected_outcome`.
pub(crate) fn missing_outcome_verdict(node_id: Uuid, node_name: &str) -> NodeVerdict {
    NodeVerdict {
        node_id,
        node_name: node_name.to_string(),
        check_results: vec![CheckResult {
            check_name: node_name.to_string(),
            check_type: CheckType::ScreenshotMatch,
            verdict: CheckVerdict::Warn,
            reasoning: "Verification role set but no expected_outcome configured".to_string(),
        }],
        expected_outcome_verdict: None,
    }
}

/// Create a Fail verdict when screenshot capture fails for a Verification node.
pub(crate) fn screenshot_capture_failed_verdict(node_id: Uuid, node_name: &str) -> NodeVerdict {
    NodeVerdict {
        node_id,
        node_name: node_name.to_string(),
        check_results: vec![CheckResult {
            check_name: node_name.to_string(),
            check_type: CheckType::ScreenshotMatch,
            verdict: CheckVerdict::Fail,
            reasoning: "Screenshot capture failed — cannot verify expected outcome".to_string(),
        }],
        expected_outcome_verdict: None,
    }
}

/// Evaluate a TakeScreenshot verification node using VLM.
/// Sends the screenshot + expected_outcome to the VLM and returns a NodeVerdict.
pub(crate) async fn screenshot_verdict<C: ChatBackend>(
    backend: &C,
    node_id: Uuid,
    node_name: &str,
    expected_outcome: &str,
    screenshot_base64: &str,
) -> NodeVerdict {
    let (prepared_b64, mime) = match clickweave_llm::prepare_base64_image_for_vlm(
        screenshot_base64,
        clickweave_llm::DEFAULT_MAX_DIMENSION,
    ) {
        Some(pair) => pair,
        None => {
            return screenshot_capture_failed_verdict(node_id, node_name);
        }
    };

    let user_msg = Message::user_with_images(
        format!(
            "## Node: \"{}\"\n\n## Expected outcome:\n{}",
            node_name, expected_outcome
        ),
        vec![(prepared_b64, mime)],
    );

    let messages = vec![Message::system(screenshot_verification_prompt()), user_msg];

    let (verdict, reasoning) = match backend.chat(&messages, None).await {
        Ok(response) => {
            let text = response
                .choices
                .first()
                .and_then(|c| c.message.content_text())
                .unwrap_or("");
            let parsed = super::app_resolve::parse_llm_json_response(text)
                .and_then(|s| serde_json::from_str::<VlmVerdict>(s).ok());
            match parsed {
                Some(v) => {
                    let verdict = match v.verdict.to_lowercase().as_str() {
                        "pass" => CheckVerdict::Pass,
                        _ => CheckVerdict::Fail,
                    };
                    (verdict, v.reasoning)
                }
                None => (
                    CheckVerdict::Fail,
                    format!("Failed to parse VLM response: {}", text),
                ),
            }
        }
        Err(e) => (CheckVerdict::Fail, format!("VLM call failed: {}", e)),
    };

    let check_name = format!("{}: {}", node_name, expected_outcome);
    make_verdict(
        node_id,
        node_name,
        &check_name,
        CheckType::ScreenshotMatch,
        verdict,
        reasoning,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use clickweave_core::{FindAppParams, FindImageParams, FindTextParams};

    #[test]
    fn find_text_found_produces_pass() {
        let result = serde_json::json!([
            {"text": "Login", "x": 100, "y": 200}
        ]);
        let verdict = deterministic_verdict(
            Uuid::nil(),
            "Check login visible",
            &NodeType::FindText(FindTextParams::default()),
            &result,
        );
        assert_eq!(verdict.check_results.len(), 1);
        assert_eq!(verdict.check_results[0].verdict, CheckVerdict::Pass);
        assert_eq!(verdict.check_results[0].check_type, CheckType::TextPresent);
        assert!(verdict.check_results[0].reasoning.contains("1 match"));
    }

    #[test]
    fn find_text_not_found_produces_fail() {
        let result = serde_json::json!([]);
        let verdict = deterministic_verdict(
            Uuid::nil(),
            "Check login visible",
            &NodeType::FindText(FindTextParams::default()),
            &result,
        );
        assert_eq!(verdict.check_results[0].verdict, CheckVerdict::Fail);
        assert_eq!(verdict.check_results[0].reasoning, "No matches found");
    }

    #[test]
    fn find_image_found_produces_pass() {
        let result = serde_json::json!([
            {"x": 50, "y": 60, "score": 0.95},
            {"x": 150, "y": 160, "score": 0.88}
        ]);
        let verdict = deterministic_verdict(
            Uuid::nil(),
            "Check icon present",
            &NodeType::FindImage(FindImageParams::default()),
            &result,
        );
        assert_eq!(verdict.check_results[0].verdict, CheckVerdict::Pass);
        assert_eq!(
            verdict.check_results[0].check_type,
            CheckType::TemplateFound
        );
        assert!(verdict.check_results[0].reasoning.contains("2 matches"));
    }

    #[test]
    fn find_app_empty_produces_fail() {
        let result = serde_json::json!([]);
        let verdict = deterministic_verdict(
            Uuid::nil(),
            "Check app exists",
            &NodeType::FindApp(FindAppParams::default()),
            &result,
        );
        assert_eq!(verdict.check_results[0].verdict, CheckVerdict::Fail);
        assert_eq!(
            verdict.check_results[0].check_type,
            CheckType::WindowTitleMatches
        );
    }

    #[test]
    fn find_image_object_with_matches_produces_pass() {
        // Real MCP find_image response: object with matches array
        let result = serde_json::json!({
            "matches": [
                {"screen_x": 120.0, "screen_y": 340.0, "score": 0.92}
            ]
        });
        let verdict = deterministic_verdict(
            Uuid::nil(),
            "Check icon present",
            &NodeType::FindImage(FindImageParams::default()),
            &result,
        );
        assert_eq!(verdict.check_results[0].verdict, CheckVerdict::Pass);
        assert!(verdict.check_results[0].reasoning.contains("1 match"));
    }

    #[test]
    fn find_image_object_with_empty_matches_produces_fail() {
        let result = serde_json::json!({"matches": []});
        let verdict = deterministic_verdict(
            Uuid::nil(),
            "Check icon present",
            &NodeType::FindImage(FindImageParams::default()),
            &result,
        );
        assert_eq!(verdict.check_results[0].verdict, CheckVerdict::Fail);
    }

    #[test]
    fn find_app_object_found_produces_pass() {
        // Real FindApp response: object with found boolean
        let result = serde_json::json!({"found": true, "name": "Safari", "pid": 1234});
        let verdict = deterministic_verdict(
            Uuid::nil(),
            "Check app running",
            &NodeType::FindApp(FindAppParams::default()),
            &result,
        );
        assert_eq!(verdict.check_results[0].verdict, CheckVerdict::Pass);
    }

    #[test]
    fn find_app_object_not_found_produces_fail() {
        let result = serde_json::json!({"found": false});
        let verdict = deterministic_verdict(
            Uuid::nil(),
            "Check app running",
            &NodeType::FindApp(FindAppParams::default()),
            &result,
        );
        assert_eq!(verdict.check_results[0].verdict, CheckVerdict::Fail);
    }

    #[test]
    fn non_array_result_produces_fail() {
        let result = serde_json::json!("some string");
        let verdict = deterministic_verdict(
            Uuid::nil(),
            "Check something",
            &NodeType::FindText(FindTextParams::default()),
            &result,
        );
        assert_eq!(verdict.check_results[0].verdict, CheckVerdict::Fail);
    }
}
