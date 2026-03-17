use crate::{ChatBackend, ChatResponse, Message};
use anyhow::{Context, Result, anyhow};
use tracing::{debug, info, warn};

const MAX_REPAIR_ATTEMPTS: usize = 1;

/// Chat with the LLM, retrying once with error feedback on failure.
/// `label` is used for log messages (e.g. "Planner", "Patcher").
/// `process` receives the raw text content and returns Ok(T) or Err to trigger a repair.
pub(crate) async fn chat_with_repair<T>(
    backend: &impl ChatBackend,
    label: &str,
    mut messages: Vec<Message>,
    mut process: impl FnMut(&str) -> Result<T>,
) -> Result<T> {
    let mut last_error: Option<String> = None;

    for attempt in 0..=MAX_REPAIR_ATTEMPTS {
        if let Some(err) = &last_error {
            info!("Repair attempt {} for {} error: {}", attempt, label, err);
            messages.push(Message::user(format!(
                "Your previous output had an error: {}\n\nPlease fix the JSON and try again. Output ONLY the corrected JSON object.",
                err
            )));
        }

        let response: ChatResponse = backend
            .chat(messages.clone(), None)
            .await
            .context(format!("{} LLM call failed", label))?;

        let choice = response
            .choices
            .first()
            .ok_or_else(|| anyhow!("No response from {}", label.to_lowercase()))?;

        let content = choice
            .message
            .text_content()
            .ok_or_else(|| anyhow!("{} returned no text content", label))?;

        debug!("{} raw output (attempt {}): {}", label, attempt, content);

        messages.push(Message::assistant(content));

        match process(content) {
            Ok(result) => return Ok(result),
            Err(e) if attempt < MAX_REPAIR_ATTEMPTS => {
                last_error = Some(e.to_string());
            }
            Err(e) => return Err(e),
        }
    }

    Err(anyhow!("{} failed after repair attempts", label))
}

#[allow(clippy::too_many_arguments)]
/// Like [`chat_with_repair`] but with a post-parse validation step and configurable attempts.
///
/// - `max_attempts` — total LLM calls allowed (1 = single call with no retry).
/// - `process` — parse the raw LLM text into `T`. Errors trigger a retry (same as `chat_with_repair`).
/// - `validate` — runs after a successful parse. `Ok(())` accepts; `Err` triggers a retry with the
///   error message as feedback. When attempts are exhausted, the last successfully parsed `T` is
///   returned regardless of validation failure.
/// - `on_repair` — called before each retry with `(attempt_number, max_attempts)` for UI feedback.
/// - `repair_hint` — optional extra text appended to the retry prompt (e.g. structural reminders).
pub(crate) async fn chat_with_repair_and_validate<T>(
    backend: &impl ChatBackend,
    label: &str,
    mut messages: Vec<Message>,
    max_attempts: usize,
    mut process: impl FnMut(&str) -> Result<T>,
    mut validate: impl FnMut(&T) -> Result<()>,
    mut on_repair: impl FnMut(usize, usize),
    repair_hint: Option<&str>,
) -> Result<T> {
    let mut last_error: Option<String> = None;

    for attempt in 0..max_attempts {
        if let Some(err) = &last_error {
            info!("Repair attempt {} for {} error: {}", attempt, label, err);
            on_repair(attempt, max_attempts);

            let mut feedback = format!(
                "Your previous output had an error: {}\n\nPlease fix the JSON output.",
                err
            );
            if let Some(hint) = repair_hint {
                feedback.push_str("\n\n");
                feedback.push_str(hint);
            }
            messages.push(Message::user(feedback));
        }

        let response: ChatResponse = backend
            .chat(messages.clone(), None)
            .await
            .context(format!("{} LLM call failed", label))?;

        let choice = response
            .choices
            .first()
            .ok_or_else(|| anyhow!("No response from {}", label.to_lowercase()))?;

        let content = choice
            .message
            .text_content()
            .ok_or_else(|| anyhow!("{} returned no text content", label))?;

        debug!("{} raw output (attempt {}): {}", label, attempt, content);

        messages.push(Message::assistant(content));

        let result = match process(content) {
            Ok(val) => val,
            Err(e) if attempt + 1 < max_attempts => {
                last_error = Some(e.to_string());
                continue;
            }
            Err(e) => return Err(e),
        };

        // Parse succeeded — run validation
        match validate(&result) {
            Ok(()) => return Ok(result),
            Err(e) if attempt + 1 < max_attempts => {
                last_error = Some(e.to_string());
            }
            Err(e) => {
                warn!(
                    error = %e,
                    "Validation failed after {} attempts, returning as-is",
                    max_attempts
                );
                return Ok(result);
            }
        }
    }

    Err(anyhow!("{} failed after repair attempts", label))
}
