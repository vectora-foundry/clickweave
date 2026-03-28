use super::helpers::MockBackend;
use crate::Message;
use crate::planner::conversation_loop::{NoExecutor, conversation_loop};

#[tokio::test]
async fn conversation_loop_succeeds_on_first_try() {
    let backend = MockBackend::single(r#"{"result": "ok"}"#);
    let messages = vec![Message::user("generate JSON")];

    let output = conversation_loop(
        &backend,
        messages,
        None::<&NoExecutor>,
        |content| {
            let v: serde_json::Value = serde_json::from_str(content)?;
            Ok(v)
        },
        None::<fn(&serde_json::Value) -> anyhow::Result<()>>,
        1,
        None,
        None,
    )
    .await;

    assert!(output.is_ok());
    let out = output.unwrap();
    assert_eq!(out.result["result"], "ok");
    assert_eq!(backend.call_count(), 1);
}

#[tokio::test]
async fn conversation_loop_retries_on_parse_error_then_succeeds() {
    // First response is invalid JSON, second is valid
    let backend = MockBackend::new(vec!["not valid json", r#"{"fixed": true}"#]);
    let messages = vec![Message::user("generate JSON")];

    let output = conversation_loop(
        &backend,
        messages,
        None::<&NoExecutor>,
        |content| {
            let v: serde_json::Value = serde_json::from_str(content)?;
            Ok(v)
        },
        None::<fn(&serde_json::Value) -> anyhow::Result<()>>,
        1,
        None,
        None,
    )
    .await;

    assert!(output.is_ok());
    let out = output.unwrap();
    assert_eq!(out.result["fixed"], true);
    assert_eq!(backend.call_count(), 2);
}

#[tokio::test]
async fn conversation_loop_fails_after_max_repairs() {
    // Both responses are invalid JSON — should fail after 2 attempts
    // (max_repairs = 1, so initial + 1 retry = 2 calls)
    let backend = MockBackend::new(vec!["bad json 1", "bad json 2"]);
    let messages = vec![Message::user("generate JSON")];

    let result = conversation_loop(
        &backend,
        messages,
        None::<&NoExecutor>,
        |content| {
            let _v: serde_json::Value = serde_json::from_str(content)?;
            Ok(())
        },
        None::<fn(&()) -> anyhow::Result<()>>,
        1,
        None,
        None,
    )
    .await;

    let err = result.err().expect("should be an error");
    let err_msg = err.to_string();
    assert!(
        err_msg.contains("expected"),
        "Error should be a JSON parse error, got: {}",
        err_msg
    );
    assert_eq!(backend.call_count(), 2);
}

#[tokio::test]
async fn conversation_loop_process_error_triggers_retry() {
    // First response is valid JSON but fails the process closure,
    // second response succeeds
    let backend = MockBackend::new(vec![r#"{"status": "draft"}"#, r#"{"status": "final"}"#]);
    let messages = vec![Message::user("generate JSON")];

    let output = conversation_loop(
        &backend,
        messages,
        None::<&NoExecutor>,
        |content| {
            let v: serde_json::Value = serde_json::from_str(content)?;
            if v["status"] == "draft" {
                anyhow::bail!("status must be final");
            }
            Ok(v)
        },
        None::<fn(&serde_json::Value) -> anyhow::Result<()>>,
        1,
        None,
        None,
    )
    .await;

    assert!(output.is_ok());
    let out = output.unwrap();
    assert_eq!(out.result["status"], "final");
    assert_eq!(backend.call_count(), 2);
}

#[tokio::test]
async fn conversation_loop_process_error_on_all_attempts() {
    // Both responses fail the process closure
    let backend = MockBackend::new(vec![r#"{"status": "draft"}"#, r#"{"status": "draft"}"#]);
    let messages = vec![Message::user("generate JSON")];

    let result = conversation_loop(
        &backend,
        messages,
        None::<&NoExecutor>,
        |content| {
            let v: serde_json::Value = serde_json::from_str(content)?;
            if v["status"] == "draft" {
                anyhow::bail!("status must be final");
            }
            Ok(v)
        },
        None::<fn(&serde_json::Value) -> anyhow::Result<()>>,
        1,
        None,
        None,
    )
    .await;

    let err = result.err().expect("should be an error");
    assert!(err.to_string().contains("status must be final"));
    assert_eq!(backend.call_count(), 2);
}
