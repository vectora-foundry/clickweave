use super::helpers::*;
use clickweave_core::walkthrough::AppKind;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// resolve_element_name integration tests (scripted LLM backend)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn resolve_element_name_successful_match() {
    let exec = make_scripted_executor(vec![r#"{"name": "Multiply"}"#]);
    let available = strs(&["Calculator", "Multiply", "Divide"]);
    assert_eq!(
        exec.resolve_element_name(Uuid::new_v4(), "×", &available, Some("Calculator"), None)
            .await
            .unwrap(),
        "Multiply"
    );
}

#[tokio::test]
async fn resolve_element_name_caches_result() {
    // Only one scripted response — second call must hit cache.
    let exec = make_scripted_executor(vec![r#"{"name": "Subtract"}"#]);
    let available = strs(&["Subtract", "Add"]);
    let node_id = Uuid::new_v4();
    let first = exec
        .resolve_element_name(node_id, "−", &available, None, None)
        .await
        .unwrap();
    let second = exec
        .resolve_element_name(node_id, "−", &available, None, None)
        .await
        .unwrap();
    assert_eq!(first, "Subtract");
    assert_eq!(second, "Subtract");
}

#[tokio::test]
async fn resolve_element_name_null_match_returns_error() {
    let exec = make_scripted_executor(vec![r#"{"name": null}"#]);
    let err = exec
        .resolve_element_name(
            Uuid::new_v4(),
            "nonexistent",
            &strs(&["Multiply"]),
            None,
            None,
        )
        .await
        .unwrap_err();
    assert!(err.to_string().contains("not found"));
}

#[tokio::test]
async fn resolve_element_name_rejects_hallucinated_name() {
    let exec = make_scripted_executor(vec![r#"{"name": "Hallucinated"}"#]);
    let err = exec
        .resolve_element_name(
            Uuid::new_v4(),
            "×",
            &strs(&["Multiply", "Divide"]),
            None,
            None,
        )
        .await
        .unwrap_err();
    assert!(err.to_string().contains("not in available elements list"));
}

#[tokio::test]
async fn resolve_element_name_handles_code_block_wrapped_response() {
    let exec = make_scripted_executor(vec!["```json\n{\"name\": \"All Clear\"}\n```"]);
    assert_eq!(
        exec.resolve_element_name(
            Uuid::new_v4(),
            "AC",
            &strs(&["All Clear", "Equals"]),
            Some("Calculator"),
            None
        )
        .await
        .unwrap(),
        "All Clear"
    );
}

#[tokio::test]
async fn resolve_element_name_handles_prose_wrapped_response() {
    let exec = make_scripted_executor(vec![
        "The matching element is:\n{\"name\": \"Divide\"}\nThis maps the ÷ symbol.",
    ]);
    assert_eq!(
        exec.resolve_element_name(
            Uuid::new_v4(),
            "÷",
            &strs(&["Multiply", "Divide"]),
            None,
            None
        )
        .await
        .unwrap(),
        "Divide"
    );
}

// ---------------------------------------------------------------------------
// prepare_find_text_retry end-to-end tests (parse → LLM resolve → retry args)
// ---------------------------------------------------------------------------

const AVAILABLE_ELEMENTS_RESPONSE: &str =
    "[]\n{\"available_elements\":[\"Multiply\",\"Divide\",\"Subtract\"]}";

#[tokio::test]
async fn prepare_find_text_retry_full_flow() {
    let exec = make_scripted_executor(vec![r#"{"name": "Multiply"}"#]);
    let args = exec
        .prepare_find_text_retry(
            Uuid::new_v4(),
            &serde_json::json!({"text": "×", "app_name": "Calculator"}),
            AVAILABLE_ELEMENTS_RESPONSE,
            None,
        )
        .await
        .expect("should produce retry args");
    assert_eq!(args["text"], "Multiply");
    assert_eq!(args["app_name"], "Calculator");
}

#[tokio::test]
async fn prepare_find_text_retry_preserves_extra_fields() {
    let exec = make_scripted_executor(vec![r#"{"name": "Subtract"}"#]);
    let args = exec
        .prepare_find_text_retry(
            Uuid::new_v4(),
            &serde_json::json!({"text": "−", "app_name": "Calculator", "match_mode": "exact"}),
            "[]\n{\"available_elements\":[\"Add\",\"Subtract\"]}",
            None,
        )
        .await
        .unwrap();
    assert_eq!(args["text"], "Subtract");
    assert_eq!(args["app_name"], "Calculator");
    assert_eq!(args["match_mode"], "exact");
}

#[tokio::test]
async fn prepare_find_text_retry_falls_back_to_focused_app() {
    let exec = make_scripted_executor(vec![r#"{"name": "Multiply"}"#]);
    *exec.focused_app.write().unwrap() = Some(("Calculator".to_string(), AppKind::Native));

    let args = exec
        .prepare_find_text_retry(
            Uuid::new_v4(),
            &serde_json::json!({"text": "×"}),
            AVAILABLE_ELEMENTS_RESPONSE,
            None,
        )
        .await
        .unwrap();
    assert_eq!(args["text"], "Multiply");
    let cache_key = ("×".to_string(), Some("Calculator".to_string()));
    assert!(exec.element_cache.read().unwrap().contains_key(&cache_key));
}

#[tokio::test]
async fn prepare_find_text_retry_none_when_no_available_elements() {
    let exec = make_scripted_executor(vec![]);
    assert!(
        exec.prepare_find_text_retry(
            Uuid::new_v4(),
            &serde_json::json!({"text": "×"}),
            "[{\"text\":\"×\",\"x\":100,\"y\":200}]",
            None,
        )
        .await
        .is_none()
    );
}

#[tokio::test]
async fn prepare_find_text_retry_none_when_llm_finds_no_match() {
    let exec = make_scripted_executor(vec![r#"{"name": null}"#]);
    assert!(
        exec.prepare_find_text_retry(
            Uuid::new_v4(),
            &serde_json::json!({"text": "zzz"}),
            AVAILABLE_ELEMENTS_RESPONSE,
            None,
        )
        .await
        .is_none()
    );
}

// ---------------------------------------------------------------------------
// Click disambiguation tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn disambiguate_click_matches_picks_llm_choice() {
    let exec = make_scripted_executor(vec![r#"{"index": 1}"#]);
    let matches = make_find_text_matches(&[("2×", "AXStaticText"), ("2", "AXButton")]);
    let idx = exec
        .disambiguate_click_matches(Uuid::new_v4(), "2", &matches, Some("Calculator"), None)
        .await
        .unwrap();
    assert_eq!(idx, 1);
}

#[tokio::test]
async fn disambiguate_click_matches_out_of_bounds() {
    let exec = make_scripted_executor(vec![r#"{"index": 5}"#]);
    let matches = make_find_text_matches(&[("2×", "AXStaticText"), ("2", "AXButton")]);
    let err = exec
        .disambiguate_click_matches(Uuid::new_v4(), "2", &matches, None, None)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("out-of-bounds"));
}

#[tokio::test]
async fn disambiguate_click_matches_missing_index_key() {
    let exec = make_scripted_executor(vec![r#"{"choice": 0}"#]);
    let matches = make_find_text_matches(&[("Save", "AXButton"), ("Save as...", "AXMenuItem")]);
    let err = exec
        .disambiguate_click_matches(Uuid::new_v4(), "Save", &matches, None, None)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("no valid index"));
}

#[tokio::test]
async fn disambiguate_click_matches_code_block_wrapped() {
    let exec = make_scripted_executor(vec!["```json\n{\"index\": 0}\n```"]);
    let matches = make_find_text_matches(&[("OK", "AXButton"), ("OK", "AXStaticText")]);
    let idx = exec
        .disambiguate_click_matches(Uuid::new_v4(), "OK", &matches, Some("MyApp"), None)
        .await
        .unwrap();
    assert_eq!(idx, 0);
}
