use super::{LoopExitReason, PendingLoopExit, WorkflowExecutor};
use clickweave_core::NodeType;
use clickweave_llm::{ChatBackend, Message};
use clickweave_mcp::{ToolContent, ToolProvider};
use serde_json::Value;
use tracing::debug;

const SUPERVISION_SYSTEM_PROMPT: &str = "\
You are supervising a UI automation workflow step by step. \
After each step executes, you receive the step description and a visual observation \
from a vision model describing the current screen state.

Your job is to determine whether each step achieved its intended effect. \
Consider the full history of prior steps to understand the workflow's progress.

Return ONLY a JSON object: {\"passed\": true/false, \"reasoning\": \"brief explanation\"}";

/// Result of LLM step verification.
pub(crate) struct VerificationResult {
    pub passed: bool,
    pub reasoning: String,
    /// Base64-encoded screenshot captured for verification, if available.
    pub screenshot: Option<String>,
}

impl<C: ChatBackend> WorkflowExecutor<C> {
    /// Take a screenshot, ask the VLM to describe it, then ask the planner
    /// (with full conversation history) whether the step succeeded.
    pub(crate) async fn verify_step(
        &self,
        node_name: &str,
        node_type: &NodeType,
        mcp: &(impl ToolProvider + ?Sized),
    ) -> VerificationResult {
        // Skip verification for read-only nodes (find_text, find_image,
        // take_screenshot, list_windows). These produce their own definitive
        // success/failure signal via the tool result — visual verification
        // adds nothing and can cause false failures.
        if node_type.is_read_only() {
            return VerificationResult {
                passed: true,
                reasoning: "Read-only steps are not verified".to_string(),
                screenshot: None,
            };
        }

        debug!(node_name = node_name, "verifying step via screenshot");

        let action = node_type.action_description();
        let app_name = self
            .focused_app_name()
            .unwrap_or_else(|| "unknown".to_string());

        // Stage 1: Capture screenshot and get VLM description
        let screenshot_data = self.capture_verification_screenshot(mcp).await;
        let observation = match &screenshot_data {
            Some(image_base64) => {
                self.describe_screenshot(image_base64, node_name, &action, &app_name)
                    .await
            }
            None => {
                self.log(
                    "Supervision: screenshot capture failed, using text-only verification"
                        .to_string(),
                );
                "Screenshot capture failed — no visual observation available.".to_string()
            }
        };

        // Stage 2: Ask planner with persistent conversation history
        let step_message = format!(
            "Step: \"{}\" — {}\nApp: {}\n\nVisual observation: {}",
            node_name, action, app_name, observation
        );
        let (passed, reasoning) = self.judge_with_history(&step_message, node_name).await;

        VerificationResult {
            passed,
            reasoning,
            screenshot: screenshot_data,
        }
    }

    /// Verify the outcome after a loop exits. Takes a screenshot and asks
    /// the supervision LLM whether the loop achieved its goal.
    pub(crate) async fn verify_loop_exit(
        &self,
        loop_exit: &PendingLoopExit,
        mcp: &(impl ToolProvider + ?Sized),
    ) -> VerificationResult {
        debug!(
            loop_name = loop_exit.loop_name.as_str(),
            reason = loop_exit.reason.as_str(),
            iterations = loop_exit.iterations,
            "verifying loop exit"
        );

        let app_name = self
            .focused_app_name()
            .unwrap_or_else(|| "unknown".to_string());

        let screenshot_data = self.capture_verification_screenshot(mcp).await;
        let observation = match &screenshot_data {
            Some(image_base64) => {
                let prompt = format!(
                    "Describe the current state of the app '{}'. \
                     The loop '{}' just finished after {} iterations (exit: {}). \
                     What does the screen show now? Be concise (1-2 sentences).",
                    app_name,
                    loop_exit.loop_name,
                    loop_exit.iterations,
                    loop_exit.reason.as_str(),
                );
                self.describe_screenshot_with_prompt(image_base64, &prompt)
                    .await
            }
            None => {
                self.log(
                    "Supervision: screenshot capture failed for loop exit verification".to_string(),
                );
                "Screenshot capture failed — no visual observation available.".to_string()
            }
        };

        let exit_description = match loop_exit.reason {
            LoopExitReason::ConditionMet => format!(
                "exit condition met after {} iterations",
                loop_exit.iterations
            ),
            LoopExitReason::MaxIterations => format!(
                "hit max iterations ({}) without meeting exit condition",
                loop_exit.iterations
            ),
        };

        let step_message = format!(
            "Loop completed: \"{}\" — {}\nApp: {}\n\nVisual observation: {}",
            loop_exit.loop_name, exit_description, app_name, observation
        );
        let log_label = format!("Loop '{}'", loop_exit.loop_name);
        let (passed, reasoning) = self.judge_with_history(&step_message, &log_label).await;

        VerificationResult {
            passed,
            reasoning,
            screenshot: screenshot_data,
        }
    }

    /// Ask the VLM to describe what it sees in the screenshot.
    async fn describe_screenshot(
        &self,
        image_base64: &str,
        node_name: &str,
        action: &str,
        app_name: &str,
    ) -> String {
        let prompt = format!(
            "Describe what you see on the screen. Focus on the app '{}' and whether \
             the action '{}' — {} appears to have taken effect. \
             Be concise (1-2 sentences).",
            app_name, node_name, action
        );
        self.describe_screenshot_with_prompt(image_base64, &prompt)
            .await
    }

    /// Ask the VLM to describe a screenshot using a custom prompt.
    /// Falls back to the planner when no explicit VLM is configured.
    async fn describe_screenshot_with_prompt(&self, image_base64: &str, prompt: &str) -> String {
        let vlm = match self.vision_backend() {
            Some(v) => v,
            None => {
                return "No VLM configured — no visual observation available.".to_string();
            }
        };

        let (prepared_b64, mime) = match clickweave_llm::prepare_base64_image_for_vlm(
            image_base64,
            clickweave_llm::DEFAULT_MAX_DIMENSION,
        ) {
            Some(pair) => pair,
            None => {
                self.log("Supervision: failed to prepare screenshot for VLM".to_string());
                return "Failed to prepare screenshot for VLM".to_string();
            }
        };

        let messages = vec![Message::user_with_images(
            prompt.to_string(),
            vec![(prepared_b64, mime)],
        )];

        match vlm.chat(messages, None).await {
            Ok(response) => response
                .choices
                .first()
                .and_then(|c| c.message.content_text())
                .unwrap_or("VLM returned empty response")
                .to_string(),
            Err(e) => {
                self.log(format!("Supervision: VLM description failed: {}", e));
                format!("VLM error: {}", e)
            }
        }
    }

    /// Push a user message into the supervision history, call the supervision
    /// LLM, store the assistant response, and parse the verdict.
    /// `log_label` is used for the log line (e.g. node name or "Loop '...'").
    async fn judge_with_history(&self, step_message: &str, log_label: &str) -> (bool, String) {
        let backend = self
            .supervision
            .as_ref()
            .or(self.vlm.as_ref())
            .unwrap_or(&self.agent);

        let messages = {
            let mut history = self.write_supervision_history();

            if history.is_empty() {
                history.push(Message::system(SUPERVISION_SYSTEM_PROMPT));
            }

            history.push(Message::user(step_message));
            history.clone()
        };

        let result = match backend.chat(messages, None).await {
            Ok(response) => {
                let raw = response
                    .choices
                    .first()
                    .and_then(|c| c.message.content_text())
                    .unwrap_or("");

                {
                    let mut history = self.write_supervision_history();
                    history.push(Message::assistant(raw));
                }

                parse_verification_response(raw)
            }
            Err(e) => {
                self.log(format!("Supervision: verification failed: {}", e));
                {
                    let mut history = self.write_supervision_history();
                    history.push(Message::assistant(format!(
                        "{{\"passed\": true, \"reasoning\": \"verification error: {}\"}}",
                        e
                    )));
                }
                (true, format!("Verification error: {}", e))
            }
        };

        self.log(format!(
            "Supervision: {} — {} ({})",
            log_label,
            if result.0 { "PASSED" } else { "FAILED" },
            result.1
        ));

        result
    }

    /// Capture a screenshot for verification. Returns base64-encoded image data.
    ///
    /// Waits briefly for UI animations to settle, then tries an app-scoped
    /// window screenshot up to 3 times with 500ms delays (the window may not
    /// be ready right after `launch_app`).
    async fn capture_verification_screenshot(
        &self,
        mcp: &(impl ToolProvider + ?Sized),
    ) -> Option<String> {
        // Let UI animations/transitions settle before capturing.
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;

        let app_name = self.focused_app_name();
        let mut args = serde_json::json!({ "mode": "window" });
        if let Some(ref name) = app_name {
            args["app_name"] = Value::String(name.clone());
        }

        // Retry window screenshot — the app window may take a moment to appear.
        for attempt in 0..3 {
            if attempt > 0 {
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            }
            if let Some(image) = self.extract_screenshot_image(mcp, args.clone()).await {
                return Some(image);
            }
        }

        None
    }

    /// Call `take_screenshot` and extract the base64-encoded image from the result.
    pub(crate) async fn extract_screenshot_image(
        &self,
        mcp: &(impl ToolProvider + ?Sized),
        args: Value,
    ) -> Option<String> {
        let result = mcp.call_tool("take_screenshot", Some(args)).await.ok()?;
        if result.is_error == Some(true) {
            return None;
        }
        for content in &result.content {
            if let ToolContent::Image { data, .. } = content {
                return Some(data.clone());
            }
        }
        None
    }

    /// Take a screenshot and extract both the image base64 and the screenshot_id
    /// (used by find_image for server-side coordinate conversion).
    pub(crate) async fn take_screenshot_with_id(
        &self,
        mcp: &(impl ToolProvider + ?Sized),
        args: Value,
    ) -> Option<(String, Option<String>)> {
        let result = mcp.call_tool("take_screenshot", Some(args)).await.ok()?;
        if result.is_error == Some(true) {
            return None;
        }
        let mut image_b64 = None;
        let mut screenshot_id = None;
        for content in &result.content {
            match content {
                ToolContent::Image { data, .. } => {
                    image_b64 = Some(data.clone());
                }
                ToolContent::Text { text } => {
                    if let Ok(meta) = serde_json::from_str::<Value>(text)
                        && let Some(id) = meta["screenshot_id"].as_str()
                    {
                        screenshot_id = Some(id.to_string());
                    }
                }
                _ => {}
            }
        }
        image_b64.map(|img| (img, screenshot_id))
    }
}

/// Parse the LLM's JSON verification response. Returns (passed, reasoning).
fn parse_verification_response(raw: &str) -> (bool, String) {
    let json_text = super::app_resolve::parse_llm_json_response(raw);

    if let Some(json_str) = json_text
        && let Ok(parsed) = serde_json::from_str::<Value>(json_str)
    {
        let passed = parsed["passed"].as_bool().unwrap_or(true);
        let reasoning = parsed["reasoning"]
            .as_str()
            .unwrap_or("no reasoning provided")
            .to_string();
        return (passed, reasoning);
    }

    // If we can't parse, assume pass
    (
        true,
        format!("Could not parse verification response: {}", raw),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_verification_pass() {
        let (passed, reasoning) = parse_verification_response(
            r#"{"passed": true, "reasoning": "Button 2 is highlighted"}"#,
        );
        assert!(passed);
        assert!(reasoning.contains("highlighted"));
    }

    #[test]
    fn parse_verification_fail() {
        let (passed, reasoning) = parse_verification_response(
            r#"{"passed": false, "reasoning": "Display still shows 0"}"#,
        );
        assert!(!passed);
        assert!(reasoning.contains("still shows 0"));
    }

    #[test]
    fn parse_verification_code_block() {
        let (passed, _) =
            parse_verification_response("```json\n{\"passed\": true, \"reasoning\": \"ok\"}\n```");
        assert!(passed);
    }

    #[test]
    fn parse_verification_malformed_assumes_pass() {
        let (passed, _) = parse_verification_response("I think it worked fine");
        assert!(passed);
    }
}
