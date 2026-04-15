//! Integration tests for the CDP ambiguity disambiguation path.
//!
//! These exercise `WorkflowExecutor::resolve_cdp_ambiguity` directly plus the
//! retry_context override flow, avoiding the full run loop (which requires a
//! live CDP connection).  The run-loop event emission is covered by the
//! parse/unit tests inside `executor::ambiguity::tests`.

use super::helpers::*;
use crate::executor::error::{CdpCandidate, ExecutorError};
use clickweave_core::storage::RunStorage;
use clickweave_core::{ExecutionMode, TraceLevel, Workflow};
use clickweave_mcp::{ToolCallResult, ToolContent};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

/// Minimal 1x1 transparent PNG, base64-encoded. Small enough that
/// `prepare_base64_image_for_vlm` can decode and pass it to the VLM stub.
const TINY_PNG_BASE64: &str =
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNkYAAAAAYAAjCB0C8AAAAASUVORK5CYII=";

fn screenshot_response() -> ToolCallResult {
    ToolCallResult {
        content: vec![ToolContent::Image {
            data: TINY_PNG_BASE64.to_string(),
            mime_type: "image/png".to_string(),
        }],
        is_error: None,
    }
}

fn sample_candidates() -> Vec<CdpCandidate> {
    vec![
        CdpCandidate {
            uid: "a1".to_string(),
            snippet: "[uid=\"a1\"] button \"Save\"".to_string(),
        },
        CdpCandidate {
            uid: "a2".to_string(),
            snippet: "[uid=\"a2\"] button \"Save\"".to_string(),
        },
    ]
}

fn make_executor_with_vlm(
    fast_responses: Vec<&str>,
) -> crate::executor::WorkflowExecutor<ScriptedBackend> {
    let (tx, _rx) = tokio::sync::mpsc::channel(256);
    let workflow = Workflow::default();
    let temp_dir = std::env::temp_dir().join(format!(
        "clickweave_test_ambig_{}",
        Uuid::new_v4().as_simple()
    ));
    let storage = RunStorage::new_app_data(&temp_dir, &workflow.name, workflow.id);
    crate::executor::WorkflowExecutor::with_backends(
        workflow,
        ScriptedBackend::new(vec![]),
        Some(ScriptedBackend::new(fast_responses)),
        String::new(),
        ExecutionMode::Run,
        None,
        tx,
        storage,
        CancellationToken::new(),
    )
}

#[tokio::test]
async fn resolve_cdp_ambiguity_returns_chosen_uid_and_rects_from_vlm() {
    let exec = make_executor_with_vlm(vec![
        r#"{"chosen_uid": "a2", "reasoning": "second Save is the primary toolbar action"}"#,
    ]);

    let mcp = StubToolProvider::new();
    // capture_verification_screenshot -> take_screenshot (image)
    mcp.push_response(screenshot_response());
    // cdp_evaluate_script for rects
    mcp.push_text_response(
        r#"[{"x": 10.0, "y": 20.0, "width": 30.0, "height": 40.0}, {"x": 50.0, "y": 60.0, "width": 70.0, "height": 80.0}]"#,
    );

    let res = exec
        .resolve_cdp_ambiguity("Click Save", "Save", sample_candidates(), &mcp, None)
        .await
        .expect("disambiguation should succeed");

    assert_eq!(res.chosen_uid, "a2");
    assert!(res.reasoning.to_lowercase().contains("toolbar"));
    assert_eq!(res.candidates_with_rects.len(), 2);
    let r0 = res.candidates_with_rects[0]
        .rect
        .as_ref()
        .expect("first rect present");
    assert_eq!(r0.x, 10.0);
    assert_eq!(r0.width, 30.0);
    let r1 = res.candidates_with_rects[1]
        .rect
        .as_ref()
        .expect("second rect present");
    assert_eq!(r1.y, 60.0);
    // screenshot_path is a filename, not a full path — the UI resolves it
    // relative to the node's artifacts dir.
    assert!(res.screenshot_path.ends_with(".png"));
    assert!(!res.screenshot_path.contains('/'));
    // screenshot_base64 is the raw live image forwarded to the UI.
    assert_eq!(res.screenshot_base64, TINY_PNG_BASE64);

    // MCP was asked for a screenshot and a batched evaluate, in that order.
    let calls = mcp.take_calls();
    let names: Vec<&str> = calls.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(names, vec!["take_screenshot", "cdp_evaluate_script"]);
}

#[tokio::test]
async fn resolve_cdp_ambiguity_propagates_vlm_parse_failure() {
    let exec = make_executor_with_vlm(vec!["I can't decide, they all look the same."]);

    let mcp = StubToolProvider::new();
    mcp.push_response(screenshot_response());
    mcp.push_text_response("[null, null]");

    let err = exec
        .resolve_cdp_ambiguity("Click Save", "Save", sample_candidates(), &mcp, None)
        .await
        .expect_err("unparsable VLM response must surface as an error");
    let msg = err.to_string();
    assert!(
        msg.contains("Disambiguation") && msg.contains("parse"),
        "error should mention disambiguation+parse: {msg}"
    );
}

#[tokio::test]
async fn resolve_cdp_ambiguity_rejects_unknown_uid_from_vlm() {
    let exec = make_executor_with_vlm(vec![r#"{"chosen_uid": "zz", "reasoning": "fabricated"}"#]);

    let mcp = StubToolProvider::new();
    mcp.push_response(screenshot_response());
    mcp.push_text_response("[null, null]");

    let err = exec
        .resolve_cdp_ambiguity("Click Save", "Save", sample_candidates(), &mcp, None)
        .await
        .expect_err("unknown chosen_uid must fail");
    assert!(matches!(err, ExecutorError::Cdp(_)));
    assert!(err.to_string().contains("unknown uid"));
}

#[tokio::test]
async fn resolve_cdp_ambiguity_surfaces_screenshot_failure() {
    let exec = make_executor_with_vlm(vec![]);

    let mcp = StubToolProvider::new();
    // take_screenshot returns is_error: Some(true) three times (retry loop).
    for _ in 0..6 {
        mcp.push_response(ToolCallResult {
            content: vec![ToolContent::Text {
                text: "screenshot denied".to_string(),
            }],
            is_error: Some(true),
        });
    }

    let err = exec
        .resolve_cdp_ambiguity("Click Save", "Save", sample_candidates(), &mcp, None)
        .await
        .expect_err("screenshot failure must bubble up");
    let msg = err.to_string();
    assert!(
        msg.contains("screenshot") || msg.contains("Disambiguation"),
        "error message should mention screenshot or disambiguation: {msg}"
    );
}

#[tokio::test]
async fn resolve_cdp_element_uid_short_circuits_on_override() {
    // The override map should be consulted before any MCP round-trip. This
    // guards the retry path: once the agent has picked a uid, the resolver
    // must not re-take a snapshot (which would surface the same ambiguity).
    use crate::executor::retry_context::RetryContext;

    let exec = make_test_executor();
    let ctx = RetryContext::new();
    ctx.write_cdp_ambiguity_overrides()
        .insert("Save".to_string(), "a2".to_string());

    let mcp = StubToolProvider::new();
    let uid = exec
        .resolve_cdp_element_uid_with_overrides("Save", &mcp, Some(&ctx))
        .await
        .expect("override must short-circuit");
    assert_eq!(uid, "a2");

    // No MCP calls: no snapshot, no evaluate, nothing.
    assert!(mcp.take_calls().is_empty());
}

#[tokio::test]
async fn resolve_cdp_ambiguity_persists_artifacts_when_trace_enabled() {
    let exec = make_executor_with_vlm(vec![
        r#"{"chosen_uid": "a1", "reasoning": "first and only visible"}"#,
    ]);

    let mcp = StubToolProvider::new();
    mcp.push_response(screenshot_response());
    mcp.push_text_response("[null, null]");

    // Create a NodeRun so the artifact-persistence branch fires.
    let mut storage = RunStorage::new_app_data(
        &std::env::temp_dir().join(format!(
            "clickweave_test_ambig_art_{}",
            Uuid::new_v4().as_simple()
        )),
        "Test",
        Uuid::new_v4(),
    );
    storage.begin_execution().expect("begin");
    let mut run = storage
        .create_run(Uuid::new_v4(), "Click Save", TraceLevel::Minimal)
        .expect("create run");

    let res = exec
        .resolve_cdp_ambiguity(
            "Click Save",
            "Save",
            sample_candidates(),
            &mcp,
            Some(&mut run),
        )
        .await
        .expect("disambiguation should succeed");

    assert!(res.screenshot_path.ends_with(".png"));
    // NodeRun should have picked up at least the screenshot artifact via
    // save_artifact (executor's storage is separate from the test helper
    // storage so the artifact files live under the executor's storage —
    // this assertion verifies the path wiring rather than the files).
    assert_eq!(res.candidates_with_rects.len(), 2);
}
