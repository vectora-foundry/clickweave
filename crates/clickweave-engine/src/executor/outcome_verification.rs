use super::retry_context::{ExecutionHistoryEntry, RetryContext};
use super::{Mcp, WorkflowExecutor};
use clickweave_llm::{ChatBackend, Message};
use serde_json::Value;

/// Result of the two-stage outcome verification.
pub(crate) struct OutcomeVerificationResult {
    pub passed: bool,
    pub query: String,
    pub reasoning: String,
    pub screenshot: Option<String>,
}

/// Parse the VLM's JSON verification response. Returns (passed, reasoning).
fn parse_outcome_response(raw: &str) -> (bool, String) {
    let json_text = super::app_resolve::parse_llm_json_response(raw);

    if let Some(json_str) = json_text
        && let Ok(parsed) = serde_json::from_str::<Value>(json_str)
    {
        let passed = parsed["passed"].as_bool().unwrap_or(false);
        let reasoning = parsed["reasoning"]
            .as_str()
            .unwrap_or("no reasoning provided")
            .to_string();
        return (passed, reasoning);
    }

    (
        false,
        format!("Could not parse outcome verification response: {}", raw),
    )
}

/// Build a human-readable execution summary for the verification query prompt.
/// Uses the ordered execution history which interleaves node completions and
/// control-flow decisions chronologically, preserving the actual execution path.
pub(crate) fn build_execution_summary(execution_history: &[ExecutionHistoryEntry]) -> String {
    let mut lines = Vec::new();
    let mut step = 1;

    for entry in execution_history {
        match entry {
            ExecutionHistoryEntry::NodeCompleted {
                node_name,
                action_description,
            } => {
                lines.push(format!(
                    "{}. {} \u{2014} {}",
                    step, node_name, action_description
                ));
                step += 1;
            }
            ExecutionHistoryEntry::BranchTaken { node_name, outcome } => {
                lines.push(format!("  [Branch '{}': took {}]", node_name, outcome));
            }
            ExecutionHistoryEntry::LoopIteration {
                node_name,
                iteration,
            } => {
                lines.push(format!("  [Loop '{}': iteration {}]", node_name, iteration));
            }
            ExecutionHistoryEntry::LoopExited {
                node_name,
                reason,
                iterations,
            } => {
                lines.push(format!(
                    "  [Loop '{}': exited after {} iterations ({})]",
                    node_name, iterations, reason
                ));
            }
        }
    }

    lines.join("\n")
}

const QUERY_GENERATION_PROMPT: &str = "\
You are generating a visual verification query for a UI automation workflow.

Given the workflow's intent and the steps that were executed, describe precisely \
what should be visible on screen right now. Be specific about text content, UI \
elements, and their expected state. Write 2-4 sentences.

Return ONLY the visual query text, no JSON wrapping.";

const QUERY_EVALUATION_PROMPT: &str = "\
You are verifying the final outcome of a UI automation workflow. \
You will receive a screenshot and a description of what should be visible.

Return ONLY a JSON object (no markdown fences): \
{\"passed\": true/false, \"reasoning\": \"brief explanation\"}

Be precise: only mark 'pass' if the screenshot clearly shows what was expected.";

impl<C: ChatBackend> WorkflowExecutor<C> {
    /// Run outcome verification after the graph walk completes.
    /// Returns None if verification is disabled, skipped, or fails to run.
    pub(crate) async fn verify_outcome(
        &self,
        ctx: &RetryContext,
        mcp: &(impl Mcp + ?Sized),
    ) -> Option<OutcomeVerificationResult> {
        let intent = self.workflow.intent.as_deref()?;
        if !self.workflow.verify_outcome {
            return None;
        }

        self.log("Outcome verification: generating visual query...".to_string());

        let summary = build_execution_summary(&ctx.execution_history);
        let query = self.generate_verification_query(intent, &summary).await?;

        self.log(format!("Outcome verification query: {}", query));

        // Allow the UI to settle after the last action before capturing the screenshot.
        // Without this delay, fast-completing actions (e.g. pressing Enter to send a message)
        // may not have visually resolved yet.
        let delay = self.workflow.outcome_delay_ms;
        if delay > 0 {
            tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
        }

        let Some(screenshot) = self.capture_outcome_screenshot(mcp).await else {
            self.log("Outcome verification: screenshot capture failed, skipping".to_string());
            return None;
        };

        let (passed, reasoning) = match self.evaluate_outcome_query(&query, &screenshot).await {
            Some(result) => result,
            None => return None,
        };

        self.log(format!(
            "Outcome verification: {} ({})",
            if passed { "PASSED" } else { "FAILED" },
            reasoning
        ));

        Some(OutcomeVerificationResult {
            passed,
            query,
            reasoning,
            screenshot: Some(screenshot),
        })
    }

    /// Generate a natural-language visual query from the intent and execution summary.
    async fn generate_verification_query(
        &self,
        intent: &str,
        execution_summary: &str,
    ) -> Option<String> {
        let backend = self.reasoning_backend();

        let user_msg = format!(
            "Workflow intent: \"{}\"\n\nExecuted steps:\n{}\n",
            intent, execution_summary
        );

        let messages = vec![
            Message::system(QUERY_GENERATION_PROMPT),
            Message::user(&user_msg),
        ];

        match backend.chat(messages, None).await {
            Ok(response) => response
                .choices
                .first()
                .and_then(|c| c.message.content_text())
                .map(|s| s.trim().to_string()),
            Err(e) => {
                self.log(format!(
                    "Outcome verification: query generation failed: {}",
                    e
                ));
                None
            }
        }
    }

    /// Take a screenshot for outcome verification.
    /// Tries app-scoped window first, falls back to full-screen.
    async fn capture_outcome_screenshot(&self, mcp: &(impl Mcp + ?Sized)) -> Option<String> {
        if let Some(img) = self.capture_verification_screenshot(mcp).await {
            return Some(img);
        }

        self.log(
            "Outcome verification: app window not found, trying full-screen capture".to_string(),
        );
        let args = serde_json::json!({ "mode": "screen" });
        self.extract_screenshot_image(mcp, args).await
    }

    /// Evaluate a screenshot against a visual query using the VLM.
    /// Returns None if no VLM is configured or the call fails.
    async fn evaluate_outcome_query(
        &self,
        query: &str,
        screenshot_base64: &str,
    ) -> Option<(bool, String)> {
        let (prepared_b64, mime) = match clickweave_llm::prepare_base64_image_for_vlm(
            screenshot_base64,
            clickweave_llm::DEFAULT_MAX_DIMENSION,
        ) {
            Some(pair) => pair,
            None => {
                self.log(
                    "Outcome verification: failed to prepare screenshot for VLM, skipping"
                        .to_string(),
                );
                return None;
            }
        };

        let user_msg = Message::user_with_images(
            format!(
                "Expected screen state:\n\"{}\"\n\nDoes the screenshot match this description?",
                query
            ),
            vec![(prepared_b64, mime)],
        );

        let messages = vec![Message::system(QUERY_EVALUATION_PROMPT), user_msg];

        if let Some(ref vlm) = self.verdict_fast {
            self.evaluate_outcome_with_vlm(vlm, messages).await
        } else if let Some(vlm) = self.vision_backend() {
            self.evaluate_outcome_with_vlm(vlm, messages).await
        } else {
            self.log("Outcome verification: no VLM configured, skipping".to_string());
            None
        }
    }

    async fn evaluate_outcome_with_vlm(
        &self,
        vlm: &impl ChatBackend,
        messages: Vec<Message>,
    ) -> Option<(bool, String)> {
        match vlm.chat(messages, None).await {
            Ok(response) => {
                let raw = response
                    .choices
                    .first()
                    .and_then(|c| c.message.content_text())
                    .unwrap_or("");
                Some(parse_outcome_response(raw))
            }
            Err(e) => {
                self.log(format!(
                    "Outcome verification: VLM evaluation failed: {}, skipping",
                    e
                ));
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_outcome_pass() {
        let (passed, reasoning) =
            parse_outcome_response(r#"{"passed": true, "reasoning": "Message visible in chat"}"#);
        assert!(passed);
        assert!(reasoning.contains("Message visible"));
    }

    #[test]
    fn parse_outcome_fail() {
        let (passed, reasoning) = parse_outcome_response(
            r#"{"passed": false, "reasoning": "Input field still has text"}"#,
        );
        assert!(!passed);
        assert!(reasoning.contains("Input field"));
    }

    #[test]
    fn parse_outcome_code_block() {
        let (passed, _) =
            parse_outcome_response("```json\n{\"passed\": true, \"reasoning\": \"ok\"}\n```");
        assert!(passed);
    }

    #[test]
    fn parse_outcome_malformed_defaults_to_fail() {
        let (passed, _) = parse_outcome_response("looks good to me");
        assert!(!passed);
    }

    #[test]
    fn build_execution_summary_formats_correctly() {
        let history = vec![
            ExecutionHistoryEntry::NodeCompleted {
                node_name: "Click Send".to_string(),
                action_description: "Click 'Send'".to_string(),
            },
            ExecutionHistoryEntry::BranchTaken {
                node_name: "Check result".to_string(),
                outcome: "IfTrue".to_string(),
            },
            ExecutionHistoryEntry::NodeCompleted {
                node_name: "Type message".to_string(),
                action_description: "Type 'hello'".to_string(),
            },
        ];

        let summary = build_execution_summary(&history);
        assert!(summary.contains("1. Click Send"));
        assert!(summary.contains("[Branch 'Check result': took IfTrue]"));
        assert!(summary.contains("2. Type message"));
    }
}
