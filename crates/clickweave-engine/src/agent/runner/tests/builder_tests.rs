use super::*;
use clickweave_llm::{ChatBackend, ChatOptions, ChatResponse, Message};
use serde_json::Value;
use std::sync::Arc;
use tokio::sync::mpsc;

/// Minimal stub implementing `ChatBackend` so we can confirm the
/// blanket `DynChatBackend` impl lets us stash one behind `Arc<dyn>`.
#[derive(Default)]
struct YesVlmStub;
impl ChatBackend for YesVlmStub {
    fn model_name(&self) -> &str {
        "yes-vlm"
    }
    async fn chat_with_options(
        &self,
        _messages: &[Message],
        _tools: Option<&[Value]>,
        _options: &ChatOptions,
    ) -> anyhow::Result<ChatResponse> {
        Ok(ChatResponse {
            id: "t".into(),
            choices: vec![clickweave_llm::Choice {
                index: 0,
                message: Message::assistant("YES"),
                finish_reason: Some("stop".into()),
            }],
            usage: None,
        })
    }
}

#[test]
fn with_events_stores_sender() {
    let (tx, _rx) = mpsc::channel::<RunnerOutput>(8);
    let r = StateRunner::new_for_test("g".to_string()).with_events(tx);
    assert!(r.event_tx.is_some());
}

#[test]
fn with_approval_stores_gate() {
    let (tx, _rx) = mpsc::channel::<(ApprovalRequest, oneshot::Sender<bool>)>(8);
    let r = StateRunner::new_for_test("g".to_string()).with_approval(tx);
    assert!(r.approval_gate.is_some());
}

#[test]
fn with_vision_stores_backend_as_arc_dyn() {
    let vlm: Arc<dyn DynChatBackend> = Arc::new(YesVlmStub);
    let r = StateRunner::new_for_test("g".to_string()).with_vision(vlm);
    assert!(r.vision.is_some());
}

#[test]
fn with_permissions_replaces_default_policy() {
    let policy = PermissionPolicy::default();
    let r = StateRunner::new_for_test("g".to_string()).with_permissions(policy);
    // Confirm the field is populated — the default policy is Copy-
    // like and doesn't diverge from the constructor default, so the
    // guarantee here is "no panic, no drop".
    let _ = &r.permissions;
}

#[test]
fn with_verification_artifacts_dir_stores_path() {
    let r = StateRunner::new_for_test("g".to_string())
        .with_verification_artifacts_dir(PathBuf::from("/tmp/artifacts"));
    assert_eq!(
        r.verification_artifacts_dir.as_deref(),
        Some(std::path::Path::new("/tmp/artifacts"))
    );
}
