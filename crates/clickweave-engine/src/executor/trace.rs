use super::{ExecutorCommand, ExecutorEvent, WorkflowExecutor};
use base64::Engine;
use clickweave_core::{ArtifactKind, NodeRun, RunStatus, TraceEvent, TraceLevel};
use clickweave_llm::ChatBackend;
use clickweave_mcp::{ToolCallResult, ToolContent};
use serde_json::Value;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::mpsc::Receiver;
use tracing::{debug, error, info};

impl<C: ChatBackend> WorkflowExecutor<C> {
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

    pub(crate) fn record_event(&self, run: Option<&NodeRun>, event_type: &str, payload: Value) {
        let event = TraceEvent {
            timestamp: Self::now_millis(),
            event_type: event_type.to_string(),
            payload,
        };
        let result = match run {
            Some(run) => self.storage.append_event(run, &event),
            None => self.storage.append_execution_event(&event),
        };
        if let Err(e) = result {
            tracing::warn!("Failed to append trace event: {}", e);
        }
    }

    pub(crate) fn extract_result_text(result: &ToolCallResult) -> String {
        result
            .content
            .iter()
            .filter_map(|c| match c {
                ToolContent::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Truncate text to a max byte length, snapping to a char boundary.
    pub(crate) fn truncate_for_trace(text: &str, max_bytes: usize) -> String {
        if text.len() <= max_bytes {
            return text.to_string();
        }
        let end = text.floor_char_boundary(max_bytes);
        format!("{}...[truncated, {} total]", &text[..end], text.len())
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

    pub(crate) fn stop_requested(&self, command_rx: &mut Receiver<ExecutorCommand>) -> bool {
        matches!(command_rx.try_recv(), Ok(ExecutorCommand::Stop))
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
