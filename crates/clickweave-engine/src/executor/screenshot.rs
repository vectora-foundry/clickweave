//! Shared `take_screenshot` helpers for in-engine callers.
//!
//! The functions are pure I/O — no retries, no UI delay, no log spam —
//! so callers layer their own retry/log policy on top (e.g.
//! [`WorkflowExecutor::capture_verification_screenshot`] retries the
//! underlying call three times before giving up).
//!
//! Returning `Option` rather than `Result` is intentional: a missing
//! screenshot must not tank the surrounding flow, and callers already
//! handle the `None` branch by falling back to text-only verification.

use super::Mcp;
use clickweave_mcp::ToolContent;
use serde_json::Value;
use tracing::warn;

/// Capture scope for a screenshot used as VLM input.
#[derive(Debug, Clone)]
pub(crate) enum ScreenshotScope {
    /// Capture the window belonging to a named app (mode=window, app_name=…).
    /// Matches the supervisor's per-step observation shape.
    Window(String),
    /// Capture the full screen (mode=screen). Used by fallback paths that
    /// have no focused app to anchor on.
    Screen,
}

impl ScreenshotScope {
    /// Translate the scope into the MCP `take_screenshot` argument payload.
    pub(crate) fn to_arguments(&self) -> Value {
        match self {
            ScreenshotScope::Window(app_name) => serde_json::json!({
                "mode": "window",
                "app_name": app_name,
                "include_ocr": false,
            }),
            ScreenshotScope::Screen => serde_json::json!({
                "mode": "screen",
                "include_ocr": false,
            }),
        }
    }
}

/// Call `take_screenshot` and extract the first image block as raw base64.
///
/// Returns `Some(raw_base64)` on success, or `None` on any of: tool error,
/// missing image block, or transport failure. Callers that need a
/// VLM-ready payload should use [`capture_screenshot_for_vlm`], which
/// chains [`clickweave_llm::prepare_base64_image_for_vlm`] on top.
pub(crate) async fn capture_raw_image(
    mcp: &(impl Mcp + ?Sized),
    scope: ScreenshotScope,
) -> Option<String> {
    capture_raw_image_with_args(mcp, scope.to_arguments()).await
}

/// Lower-level variant of [`capture_raw_image`] for callers that need to
/// pass a hand-rolled argument payload (e.g. an explicit `format` override
/// not expressed by [`ScreenshotScope`]). Shares the same contract:
/// `Some(raw_base64)` on success, `None` on any failure.
pub(crate) async fn capture_raw_image_with_args(
    mcp: &(impl Mcp + ?Sized),
    args: Value,
) -> Option<String> {
    let result = match mcp.call_tool("take_screenshot", Some(args.clone())).await {
        Ok(r) => r,
        Err(e) => {
            warn!(error = %e, tool_args = %args, "take_screenshot transport failed");
            return None;
        }
    };
    if result.is_error == Some(true) {
        let err_text = result
            .content
            .iter()
            .find_map(ToolContent::as_text)
            .unwrap_or("<no error text>");
        warn!(error = %err_text, tool_args = %args, "take_screenshot returned error");
        return None;
    }
    let image = result.content.iter().find_map(|content| match content {
        ToolContent::Image { data, .. } => Some(data.clone()),
        _ => None,
    });
    if image.is_none() {
        warn!(tool_args = %args, "take_screenshot returned no image block");
    }
    image
}

/// Call `take_screenshot`, extract the first image block, and prepare it
/// for VLM consumption.
///
/// Returns `Some((prepared_base64, mime))` on success, or `None` when any
/// step fails — tool error, missing image, or a prepare failure. The
/// caller decides how to degrade (text-only supervision, skipping the
/// VLM check entirely, etc.).
pub(crate) async fn capture_screenshot_for_vlm(
    mcp: &(impl Mcp + ?Sized),
    scope: ScreenshotScope,
) -> Option<(String, String)> {
    let raw_b64 = capture_raw_image(mcp, scope).await?;
    clickweave_llm::prepare_base64_image_for_vlm(&raw_b64, clickweave_llm::DEFAULT_MAX_DIMENSION)
}

#[cfg(test)]
mod tests {
    use super::*;
    use clickweave_mcp::ToolCallResult;
    use std::sync::Mutex;

    /// Fixture: a minimal 1x1 transparent PNG. Small enough that
    /// `prepare_base64_image_for_vlm` can decode and scale it without work.
    const TINY_PNG_BASE64: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNkYAAAAAYAAjCB0C8AAAAASUVORK5CYII=";

    /// Minimal `Mcp` impl scoped to this module so `screenshot.rs` can own
    /// its own tests without leaking test-only helpers out of
    /// `executor::tests::helpers`.
    struct ScriptedMcp {
        responses: Mutex<Vec<ToolCallResult>>,
        calls: Mutex<Vec<(String, Option<Value>)>>,
    }

    impl ScriptedMcp {
        fn new(responses: Vec<ToolCallResult>) -> Self {
            Self {
                responses: Mutex::new(responses),
                calls: Mutex::new(Vec::new()),
            }
        }
    }

    impl super::Mcp for ScriptedMcp {
        fn call_tool(
            &self,
            name: &str,
            arguments: Option<Value>,
        ) -> impl std::future::Future<Output = anyhow::Result<ToolCallResult>> + Send {
            let logged_name = name.to_string();
            let logged_args = arguments.clone();
            let popped = {
                let mut guard = self.responses.lock().unwrap_or_else(|e| e.into_inner());
                (!guard.is_empty()).then(|| guard.remove(0))
            };
            {
                let mut calls = self.calls.lock().unwrap_or_else(|e| e.into_inner());
                calls.push((logged_name, logged_args));
            }
            async move { popped.ok_or_else(|| anyhow::anyhow!("no scripted response left")) }
        }

        fn has_tool(&self, _name: &str) -> bool {
            true
        }

        fn tools_as_openai(&self) -> Vec<Value> {
            Vec::new()
        }

        async fn refresh_server_tool_list(&self) -> anyhow::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn window_scope_payload_carries_app_name() {
        let payload = ScreenshotScope::Window("Calculator".to_string()).to_arguments();
        assert_eq!(payload["mode"], "window");
        assert_eq!(payload["app_name"], "Calculator");
        assert_eq!(payload["include_ocr"], false);
    }

    #[test]
    fn screen_scope_payload_omits_app_name() {
        let payload = ScreenshotScope::Screen.to_arguments();
        assert_eq!(payload["mode"], "screen");
        assert!(payload.get("app_name").is_none());
    }

    #[tokio::test]
    async fn capture_returns_prepared_image_on_success() {
        let mcp = ScriptedMcp::new(vec![ToolCallResult {
            content: vec![ToolContent::Image {
                data: TINY_PNG_BASE64.to_string(),
                mime_type: "image/png".to_string(),
            }],
            is_error: None,
        }]);
        let out = capture_screenshot_for_vlm(&mcp, ScreenshotScope::Screen).await;
        assert!(out.is_some(), "happy path must succeed");
        let (b64, mime) = out.expect("payload present");
        assert!(!b64.is_empty(), "prepared base64 must not be empty");
        assert!(
            mime.contains("png") || mime.contains("jpeg"),
            "mime should be a concrete image type: {mime}"
        );
        let calls = mcp.calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "take_screenshot");
    }

    #[tokio::test]
    async fn capture_returns_none_when_tool_errors() {
        let mcp = ScriptedMcp::new(vec![ToolCallResult {
            content: vec![ToolContent::Text {
                text: "permission denied".to_string(),
            }],
            is_error: Some(true),
        }]);
        let out =
            capture_screenshot_for_vlm(&mcp, ScreenshotScope::Window("Chrome".to_string())).await;
        assert!(out.is_none(), "tool error must surface as None");
    }

    #[tokio::test]
    async fn capture_returns_none_when_no_image_block() {
        // Text-only result — no image block means the VLM cannot see anything,
        // so the caller must know to fall back.
        let mcp = ScriptedMcp::new(vec![ToolCallResult {
            content: vec![ToolContent::Text {
                text: "ok".to_string(),
            }],
            is_error: None,
        }]);
        let out = capture_screenshot_for_vlm(&mcp, ScreenshotScope::Screen).await;
        assert!(out.is_none(), "missing image block must surface as None");
    }
}
