use clickweave_core::{CheckResult, CheckType, CheckVerdict, NodeType, NodeVerdict};
use serde_json::Value;
use uuid::Uuid;

/// Create a deterministic NodeVerdict for a Verification-role node based on its
/// runtime result. Works for FindText, FindImage, and ListWindows which produce
/// array results with a `.found` boolean.
pub(crate) fn deterministic_verdict(
    node_id: Uuid,
    node_name: &str,
    node_type: &NodeType,
    result: &Value,
) -> NodeVerdict {
    let (found, count) = match result {
        Value::Array(arr) => (!arr.is_empty(), arr.len()),
        _ => (false, 0),
    };

    let check_type = match node_type {
        NodeType::FindText(_) => CheckType::TextPresent,
        NodeType::FindImage(_) => CheckType::TemplateFound,
        NodeType::ListWindows(_) => CheckType::WindowTitleMatches,
        _ => CheckType::TextPresent,
    };

    let verdict = if found {
        CheckVerdict::Pass
    } else {
        CheckVerdict::Fail
    };

    let reasoning = if found {
        format!(
            "Found {} match{}",
            count,
            if count == 1 { "" } else { "es" }
        )
    } else {
        "No matches found".to_string()
    };

    NodeVerdict {
        node_id,
        node_name: node_name.to_string(),
        check_results: vec![CheckResult {
            check_name: node_name.to_string(),
            check_type,
            verdict,
            reasoning,
        }],
        expected_outcome_verdict: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clickweave_core::{FindImageParams, FindTextParams, ListWindowsParams};

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
    fn list_windows_empty_produces_fail() {
        let result = serde_json::json!([]);
        let verdict = deterministic_verdict(
            Uuid::nil(),
            "Check window exists",
            &NodeType::ListWindows(ListWindowsParams::default()),
            &result,
        );
        assert_eq!(verdict.check_results[0].verdict, CheckVerdict::Fail);
        assert_eq!(
            verdict.check_results[0].check_type,
            CheckType::WindowTitleMatches
        );
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
