use super::{ExecutorEvent, TRACE_WRITE_FAILURE_THRESHOLD, WorkflowExecutor};
use base64::Engine;
use clickweave_core::{ArtifactKind, NodeRun, RunStatus, TraceEvent, TraceEventKind, TraceLevel};
use clickweave_llm::ChatBackend;
use clickweave_mcp::{ToolCallResult, ToolContent};
use serde_json::Value;
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::{debug, error, info};

impl<C: ChatBackend> WorkflowExecutor<C> {
    /// Send an executor event to the UI channel.
    ///
    /// Uses `try_send` rather than `send().await` even though the agent
    /// runner backpressures via `send().await`. The executor emits events
    /// from many tight inner loops (per-tool logs, per-event trace writes)
    /// that are not allowed to yield — inserting an `.await` here would
    /// either require turning ~150 call sites async or wrapping them in
    /// block-on shims. The downside is that a lagging UI can drop events;
    /// the consumer must tolerate missed `Log` entries.
    pub(crate) fn emit(&self, event: ExecutorEvent) {
        if let Err(e) = self.event_tx.try_send(event) {
            error!("Failed to send executor event: {}", e);
        }
    }

    pub(crate) fn log(&self, msg: impl Into<String>) {
        let msg = msg.into();
        info!("{}", msg);
        self.emit(ExecutorEvent::Log(msg));
    }

    pub(crate) async fn log_model_info(&self, label: &str, backend: &C) {
        match backend.fetch_model_info().await {
            Ok(Some(info)) => {
                let ctx = info
                    .effective_context_length()
                    .map_or("?".to_string(), |v| v.to_string());

                let mut details = vec![format!("model={}", info.id), format!("ctx={}", ctx)];

                if let Some(arch) = &info.arch {
                    details.push(format!("arch={}", arch));
                }
                if let Some(quant) = &info.quantization {
                    details.push(format!("quant={}", quant));
                }
                if let Some(owned_by) = &info.owned_by {
                    details.push(format!("owned_by={}", owned_by));
                }

                self.log(format!("{}: {}", label, details.join(", ")));
            }
            Ok(None) => {
                self.log(format!(
                    "{}: {} (no model info from provider)",
                    label,
                    backend.model_name()
                ));
            }
            Err(e) => {
                debug!("Failed to fetch model info for {}: {}", label, e);
                self.log(format!(
                    "{}: {} (could not query model info)",
                    label,
                    backend.model_name()
                ));
            }
        }
    }

    pub(crate) fn emit_error(&self, msg: impl Into<String>) {
        let msg = msg.into();
        error!("{}", msg);
        self.emit(ExecutorEvent::Error(msg));
    }

    pub(crate) fn now_millis() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64
    }

    /// Record a trace event.
    ///
    /// Accepts anything that can be converted to a [`TraceEventKind`], which
    /// means call sites can pass either a typed variant
    /// (`TraceEventKind::ToolCall`) or a snake_case `&str`
    /// (`"tool_call"` / `"cdp_click"` / etc.) — the latter keeps the
    /// existing thin-shim ergonomics while still going through the enum so
    /// unknown strings land in [`TraceEventKind::Unknown`].
    ///
    /// Disk-write failures are counted via
    /// [`WorkflowExecutor::trace_write_failures`]. After
    /// [`TRACE_WRITE_FAILURE_THRESHOLD`] consecutive failures the
    /// executor emits exactly one [`ExecutorEvent::Error`] so the UI can
    /// surface a degraded-persistence warning; later failures are
    /// counted silently until a successful write clears the streak. This
    /// avoids a disk-full / permission blip producing gap-filled
    /// `events.jsonl` files indistinguishable from successful runs.
    pub(crate) fn record_event(
        &mut self,
        run: Option<&NodeRun>,
        event_type: impl Into<TraceEventKind>,
        payload: Value,
    ) {
        let event = TraceEvent {
            timestamp: Self::now_millis(),
            event_type: event_type.into(),
            payload,
        };
        let result = match run {
            Some(run) => self.storage.append_event(run, &event),
            None => self.storage.append_execution_event(&event),
        };
        match result {
            Ok(_) => {
                // A successful write resets the streak so a transient
                // blip that has since recovered will trigger a fresh
                // error the next time things go wrong.
                if self.trace_write_failures > 0 {
                    self.trace_write_failures = 0;
                    self.trace_failure_reported = false;
                }
            }
            Err(e) => {
                tracing::warn!("Failed to append trace event: {}", e);
                self.trace_write_failures = self.trace_write_failures.saturating_add(1);
                if self.trace_write_failures >= TRACE_WRITE_FAILURE_THRESHOLD
                    && !self.trace_failure_reported
                {
                    self.trace_failure_reported = true;
                    // Emit directly (not via `emit_error`) so the stored
                    // executor error text is a single definitive line and
                    // not re-tracing into the same broken stream.
                    let msg = format!(
                        "Trace persistence degraded after {} consecutive failures: {}",
                        self.trace_write_failures, e
                    );
                    error!("{}", msg);
                    self.emit(ExecutorEvent::Error(msg));
                }
            }
        }
    }

    /// Truncate text to a max byte length, snapping to a char boundary.
    pub(crate) fn truncate_for_trace(text: &str, max_bytes: usize) -> String {
        crate::agent::truncate_summary(text, max_bytes)
    }

    /// Build a single-line preview of `text` for user-facing log messages.
    ///
    /// Caps at `max_chars` Unicode scalar values. If the input exceeds that,
    /// appends `… (N total chars)` where N is the full character count.
    /// `\n` and `\r` in the preview are escaped to the literal sequences `\n`
    /// and `\r` so the log entry stays on one line (including CRLF).
    pub(crate) fn preview_for_log(text: &str, max_chars: usize) -> String {
        let full_chars = text.chars().count();
        let body = if full_chars > max_chars {
            let truncated: String = text.chars().take(max_chars).collect();
            format!("{truncated}… ({full_chars} total chars)")
        } else {
            text.to_string()
        };
        body.replace('\n', "\\n").replace('\r', "\\r")
    }

    pub(crate) fn save_result_images(
        &self,
        result: &ToolCallResult,
        prefix: &str,
        node_run: &mut Option<&mut NodeRun>,
    ) -> Vec<(String, String)> {
        let mut images = Vec::new();
        for (idx, content) in result.content.iter().enumerate() {
            if let ToolContent::Image { data, mime_type } = content {
                images.push((data.clone(), mime_type.clone()));

                if let Some(run) = &mut *node_run
                    && run.trace_level != TraceLevel::Off
                {
                    let ext = if mime_type.contains("png") {
                        "png"
                    } else {
                        "jpg"
                    };
                    let filename = format!("{}_{}.{}", prefix, idx, ext);
                    if let Ok(decoded) = base64::engine::general_purpose::STANDARD.decode(data) {
                        match self.storage.save_artifact(
                            run,
                            ArtifactKind::Screenshot,
                            &filename,
                            &decoded,
                            Value::Null,
                        ) {
                            Ok(artifact) => run.artifacts.push(artifact),
                            Err(e) => tracing::warn!("Failed to save artifact: {}", e),
                        }
                    }
                }
            }
        }
        images
    }

    pub(crate) fn finalize_run(&self, run: &mut NodeRun, status: RunStatus) {
        run.ended_at = Some(Self::now_millis());
        run.status = status;
        if let Err(e) = self.storage.save_run(run) {
            tracing::warn!("Failed to save run: {}", e);
        }
    }

    pub(crate) fn is_cancelled(&self) -> bool {
        self.cancel_token.is_cancelled()
    }

    pub(crate) fn check_step_complete(&self, content: &str) -> bool {
        serde_json::from_str::<Value>(content)
            .ok()
            .and_then(|v| v.get("step_complete")?.as_bool())
            .unwrap_or(false)
    }

    pub(crate) fn resolve_image_paths(&self, args: Option<Value>) -> Option<Value> {
        let mut args = args?;
        let Some(proj) = &self.project_path else {
            return Some(args);
        };

        let path_keys = ["image_path", "imagePath", "path", "file", "template_path"];
        if let Some(obj) = args.as_object_mut() {
            for key in path_keys {
                if let Some(Value::String(path)) = obj.get(key)
                    && !path.starts_with('/')
                {
                    let absolute = proj.join(path);
                    obj.insert(
                        key.to_string(),
                        Value::String(absolute.to_string_lossy().to_string()),
                    );
                }
            }
        }

        Some(args)
    }
}

#[cfg(test)]
mod extract_result_text_tests {
    #[test]
    fn trace_write_failure_threshold_is_three() {
        assert_eq!(super::super::TRACE_WRITE_FAILURE_THRESHOLD, 3);
    }
}
