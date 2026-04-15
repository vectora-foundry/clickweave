//! Agent-driven disambiguation for CDP targets that map to multiple snapshot
//! lines.  When `resolve_cdp_element_uid` surfaces an `ExecutorError::
//! CdpAmbiguousTarget`, this module captures a screenshot, reads each
//! candidate's viewport rect via a single batched `Runtime.evaluate`, and asks
//! the VLM to pick one.  The chosen uid is threaded back into the retry so the
//! node completes on the next attempt.

use super::error::{CandidateView, CdpCandidate, ExecutorError, ExecutorResult, Rect};
use super::{Mcp, WorkflowExecutor};
use clickweave_core::{ArtifactKind, NodeRun, TraceEvent, TraceLevel};
use clickweave_llm::{ChatBackend, ChatOptions, Message};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

/// Result of a successful agent disambiguation round.
#[derive(Debug, Clone)]
pub(crate) struct DisambiguationResult {
    pub chosen_uid: String,
    pub reasoning: String,
    pub candidates_with_rects: Vec<CandidateView>,
    /// Viewport dimensions at capture time. The UI uses these to translate
    /// the CDP-viewport rects (CSS pixels, origin at viewport top-left) into
    /// image-pixel coordinates inside the captured screenshot, which may
    /// include chrome (tab bar, title bar) above/around the viewport.
    pub viewport_width: f64,
    pub viewport_height: f64,
    /// Path to the captured screenshot, relative to the node's `artifacts/`
    /// directory (the same base the UI consumes).
    pub screenshot_path: String,
    /// Raw base64-encoded PNG of the screenshot. Forwarded inline on the
    /// executor event so the UI doesn't need to read the artifact from disk
    /// while the run is still writing to it.
    pub screenshot_base64: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct SidecarRecord {
    target: String,
    chosen_uid: String,
    reasoning: String,
    candidates: Vec<CandidateView>,
    viewport_width: f64,
    viewport_height: f64,
    screenshot_path: String,
}

const DISAMBIGUATION_PROMPT: &str = "\
You are helping a UI automation agent pick the correct element when its \
accessibility-tree resolver matched more than one candidate for the same \
target label.\n\n\
You receive:\n\
- The target label the agent tried to resolve.\n\
- A screenshot of the page taken moments ago.\n\
- A list of candidates: each has a uid, a snippet of the accessibility tree \
line that matched, and the candidate's bounding rectangle in viewport \
coordinates (x, y, width, height — in CSS pixels, top-left origin).\n\n\
Pick the candidate that best matches what the agent likely meant. Prefer \
candidates that are clearly visible, sit in a primary action area, and have \
reasonable dimensions. Avoid candidates that are off-screen or zero-sized.\n\n\
Respond with ONLY a JSON object (no markdown fences):\n\
{\"chosen_uid\": \"<uid>\", \"reasoning\": \"<1-2 sentence justification>\"}";

impl<C: ChatBackend> WorkflowExecutor<C> {
    /// Run the full disambiguation routine and return the chosen uid plus the
    /// candidate/rect data for the UI.
    pub(crate) async fn resolve_cdp_ambiguity(
        &self,
        node_name: &str,
        target: &str,
        candidates: Vec<CdpCandidate>,
        mcp: &(impl Mcp + ?Sized),
        node_run: Option<&mut NodeRun>,
    ) -> ExecutorResult<DisambiguationResult> {
        let screenshot_b64 = self
            .capture_verification_screenshot(mcp)
            .await
            .ok_or_else(|| {
                ExecutorError::Cdp(
                    "Disambiguation: failed to capture screenshot for VLM prompt".to_string(),
                )
            })?;

        let (viewport, rects) = fetch_candidate_rects(&candidates, mcp).await;
        let candidates_with_rects: Vec<CandidateView> = candidates
            .into_iter()
            .zip(rects.into_iter())
            .map(|(c, rect)| CandidateView {
                uid: c.uid,
                snippet: c.snippet,
                rect,
            })
            .collect();

        let (chosen_uid, reasoning) = self
            .agent_pick_candidate(target, &screenshot_b64, &candidates_with_rects)
            .await?;

        if !candidates_with_rects.iter().any(|c| c.uid == chosen_uid) {
            return Err(ExecutorError::Cdp(format!(
                "Disambiguation: agent returned unknown uid '{}' for target '{}'",
                chosen_uid, target
            )));
        }

        let screenshot_path = self.persist_disambiguation_artifacts(
            node_name,
            target,
            &chosen_uid,
            &reasoning,
            &candidates_with_rects,
            viewport,
            &screenshot_b64,
            node_run,
        );

        Ok(DisambiguationResult {
            chosen_uid,
            reasoning,
            candidates_with_rects,
            viewport_width: viewport.width,
            viewport_height: viewport.height,
            screenshot_path,
            screenshot_base64: screenshot_b64,
        })
    }

    /// Prompt the VLM and parse its structured choice.
    async fn agent_pick_candidate(
        &self,
        target: &str,
        screenshot_b64: &str,
        candidates: &[CandidateView],
    ) -> ExecutorResult<(String, String)> {
        let vlm = self.vision_backend().unwrap_or(&self.agent);

        let (prepared_b64, mime) = clickweave_llm::prepare_base64_image_for_vlm(
            screenshot_b64,
            clickweave_llm::DEFAULT_MAX_DIMENSION,
        )
        .ok_or_else(|| {
            ExecutorError::Cdp("Disambiguation: failed to prepare screenshot for VLM".to_string())
        })?;

        let candidate_block = candidates
            .iter()
            .map(|c| {
                let rect = c
                    .rect
                    .as_ref()
                    .map(|r| {
                        format!(
                            "{{x: {:.1}, y: {:.1}, w: {:.1}, h: {:.1}}}",
                            r.x, r.y, r.width, r.height
                        )
                    })
                    .unwrap_or_else(|| "<rect unavailable>".to_string());
                format!("- uid={}, snippet={}, rect={}", c.uid, c.snippet, rect)
            })
            .collect::<Vec<_>>()
            .join("\n");

        let user_text = format!(
            "Target label: \"{}\"\n\nCandidates:\n{}",
            target, candidate_block
        );

        let messages = vec![
            Message::system(DISAMBIGUATION_PROMPT),
            Message::user_with_images(user_text, vec![(prepared_b64, mime)]),
        ];

        let response = vlm
            .chat_with_options(&messages, None, &ChatOptions::with_temperature(0.0))
            .await
            .map_err(|e| ExecutorError::Cdp(format!("Disambiguation: VLM call failed: {}", e)))?;

        let raw = response
            .choices
            .first()
            .and_then(|c| c.message.content_text())
            .unwrap_or("")
            .to_string();

        parse_disambiguation_response(&raw).ok_or_else(|| {
            ExecutorError::Cdp(format!(
                "Disambiguation: failed to parse VLM response: {}",
                raw
            ))
        })
    }

    /// Save the screenshot PNG plus a JSON sidecar describing the
    /// disambiguation round into the node's `artifacts/` dir, and append the
    /// trace event. Returns the screenshot path relative to that dir.
    #[allow(clippy::too_many_arguments)]
    fn persist_disambiguation_artifacts(
        &self,
        node_name: &str,
        target: &str,
        chosen_uid: &str,
        reasoning: &str,
        candidates: &[CandidateView],
        viewport: Viewport,
        screenshot_b64: &str,
        mut node_run: Option<&mut NodeRun>,
    ) -> String {
        use base64::Engine;

        let short = &Uuid::new_v4().to_string()[..8];
        let screenshot_filename = format!("ambiguity_{}.png", short);
        let sidecar_filename = format!("ambiguity_{}.json", short);

        // Persist only when we have a trace-enabled NodeRun (same guard as
        // `save_result_images`). Even without persistence we still emit the
        // event; the UI will just fall back to a missing-screenshot placeholder.
        if let Some(run) = node_run.as_deref_mut()
            && run.trace_level != TraceLevel::Off
        {
            if let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(screenshot_b64) {
                match self.storage.save_artifact(
                    run,
                    ArtifactKind::Screenshot,
                    &screenshot_filename,
                    &bytes,
                    Value::Null,
                ) {
                    Ok(artifact) => run.artifacts.push(artifact),
                    Err(e) => tracing::warn!("Failed to save ambiguity screenshot: {}", e),
                }
            }

            let sidecar = SidecarRecord {
                target: target.to_string(),
                chosen_uid: chosen_uid.to_string(),
                reasoning: reasoning.to_string(),
                candidates: candidates.to_vec(),
                viewport_width: viewport.width,
                viewport_height: viewport.height,
                screenshot_path: screenshot_filename.clone(),
            };
            match serde_json::to_vec_pretty(&sidecar) {
                Ok(bytes) => match self.storage.save_artifact(
                    run,
                    ArtifactKind::Other,
                    &sidecar_filename,
                    &bytes,
                    Value::Null,
                ) {
                    Ok(artifact) => run.artifacts.push(artifact),
                    Err(e) => tracing::warn!("Failed to save ambiguity sidecar: {}", e),
                },
                Err(e) => tracing::warn!("Failed to serialize ambiguity sidecar: {}", e),
            }
        }

        // Append a structured trace event, whether or not the artifact writes
        // succeeded — events.jsonl lives at the node run level and is the
        // canonical record for post-run UI rendering.
        let payload = serde_json::json!({
            "node_name": node_name,
            "target": target,
            "chosen_uid": chosen_uid,
            "reasoning": reasoning,
            "candidates": candidates,
            "viewport_width": viewport.width,
            "viewport_height": viewport.height,
            "screenshot_path": screenshot_filename,
        });
        if let Some(run) = node_run.as_deref() {
            let event = TraceEvent {
                timestamp: Self::now_millis(),
                event_type: "ambiguity_resolved".to_string(),
                payload,
            };
            if let Err(e) = self.storage.append_event(run, &event) {
                tracing::warn!("Failed to append ambiguity_resolved event: {}", e);
            }
        }

        screenshot_filename
    }
}

/// Extract `{chosen_uid, reasoning}` from the VLM's reply, tolerating markdown
/// fences and a leading/trailing natural-language sentence.
fn parse_disambiguation_response(raw: &str) -> Option<(String, String)> {
    let json_str = super::app_resolve::parse_llm_json_response(raw)?;
    let value: Value = serde_json::from_str(json_str).ok()?;
    let chosen = value.get("chosen_uid")?.as_str()?.to_string();
    if chosen.is_empty() {
        return None;
    }
    let reasoning = value
        .get("reasoning")
        .and_then(|v| v.as_str())
        .unwrap_or("(no reasoning provided)")
        .to_string();
    Some((chosen, reasoning))
}

/// Viewport dimensions reported alongside the rects so the UI can translate
/// CDP-viewport coordinates into image-pixel coordinates when the captured
/// screenshot includes window chrome (OS title bar, browser toolbar, etc.).
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct Viewport {
    pub width: f64,
    pub height: f64,
}

/// Batched `Runtime.evaluate` that returns viewport dimensions plus a rect for
/// each candidate uid (or null for uids it can't locate). Falls back to
/// per-candidate nulls + zero-sized viewport when the CDP call fails.
async fn fetch_candidate_rects(
    candidates: &[CdpCandidate],
    mcp: &(impl Mcp + ?Sized),
) -> (Viewport, Vec<Option<Rect>>) {
    if candidates.is_empty() {
        return (Viewport::default(), Vec::new());
    }

    let uids_json = serde_json::to_string(
        &candidates
            .iter()
            .map(|c| c.uid.as_str())
            .collect::<Vec<_>>(),
    )
    .unwrap_or_else(|_| "[]".to_string());

    let js = format!(
        r#"(() => {{
  const uids = {};
  const rects = uids.map((uid) => {{
    try {{
      let el = document.querySelector('[data-uid="' + uid + '"]') ||
               document.querySelector('[uid="' + uid + '"]') ||
               (typeof __cwResolveUid === 'function' ? __cwResolveUid(uid) : null);
      if (!el) return null;
      const r = el.getBoundingClientRect();
      return {{ x: r.x, y: r.y, width: r.width, height: r.height }};
    }} catch (e) {{
      return null;
    }}
  }});
  return {{
    viewport: {{ width: window.innerWidth, height: window.innerHeight }},
    rects: rects,
  }};
}})()"#,
        uids_json
    );

    let args = serde_json::json!({ "function": js });
    let result = match mcp.call_tool("cdp_evaluate_script", Some(args)).await {
        Ok(r) if r.is_error != Some(true) => r,
        _ => return (Viewport::default(), vec![None; candidates.len()]),
    };

    let raw_text = result
        .content
        .iter()
        .filter_map(|c| match c {
            clickweave_mcp::ToolContent::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("");

    parse_candidate_rects_response(&raw_text, candidates.len())
}

/// Accept either the new `{viewport, rects}` envelope, a raw rects array, a
/// ```json-fenced array, or the `{"result": [...]}` wrapper chrome-devtools-mcp
/// sometimes emits. Returns exactly `expected_len` rects, padding with `None`
/// on parse failure. Viewport defaults to zeros when unavailable.
pub(crate) fn parse_candidate_rects_response(
    text: &str,
    expected_len: usize,
) -> (Viewport, Vec<Option<Rect>>) {
    let stripped = super::app_resolve::strip_code_block(text);
    let Ok(value): Result<Value, _> = serde_json::from_str(stripped) else {
        return (Viewport::default(), vec![None; expected_len]);
    };

    // Unwrap an optional `{"result": ...}` envelope first.
    let inner = value.get("result").unwrap_or(&value);

    // Case 1: `{ viewport: {w,h}, rects: [...] }`
    if let Some(obj) = inner.as_object()
        && let Some(rects_val) = obj.get("rects")
        && let Some(arr) = rects_val.as_array()
    {
        let viewport = obj
            .get("viewport")
            .and_then(|v| v.as_object())
            .and_then(|vp| {
                Some(Viewport {
                    width: vp.get("width")?.as_f64()?,
                    height: vp.get("height")?.as_f64()?,
                })
            })
            .unwrap_or_default();
        return (viewport, parse_rects_only(arr, expected_len));
    }

    // Case 2: a bare rects array (used by tests and legacy clients).
    if let Some(arr) = inner.as_array() {
        return (Viewport::default(), parse_rects_only(arr, expected_len));
    }

    (Viewport::default(), vec![None; expected_len])
}

fn parse_rects_only(arr: &[Value], expected_len: usize) -> Vec<Option<Rect>> {
    let mut out: Vec<Option<Rect>> = arr
        .iter()
        .map(|entry| {
            let obj = entry.as_object()?;
            Some(Rect {
                x: obj.get("x")?.as_f64()?,
                y: obj.get("y")?.as_f64()?,
                width: obj.get("width")?.as_f64()?,
                height: obj.get("height")?.as_f64()?,
            })
        })
        .collect();
    out.resize(expected_len, None);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_disambiguation_response_accepts_plain_json() {
        let (uid, reasoning) = parse_disambiguation_response(
            r#"{"chosen_uid": "a5", "reasoning": "primary Save button in toolbar"}"#,
        )
        .expect("should parse");
        assert_eq!(uid, "a5");
        assert!(reasoning.contains("toolbar"));
    }

    #[test]
    fn parse_disambiguation_response_accepts_fenced_json() {
        let (uid, _) = parse_disambiguation_response(
            "```json\n{\"chosen_uid\": \"a5\", \"reasoning\": \"x\"}\n```",
        )
        .expect("should parse fenced");
        assert_eq!(uid, "a5");
    }

    #[test]
    fn parse_disambiguation_response_rejects_missing_uid() {
        assert!(parse_disambiguation_response(r#"{"reasoning": "no uid"}"#).is_none());
    }

    #[test]
    fn parse_disambiguation_response_rejects_empty_uid() {
        assert!(parse_disambiguation_response(r#"{"chosen_uid": "", "reasoning": "x"}"#).is_none());
    }

    #[test]
    fn parse_disambiguation_response_fills_default_reasoning() {
        let (uid, reasoning) =
            parse_disambiguation_response(r#"{"chosen_uid": "a1"}"#).expect("should parse");
        assert_eq!(uid, "a1");
        assert!(reasoning.contains("no reasoning"));
    }

    #[test]
    fn parse_candidate_rects_response_accepts_bare_array() {
        let (vp, rects) = parse_candidate_rects_response(
            r#"[{"x": 1.0, "y": 2.0, "width": 3.0, "height": 4.0}, null]"#,
            2,
        );
        assert_eq!(rects.len(), 2);
        let r = rects[0].as_ref().expect("first rect");
        assert_eq!(r.x, 1.0);
        assert_eq!(r.width, 3.0);
        assert!(rects[1].is_none());
        // Bare arrays carry no viewport info.
        assert_eq!(vp.width, 0.0);
        assert_eq!(vp.height, 0.0);
    }

    #[test]
    fn parse_candidate_rects_response_pads_with_none_on_parse_failure() {
        let (_, rects) = parse_candidate_rects_response("not json", 3);
        assert_eq!(rects.len(), 3);
        assert!(rects.iter().all(|e| e.is_none()));
    }

    #[test]
    fn parse_candidate_rects_response_unwraps_result_envelope() {
        let (_, rects) = parse_candidate_rects_response(
            r#"{"result": [{"x": 10.0, "y": 20.0, "width": 30.0, "height": 40.0}]}"#,
            1,
        );
        let r = rects[0].as_ref().expect("rect present");
        assert_eq!(r.x, 10.0);
    }

    #[test]
    fn parse_candidate_rects_response_extracts_viewport_from_envelope() {
        let (vp, rects) = parse_candidate_rects_response(
            r#"{
                "viewport": {"width": 1280.0, "height": 720.0},
                "rects": [{"x": 5.0, "y": 6.0, "width": 7.0, "height": 8.0}]
            }"#,
            1,
        );
        assert_eq!(vp.width, 1280.0);
        assert_eq!(vp.height, 720.0);
        assert_eq!(rects[0].as_ref().unwrap().x, 5.0);
    }

    #[test]
    fn parse_candidate_rects_response_extracts_viewport_inside_result_wrapper() {
        let (vp, rects) = parse_candidate_rects_response(
            r#"{"result": {
                "viewport": {"width": 800.0, "height": 600.0},
                "rects": []
            }}"#,
            2,
        );
        assert_eq!(vp.width, 800.0);
        assert_eq!(vp.height, 600.0);
        assert_eq!(rects.len(), 2);
        assert!(rects.iter().all(|r| r.is_none()));
    }
}
