use super::{ExecutorResult, Mcp, WorkflowExecutor};
use clickweave_core::output_schema::VerificationMethod;
use clickweave_core::{NodeRun, NodeType};
use clickweave_llm::{ChatBackend, Message};
use clickweave_mcp::ToolContent;
use serde_json::Value;

/// Extract verification config from any action node type.
/// Returns Some((method, assertion)) only if both are set.
pub(crate) fn extract_verification_config(
    node_type: &NodeType,
) -> Option<(VerificationMethod, String)> {
    macro_rules! check {
        ($p:expr) => {
            if let (Some(method), Some(assertion)) =
                (&$p.verification_method, &$p.verification_assertion)
            {
                return Some((*method, assertion.clone()));
            }
        };
    }
    match node_type {
        NodeType::Click(p) => check!(p),
        NodeType::Hover(p) => check!(p),
        NodeType::TypeText(p) => check!(p),
        NodeType::PressKey(p) => check!(p),
        NodeType::Scroll(p) => check!(p),
        NodeType::FocusWindow(p) => check!(p),
        NodeType::Drag(p) => check!(p),
        NodeType::LaunchApp(p) => check!(p),
        NodeType::QuitApp(p) => check!(p),
        NodeType::CdpClick(p) => check!(p),
        NodeType::CdpHover(p) => check!(p),
        NodeType::CdpFill(p) => check!(p),
        NodeType::CdpType(p) => check!(p),
        NodeType::CdpPressKey(p) => check!(p),
        NodeType::CdpNavigate(p) => check!(p),
        NodeType::CdpNewPage(p) => check!(p),
        NodeType::CdpClosePage(p) => check!(p),
        NodeType::CdpSelectPage(p) => check!(p),
        NodeType::CdpHandleDialog(p) => check!(p),
        _ => {}
    }
    None
}

impl<C: ChatBackend> WorkflowExecutor<C> {
    /// Run post-action VLM verification if the node has verification enabled.
    /// Stores `verified` and `verification_reasoning` in RuntimeContext.
    pub(crate) async fn run_action_verification(
        &mut self,
        auto_id: &str,
        method: &VerificationMethod,
        assertion: &str,
        mcp: &(impl Mcp + ?Sized),
        node_run: Option<&NodeRun>,
    ) -> ExecutorResult<()> {
        let (verified, reasoning) = match method {
            VerificationMethod::Vlm => {
                self.log(format!("Running VLM verification for {}", auto_id));
                self.run_vlm_verification(auto_id, assertion, mcp).await
            }
            VerificationMethod::Dom => {
                self.log(format!(
                    "DOM verification requested for {} but not yet implemented",
                    auto_id
                ));
                (false, "DOM verification is not yet implemented".to_string())
            }
            VerificationMethod::AccessibilityTree => {
                self.log(format!(
                    "Accessibility tree verification requested for {} but not yet implemented",
                    auto_id
                ));
                (
                    false,
                    "Accessibility tree verification is not yet implemented".to_string(),
                )
            }
        };

        self.context
            .set_variable(format!("{}.verified", auto_id), Value::Bool(verified));
        self.context.set_variable(
            format!("{}.verification_reasoning", auto_id),
            Value::String(reasoning.clone()),
        );

        self.record_event(
            node_run,
            "action_verification",
            serde_json::json!({
                "auto_id": auto_id,
                "method": format!("{:?}", method),
                "verified": verified,
                "reasoning": reasoning,
            }),
        );

        Ok(())
    }

    /// Perform VLM-based verification: take a screenshot, describe it via VLM,
    /// then judge whether the assertion holds.
    async fn run_vlm_verification(
        &self,
        auto_id: &str,
        assertion: &str,
        mcp: &(impl Mcp + ?Sized),
    ) -> (bool, String) {
        // Wait for UI to settle after action
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;

        // Capture screenshot and extract base64 image data.
        // Prefer the focused app window; fall back to full-screen only when no
        // app is known, so unrelated windows don't dominate the VLM observation.
        let mut screenshot_args = serde_json::json!({"include_ocr": false});
        if let Some(app_name) = self.focused_app_name() {
            screenshot_args["app_name"] = serde_json::Value::String(app_name);
        } else {
            screenshot_args["mode"] = serde_json::Value::String("screen".to_string());
        }
        let screenshot_result = mcp
            .call_tool("take_screenshot", Some(screenshot_args))
            .await;

        let image_b64 = match screenshot_result {
            Ok(result) => {
                let mut found = None;
                for content in &result.content {
                    if let ToolContent::Image { data, .. } = content {
                        found = Some(data.clone());
                        break;
                    }
                }
                match found {
                    Some(img) => img,
                    None => {
                        return (false, "Screenshot returned no image data".to_string());
                    }
                }
            }
            Err(e) => return (false, format!("Screenshot failed: {}", e)),
        };

        // Prepare the image for VLM (resize/compress)
        let (prepared_b64, mime) = match clickweave_llm::prepare_base64_image_for_vlm(
            &image_b64,
            clickweave_llm::DEFAULT_MAX_DIMENSION,
        ) {
            Some(pair) => pair,
            None => {
                self.log("Action verification: failed to prepare screenshot for VLM".to_string());
                return (false, "Failed to prepare screenshot for VLM".to_string());
            }
        };

        // Select the VLM backend: prefer verdict_vlm, fall back to vlm/supervision, then agent.
        // We branch here because verdict_vlm is a concrete LlmClient while
        // vision_backend() returns &C (generic).
        if let Some(ref vlm) = self.verdict_vlm {
            self.vlm_describe_and_judge(vlm, auto_id, assertion, &prepared_b64, &mime)
                .await
        } else if let Some(vlm) = self.vision_backend() {
            self.vlm_describe_and_judge(vlm, auto_id, assertion, &prepared_b64, &mime)
                .await
        } else {
            (false, "No VLM configured for verification".to_string())
        }
    }

    /// Two-stage VLM verification: describe the screenshot, then judge the assertion.
    /// Generic over any `ChatBackend` so it works with both `LlmClient` (verdict_vlm)
    /// and the generic `C` (vision_backend).
    async fn vlm_describe_and_judge(
        &self,
        vlm: &impl ChatBackend,
        auto_id: &str,
        assertion: &str,
        prepared_b64: &str,
        mime: &str,
    ) -> (bool, String) {
        // Stage 1: Ask VLM to describe what it sees
        let describe_prompt = format!(
            "Describe the current state of the screen. Focus on whether the following \
             assertion appears to be true: \"{}\". Be concise (1-2 sentences).",
            assertion
        );

        let describe_messages = vec![Message::user_with_images(
            describe_prompt,
            vec![(prepared_b64.to_string(), mime.to_string())],
        )];

        let observation = match vlm.chat(describe_messages, None).await {
            Ok(response) => response
                .choices
                .first()
                .and_then(|c| c.message.content_text())
                .unwrap_or("VLM returned empty response")
                .to_string(),
            Err(e) => {
                self.log(format!(
                    "Action verification: VLM description failed: {}",
                    e
                ));
                return (false, format!("VLM description error: {}", e));
            }
        };

        self.log(format!(
            "Action verification for {}: observation = {}",
            auto_id, observation
        ));

        // Stage 2: Judge whether the assertion holds based on the observation
        let judge_prompt = format!(
            "Based on the following visual observation of a screen, determine whether \
             this assertion is true:\n\n\
             Assertion: \"{}\"\n\
             Observation: \"{}\"\n\n\
             Return ONLY a JSON object: {{\"verified\": true/false, \"reasoning\": \"brief explanation\"}}",
            assertion, observation
        );

        let judge_messages = vec![Message::user(judge_prompt)];

        match vlm.chat(judge_messages, None).await {
            Ok(response) => {
                let raw = response
                    .choices
                    .first()
                    .and_then(|c| c.message.content_text())
                    .unwrap_or("");

                parse_action_verification_response(raw)
            }
            Err(e) => {
                self.log(format!("Action verification: VLM judgment failed: {}", e));
                (false, format!("VLM judgment error: {}", e))
            }
        }
    }
}

/// Parse the VLM's JSON verification response. Returns (verified, reasoning).
fn parse_action_verification_response(raw: &str) -> (bool, String) {
    let json_text = super::app_resolve::parse_llm_json_response(raw);

    if let Some(json_str) = json_text
        && let Ok(parsed) = serde_json::from_str::<Value>(json_str)
    {
        let verified = parsed["verified"].as_bool().unwrap_or(false);
        let reasoning = parsed["reasoning"]
            .as_str()
            .unwrap_or("no reasoning provided")
            .to_string();
        return (verified, reasoning);
    }

    // If we can't parse, assume not verified (conservative)
    (
        false,
        format!("Could not parse verification response: {}", raw),
    )
}
