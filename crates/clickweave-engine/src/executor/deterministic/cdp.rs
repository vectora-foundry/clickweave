use std::path::Path;

use super::super::retry_context::RetryContext;
use super::super::{ExecutorError, ExecutorResult, Mcp, WorkflowExecutor};
use clickweave_core::NodeRun;
use clickweave_core::cdp::{
    SnapshotMatch, build_disambiguation_prompt, build_inventory_prompt_with_extras,
    find_interactive_in_snapshot, resolve_disambiguation_response, resolve_inventory_response,
};
use clickweave_llm::ChatBackend;
use uuid::Uuid;

/// A contenteditable or input element discovered via DOM query.
#[derive(Debug)]
struct ContenteditableElement {
    label: String,
    role: String,
    /// Center X coordinate in viewport pixels.
    cx: f64,
    /// Center Y coordinate in viewport pixels.
    cy: f64,
}

/// Expected CDP element attributes for matching during snapshot search.
#[derive(Debug, Default)]
pub(crate) struct CdpExpected<'a> {
    pub role: Option<&'a str>,
    pub href: Option<&'a str>,
    pub parent_role: Option<&'a str>,
    pub parent_name: Option<&'a str>,
}

impl<C: ChatBackend> WorkflowExecutor<C> {
    /// Resolve a text target to a CDP element UID via snapshot + find + disambiguate.
    ///
    /// Shared by both click and hover CDP paths. Returns the resolved element UID.
    pub(in crate::executor) async fn resolve_cdp_element_uid(
        &self,
        node_id: Uuid,
        target: &str,
        expected: &CdpExpected<'_>,
        mcp: &(impl Mcp + ?Sized),
        retry_ctx: &RetryContext,
    ) -> ExecutorResult<String> {
        // Refresh page list to verify CDP connection is healthy.
        let _ = mcp
            .call_tool("cdp_list_pages", Some(serde_json::json!({})))
            .await;

        // Take CDP snapshot
        self.log(format!("CDP: taking snapshot to find '{}'", target));
        let snapshot_result = mcp
            .call_tool("cdp_take_snapshot", Some(serde_json::json!({})))
            .await
            .map_err(|e| ExecutorError::Cdp(format!("take_snapshot failed: {e}")))?;

        if snapshot_result.is_error == Some(true) {
            let error_text = Self::extract_result_text(&snapshot_result);
            self.log(format!("CDP take_snapshot error: {}", error_text));
            return Err(ExecutorError::Cdp(format!(
                "take_snapshot error: {}",
                error_text
            )));
        }

        let snapshot_text = Self::extract_result_text(&snapshot_result);

        // Tier 0: contenteditable/input elements discovered via DOM query.
        // These are often invisible to the accessibility tree (generic with no
        // label) but have placeholder/aria-label attributes. Run this BEFORE
        // fuzzy snapshot matching to avoid false positives like "React to Message"
        // when the target is "Message" (the input placeholder).
        //
        // Strategy: ask the LLM which contenteditable matches the target,
        // then use cdp_element_at_point with that element's center coordinates
        // to get the exact uid (bypasses broken snapshot text search entirely).
        let ce_elements = self.query_contenteditable_elements_raw(mcp).await;
        if !ce_elements.is_empty() {
            self.log(format!(
                "CDP: checking {} contenteditable inputs for '{}'",
                ce_elements.len(),
                target
            ));

            // Ask the LLM which contenteditable matches the target.
            // Even with a single element, the LLM must confirm relevance —
            // e.g. "Search" input should not match target "Vesna".
            let matched_ce = {
                let options: Vec<String> = ce_elements
                    .iter()
                    .enumerate()
                    .map(|(i, e)| format!("{}. {} ({})", i + 1, e.label, e.role))
                    .collect();
                let prompt = format!(
                    "The user wants to interact with: \"{}\"\n\n\
                     Which of these input fields is the correct target? \
                     Reply with ONLY the label if one matches, or \"NONE\" \
                     if none of them match.\n\n{}",
                    target,
                    options.join("\n")
                );
                let response = self
                    .reasoning_backend()
                    .chat(vec![clickweave_llm::Message::user(prompt)], None)
                    .await;
                match response {
                    Ok(resp) => {
                        let text = resp
                            .choices
                            .first()
                            .and_then(|c| c.message.content_text())
                            .unwrap_or_default()
                            .trim()
                            .trim_matches('"')
                            .to_string();
                        // Strip role suffix if present (e.g. "Message (contenteditable)" → "Message")
                        let label = text.rfind(" (").map(|i| &text[..i]).unwrap_or(&text);
                        self.log(format!("CDP: LLM picked contenteditable '{}'", label));
                        ce_elements
                            .iter()
                            .find(|e| e.label.eq_ignore_ascii_case(label))
                    }
                    Err(_) => None,
                }
            };

            if let Some(ce) = matched_ce {
                if ce.cx > 0.0 && ce.cy > 0.0 {
                    self.log(format!(
                        "CDP: contenteditable '{}' at ({:.0}, {:.0}), resolving via element_at_point",
                        ce.label, ce.cx, ce.cy
                    ));
                    let eap_result = mcp
                        .call_tool(
                            "cdp_element_at_point",
                            Some(serde_json::json!({ "x": ce.cx, "y": ce.cy })),
                        )
                        .await;
                    if let Ok(ref result) = eap_result {
                        if result.is_error != Some(true) {
                            let text = Self::extract_result_text(result);
                            if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&text) {
                                if let Some(uid) = parsed["uid"].as_str() {
                                    let name = parsed["name"].as_str().unwrap_or("");
                                    let role = parsed["role"].as_str().unwrap_or("");
                                    if name.is_empty() && role == "generic" {
                                        // Nameless generic = container wrapping the
                                        // actual input. Focus it via JS and find the
                                        // focused element in a fresh snapshot.
                                        self.log(format!(
                                            "CDP: element_at_point returned nameless generic for '{}', focusing via JS",
                                            ce.label
                                        ));
                                        if let Some(focused_uid) =
                                            self.focus_contenteditable_via_js(&ce.label, mcp).await
                                        {
                                            return Ok(focused_uid);
                                        }
                                    } else {
                                        self.log(format!(
                                            "CDP: contenteditable resolved '{}' -> uid='{}'",
                                            target, uid
                                        ));
                                        return Ok(uid.to_string());
                                    }
                                }
                            }
                        }
                    }
                    self.log(format!(
                        "CDP: cdp_element_at_point failed for contenteditable '{}', continuing",
                        ce.label
                    ));
                }
            } else {
                self.log("CDP: no contenteditable matched target, continuing");
            }
        }
        let contenteditable_inputs: Vec<String> = ce_elements
            .iter()
            .map(|e| format!("{} ({})", e.label, e.role))
            .collect();

        // Find matching elements, preferring interactive roles (buttons, textboxes, etc.)
        // over non-interactive ones (images, headings) when both match.
        let mut matches = find_interactive_in_snapshot(&snapshot_text, target);
        clickweave_core::cdp::narrow_matches(&mut matches, expected.role, expected.href);
        clickweave_core::cdp::narrow_by_parent(
            &mut matches,
            expected.parent_role,
            expected.parent_name,
        );

        if matches.is_empty() {
            // Tier 2: VLM visual resolution (Test mode only, requires vision backend).
            if self.execution_mode == clickweave_core::ExecutionMode::Test {
                self.log(format!("CDP: attempting VLM resolution for '{}'", target));
                match self.vlm_identify_and_locate(target, mcp).await {
                    None => {
                        self.log(format!(
                            "CDP: VLM resolution returned None for '{}'",
                            target
                        ));
                    }
                    Some((screen_x, screen_y)) => {
                        self.log(format!(
                            "CDP: VLM located '{}', trying cdp_element_at_point at ({:.0}, {:.0})",
                            target, screen_x, screen_y
                        ));
                        let eap_result = mcp
                            .call_tool(
                                "cdp_element_at_point",
                                Some(serde_json::json!({ "x": screen_x, "y": screen_y })),
                            )
                            .await;

                        if let Ok(ref result) = eap_result {
                            if result.is_error != Some(true) {
                                let text = Self::extract_result_text(result);
                                if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&text)
                                {
                                    if let Some(uid) = parsed["uid"].as_str() {
                                        let name = parsed["name"].as_str().unwrap_or("");
                                        self.log(format!(
                                            "CDP: VLM resolved '{}' -> uid='{}' name='{}'",
                                            target, uid, name
                                        ));

                                        // Cache the resolved name for Run mode replay.
                                        if !name.is_empty() {
                                            let app = self.focused_app_name();
                                            let key = clickweave_core::decision_cache::cache_key(
                                                node_id,
                                                target,
                                                app.as_deref(),
                                            );
                                            self.write_decision_cache().element_resolution.insert(
                                            key,
                                            clickweave_core::decision_cache::ElementResolution {
                                                target: target.to_string(),
                                                resolved_name: name.to_string(),
                                            },
                                        );
                                        }

                                        return Ok(uid.to_string());
                                    }
                                }
                            }
                        }

                        // cdp_element_at_point failed — fall through to inventory resolution.
                        // Don't return CdpNativeClickFallback here because this resolver
                        // is shared by click and hover; native-click fallback only makes
                        // sense for click and is handled at the call site.
                        self.log(format!(
                            "CDP: cdp_element_at_point failed for '{}', trying inventory",
                            target
                        ));
                    }
                }
            } else {
                self.log(format!(
                    "CDP: skipping VLM (execution_mode={:?}, not Test)",
                    self.execution_mode
                ));
            }

            // Check decision cache for VLM-resolved name from a prior Test run.
            // Skip when force_resolve is set (retry after eviction), and
            // remove the stale persistent entry so it doesn't replay later.
            let app = self.focused_app_name();
            let ck = clickweave_core::decision_cache::cache_key(node_id, target, app.as_deref());
            if retry_ctx.force_resolve {
                if self
                    .write_decision_cache()
                    .element_resolution
                    .remove(&ck)
                    .is_some()
                {
                    self.log(format!(
                        "CDP: evicted stale element_resolution cache for '{}'",
                        target
                    ));
                }
            } else {
                let cached_resolution = {
                    self.read_decision_cache()
                        .element_resolution
                        .get(&ck)
                        .cloned()
                };
                if let Some(cached) = cached_resolution {
                    self.log(format!(
                        "CDP: trying cached VLM resolution '{}' -> '{}'",
                        target, cached.resolved_name
                    ));
                    // Use exact-only matching for cached replay to prevent
                    // substring false positives (e.g. "Reply" matching "Reply all").
                    let cached_matches: Vec<_> =
                        find_interactive_in_snapshot(&snapshot_text, &cached.resolved_name)
                            .into_iter()
                            .filter(|m| m.label.eq_ignore_ascii_case(&cached.resolved_name))
                            .collect();
                    if cached_matches.len() == 1 {
                        return Ok(cached_matches[0].uid.clone());
                    }
                    // 0 or 2+ exact matches — stale or ambiguous cache, fall through.
                    self.log(format!(
                        "CDP: cached name '{}' had {} exact matches, falling through",
                        cached.resolved_name,
                        cached_matches.len()
                    ));
                }
            }

            // Existing inventory resolution fallback (reuses contenteditable
            // inputs already queried above).
            self.log(format!(
                "CDP: no exact match for '{}', resolving via element inventory",
                target
            ));
            let mut resolved = self
                .resolve_via_inventory(target, &snapshot_text, &contenteditable_inputs)
                .await?;
            clickweave_core::cdp::narrow_matches(&mut resolved, expected.role, expected.href);
            clickweave_core::cdp::narrow_by_parent(
                &mut resolved,
                expected.parent_role,
                expected.parent_name,
            );
            if resolved.is_empty() {
                Err(ExecutorError::Cdp(format!(
                    "No matching elements for '{}' after inventory resolution",
                    target
                )))
            } else if resolved.len() == 1 {
                self.log(format!(
                    "CDP: inventory resolved '{}' -> uid='{}'",
                    target, resolved[0].uid
                ));
                Ok(resolved[0].uid.clone())
            } else {
                self.log(format!(
                    "CDP: inventory found {} matches for '{}', disambiguating",
                    resolved.len(),
                    target
                ));
                self.disambiguate_cdp_elements(target, &resolved, retry_ctx)
                    .await
            }
        } else if matches.len() == 1 {
            Ok(matches[0].uid.clone())
        } else {
            self.log(format!(
                "CDP: {} matches for '{}', disambiguating",
                matches.len(),
                target
            ));
            self.disambiguate_cdp_elements(target, &matches, retry_ctx)
                .await
        }
    }

    /// Sentinel UID returned when a contenteditable element was focused
    /// directly via JS — the caller should skip the `cdp_click` because the
    /// element already has DOM focus.
    const FOCUSED_VIA_JS: &'static str = "__focused_via_js__";

    /// Resolve a CDP element and perform an action (click or hover) on it.
    /// Returns the action result text.
    pub(in crate::executor) async fn execute_cdp_action(
        &self,
        action: &str,
        node_id: Uuid,
        target: &str,
        expected: &CdpExpected<'_>,
        mcp: &(impl Mcp + ?Sized),
        node_run: Option<&NodeRun>,
        retry_ctx: &RetryContext,
    ) -> ExecutorResult<String> {
        let uid = self
            .resolve_cdp_element_uid(node_id, target, expected, mcp, retry_ctx)
            .await?;

        // Contenteditable elements focused via JS don't have a clickable
        // UID — skip the action to avoid stealing focus.
        if uid == Self::FOCUSED_VIA_JS {
            self.log(format!(
                "CDP: '{}' already focused via JS, skipping {}",
                target, action
            ));
            self.record_event(
                node_run,
                &format!("cdp_{}", action),
                serde_json::json!({ "target": target, "uid": uid }),
            );
            return Ok(format!("Focused '{}' via JS", target));
        }

        self.log(format!("CDP: {} element uid='{}'", action, uid));
        let result = mcp
            .call_tool(
                &format!("cdp_{action}"),
                Some(serde_json::json!({ "uid": uid })),
            )
            .await
            .map_err(|e| ExecutorError::Cdp(format!("{} failed: {e}", action)))?;

        if result.is_error == Some(true) {
            return Err(ExecutorError::Cdp(format!(
                "{} error: {}",
                action,
                Self::extract_result_text(&result)
            )));
        }

        self.record_event(
            node_run,
            &format!("cdp_{}", action),
            serde_json::json!({ "target": target, "uid": uid }),
        );

        Ok(Self::extract_result_text(&result))
    }

    /// Resolve a CDP element and click it. Returns the click result text.
    pub(in crate::executor) async fn resolve_and_click_cdp(
        &self,
        node_id: Uuid,
        target: &str,
        expected: &CdpExpected<'_>,
        mcp: &(impl Mcp + ?Sized),
        node_run: Option<&NodeRun>,
        retry_ctx: &RetryContext,
    ) -> ExecutorResult<String> {
        self.execute_cdp_action("click", node_id, target, expected, mcp, node_run, retry_ctx)
            .await
    }

    /// Resolve a CDP element and hover it. Returns the hover result text.
    pub(in crate::executor) async fn resolve_and_hover_cdp(
        &self,
        node_id: Uuid,
        target: &str,
        expected: &CdpExpected<'_>,
        mcp: &(impl Mcp + ?Sized),
        node_run: Option<&NodeRun>,
        retry_ctx: &RetryContext,
    ) -> ExecutorResult<String> {
        self.execute_cdp_action("hover", node_id, target, expected, mcp, node_run, retry_ctx)
            .await
    }

    /// Focus a contenteditable element by label via JS.
    ///
    /// Contenteditable elements are often invisible in the accessibility tree
    /// (nameless generic containers all the way up). Instead of trying to
    /// resolve a UID, we focus the element directly via JS and return the
    /// `FOCUSED_VIA_JS` sentinel so the caller skips `cdp_click`.
    async fn focus_contenteditable_via_js(
        &self,
        label: &str,
        mcp: &(impl Mcp + ?Sized),
    ) -> Option<String> {
        let focus_js = format!(
            r#"() => {{
                const all = document.querySelectorAll('*');
                for (const el of all) {{
                    if (!el.isContentEditable || !el.parentElement || el.parentElement.isContentEditable) continue;
                    const lbl = el.getAttribute('placeholder') || el.getAttribute('aria-label') || el.getAttribute('data-placeholder') || '';
                    if (lbl === '{}') {{ el.focus(); el.click(); return true; }}
                }}
                return false;
            }}"#,
            label.replace('\\', "\\\\").replace('\'', "\\'")
        );
        let focus_result = mcp
            .call_tool(
                "cdp_evaluate_script",
                Some(serde_json::json!({ "function": focus_js })),
            )
            .await;
        match focus_result {
            Ok(ref r) if r.is_error != Some(true) => {
                let text = Self::extract_result_text(r);
                if text.contains("true") {
                    self.log(format!("CDP: focused contenteditable '{}' via JS", label));
                    return Some(Self::FOCUSED_VIA_JS.to_string());
                }
            }
            _ => {}
        }
        self.log("CDP: JS focus call failed for contenteditable");
        None
    }

    /// Query the DOM for contenteditable elements, returning full metadata
    /// including bounding rect center for coordinate-based resolution.
    async fn query_contenteditable_elements_raw(
        &self,
        mcp: &(impl Mcp + ?Sized),
    ) -> Vec<ContenteditableElement> {
        // Walk all DOM elements and find editable ones (contenteditable,
        // textarea, text inputs). CSS selectors miss inherited contenteditable
        // (e.g. Quill editors), so we check isContentEditable on each element
        // and skip children of editable parents to avoid duplicates.
        let js = r#"() => {
            const results = [];
            // cdp_element_at_point expects screen coordinates (points), so
            // convert getBoundingClientRect viewport coords to screen coords.
            const chromeH = window.outerHeight - window.innerHeight;
            const all = document.querySelectorAll('*');
            for (const el of all) {
                const isInput = el.tagName === 'TEXTAREA' ||
                    (el.tagName === 'INPUT' && (!el.type || el.type === 'text' || el.type === 'search' || el.type === 'url' || el.type === 'email'));
                const isEditable = el.isContentEditable && el.parentElement && !el.parentElement.isContentEditable;
                if (!isInput && !isEditable) continue;
                const label = el.getAttribute('placeholder') || el.getAttribute('aria-label') || el.getAttribute('data-placeholder') || '';
                if (!label) continue;
                const rect = el.getBoundingClientRect();
                results.push({
                    label,
                    role: isEditable ? 'contenteditable' : el.tagName.toLowerCase(),
                    cx: Math.round(window.screenX + rect.left + rect.width / 2),
                    cy: Math.round(window.screenY + chromeH + rect.top + rect.height / 2)
                });
            }
            return results;
        }"#;

        match mcp
            .call_tool(
                "cdp_evaluate_script",
                Some(serde_json::json!({ "function": js })),
            )
            .await
        {
            Ok(result) if result.is_error != Some(true) => {
                let text = Self::extract_result_text(&result);
                // Parse JSON array from the result (may be wrapped in markdown fences).
                let json_text = text
                    .trim()
                    .strip_prefix("```json")
                    .unwrap_or(&text)
                    .strip_prefix("```")
                    .unwrap_or(&text)
                    .strip_suffix("```")
                    .unwrap_or(&text)
                    .trim();

                if let Ok(entries) = serde_json::from_str::<Vec<serde_json::Value>>(json_text) {
                    let elements: Vec<ContenteditableElement> = entries
                        .iter()
                        .filter_map(|e| {
                            let label = e.get("label")?.as_str()?.to_string();
                            let role = e.get("role")?.as_str().unwrap_or("input").to_string();
                            if label.is_empty() {
                                return None;
                            }
                            let cx = e.get("cx").and_then(|v| v.as_f64()).unwrap_or(0.0);
                            let cy = e.get("cy").and_then(|v| v.as_f64()).unwrap_or(0.0);
                            Some(ContenteditableElement {
                                label,
                                role,
                                cx,
                                cy,
                            })
                        })
                        .collect();
                    if !elements.is_empty() {
                        let display: Vec<String> = elements
                            .iter()
                            .map(|e| format!("{} ({})", e.label, e.role))
                            .collect();
                        self.log(format!(
                            "CDP: found {} contenteditable elements via JS: {}",
                            elements.len(),
                            display.join(", ")
                        ));
                    }
                    elements
                } else {
                    Vec::new()
                }
            }
            _ => Vec::new(),
        }
    }

    /// Resolve a target with no direct matches by showing the LLM a compact
    /// element inventory and asking it to pick the best label, then searching
    /// the snapshot for that label to get structured matches with ancestors.
    async fn resolve_via_inventory(
        &self,
        target: &str,
        snapshot_text: &str,
        extra_inputs: &[String],
    ) -> ExecutorResult<Vec<SnapshotMatch>> {
        let prompt = build_inventory_prompt_with_extras(target, snapshot_text, extra_inputs)
            .ok_or_else(|| {
                ExecutorError::Cdp(format!(
                    "No interactive elements found in CDP snapshot for '{}'",
                    target
                ))
            })?;

        let response = self
            .reasoning_backend()
            .chat(vec![clickweave_llm::Message::user(prompt)], None)
            .await
            .map_err(|e| ExecutorError::Cdp(format!("LLM inventory resolution failed: {e}")))?;

        let raw_text = response
            .choices
            .first()
            .and_then(|c| c.message.content_text())
            .ok_or_else(|| ExecutorError::Cdp("LLM returned empty content".to_string()))?;

        let resolved_label = raw_text.trim().trim_matches('"');
        self.log(format!(
            "CDP: inventory resolved '{}' -> '{}'",
            target, resolved_label
        ));

        resolve_inventory_response(target, raw_text, snapshot_text).map_err(ExecutorError::Cdp)
    }

    /// Disambiguate between multiple CDP element matches using the LLM.
    async fn disambiguate_cdp_elements(
        &self,
        target: &str,
        matches: &[SnapshotMatch],
        retry_ctx: &RetryContext,
    ) -> ExecutorResult<String> {
        let hint = retry_ctx.supervision_hint.as_deref();
        let tried: Vec<String> = retry_ctx.read_tried_cdp_uids().clone();
        let prompt = build_disambiguation_prompt(target, matches, hint, &tried);

        let response = self
            .reasoning_backend()
            .chat(vec![clickweave_llm::Message::user(prompt)], None)
            .await
            .map_err(|e| ExecutorError::Cdp(format!("LLM disambiguation failed: {e}")))?;

        let raw_text = response
            .choices
            .first()
            .and_then(|c| c.message.content_text())
            .unwrap_or_default();

        let uid = resolve_disambiguation_response(raw_text, matches);
        if raw_text.trim().trim_matches('"') != uid || uid == matches[0].uid.as_str() {
            // LLM returned invalid uid — we fell back to first match
            if raw_text.trim().trim_matches('"') != uid {
                self.log(format!(
                    "CDP: LLM returned '{}' which is not in candidate set, using first match",
                    raw_text.trim()
                ));
            }
        }
        retry_ctx.write_tried_cdp_uids().push(uid.clone());
        Ok(uid)
    }

    /// Use VLM to identify what text the target element shows, then use
    /// `find_text` to get precise screen coordinates via OCR.
    ///
    /// The VLM is good at semantic understanding ("what does the message input
    /// say?") but bad at pixel coordinates. OCR is precise at locating known
    /// text. This combines both strengths.
    async fn vlm_identify_and_locate(
        &self,
        target: &str,
        mcp: &(impl Mcp + ?Sized),
    ) -> Option<(f64, f64)> {
        let vlm = match self.vision_backend() {
            Some(v) => v,
            None => {
                self.log("VLM identify: no vision backend available".to_string());
                return None;
            }
        };

        // Take screenshot for VLM analysis.
        let screenshot = match self.capture_screenshot_with_metadata(mcp).await {
            Some(s) => s,
            None => {
                self.log("VLM identify: screenshot capture failed".to_string());
                return None;
            }
        };

        let (prepared_b64, mime) = match clickweave_llm::prepare_base64_image_for_vlm(
            &screenshot.image_base64,
            clickweave_llm::DEFAULT_MAX_DIMENSION,
        ) {
            Some(pair) => pair,
            None => {
                self.log("VLM identify: image preparation failed".to_string());
                return None;
            }
        };

        // Ask VLM to read the actual visible text on the target element.
        let prompt = format!(
            "The user wants to interact with a UI element described as \"{}\".\n\
             Look at this screenshot and find that element.\n\
             What exact text is currently visible on or inside that element?\n\
             This could be placeholder text, a label, or a button caption.\n\
             Return ONLY a JSON object: {{\"text\": \"<the exact visible text>\"}}\n\
             If the element is not visible or has no text, return: {{\"text\": null}}",
            target
        );

        let messages = vec![clickweave_llm::Message::user_with_images(
            prompt,
            vec![(prepared_b64, mime)],
        )];

        let response = match vlm.chat(messages, None).await {
            Ok(r) => r,
            Err(e) => {
                self.log(format!("VLM identify: chat failed: {}", e));
                return None;
            }
        };
        let raw = match response
            .choices
            .first()
            .and_then(|c| c.message.content_text())
        {
            Some(t) => t,
            None => {
                self.log("VLM identify: empty response".to_string());
                return None;
            }
        };

        self.log(format!("VLM identify: raw response: {}", raw));

        // Extract the text the VLM identified.
        let visible_text = Self::parse_vlm_text_response(raw)?;
        self.log(format!(
            "VLM identify: element '{}' shows text '{}'",
            target, visible_text
        ));

        // Use find_text to get precise screen coordinates via OCR.
        let app_name = self.focused_app_name();
        let mut args = serde_json::json!({ "text": visible_text });
        if let Some(ref name) = app_name {
            args["app_name"] = serde_json::Value::String(name.clone());
        }

        let result = match mcp.call_tool("find_text", Some(args)).await {
            Ok(r) => r,
            Err(e) => {
                self.log(format!("VLM identify: find_text failed: {}", e));
                return None;
            }
        };

        if result.is_error == Some(true) {
            self.log(format!(
                "VLM identify: find_text error: {}",
                Self::extract_result_text(&result)
            ));
            return None;
        }

        // Parse find_text result to get coordinates of the first match.
        let result_text = Self::extract_result_text(&result);
        let matches: Vec<serde_json::Value> = match serde_json::from_str(&result_text) {
            Ok(m) => m,
            Err(_) => {
                self.log(format!(
                    "VLM identify: could not parse find_text result: {}",
                    &result_text[..result_text.len().min(200)]
                ));
                return None;
            }
        };

        let first = matches.first()?;
        let x = first["x"].as_f64()?;
        let y = first["y"].as_f64()?;

        self.log(format!(
            "VLM identify: find_text located '{}' at ({:.0}, {:.0})",
            visible_text, x, y
        ));

        Some((x, y))
    }

    /// Parse the VLM's text identification response. Extracts the `text` field
    /// from a JSON response like `{"text": "Message"}`.
    fn parse_vlm_text_response(raw: &str) -> Option<String> {
        // Try JSON first.
        if let Some(json_text) = crate::executor::app_resolve::parse_llm_json_response(raw) {
            if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(json_text) {
                if let Some(text) = parsed["text"].as_str() {
                    if !text.is_empty() && text != "null" {
                        return Some(text.to_string());
                    }
                }
            }
        }

        // Fallback: if the response is just a quoted string, use it.
        let trimmed = raw.trim().trim_matches('"').trim();
        if !trimmed.is_empty() && !trimmed.contains('{') && !trimmed.eq_ignore_ascii_case("null") {
            return Some(trimmed.to_string());
        }

        None
    }

    /// Ensure a CDP connection is available for the given Electron/Chrome app.
    ///
    /// If no CDP connection is active for this app:
    /// - Test mode: quit the app, relaunch with --remote-debugging-port, connect
    ///   via cdp_connect, poll until ready, store port in cache.
    /// - Run mode: read port from decision cache, try connecting, relaunch if needed.
    ///
    /// `pid` identifies the specific app instance within this execution. Pass `0`
    /// when the PID is not yet known (e.g. immediately after launch).
    pub(in crate::executor) async fn ensure_cdp_connected(
        &mut self,
        _node_id: Uuid,
        app_name: &str,
        pid: i32,
        mcp: &(impl Mcp + ?Sized),
        node_run: Option<&NodeRun>,
        chrome_profile_path: Option<&Path>,
    ) -> ExecutorResult<()> {
        use clickweave_core::ExecutionMode;
        use clickweave_core::decision_cache::CdpPort;

        // Already have a CDP connection for this exact app instance -- nothing to do.
        // Note: the CdpPort decision cache key is app-name-only for cross-run stability;
        // PIDs change between launches and cannot be used as a persistent cache key.
        if let Some((ref connected_name, connected_pid)) = self.cdp_connected_app
            && connected_name == app_name
            && connected_pid == pid
        {
            return Ok(());
        }

        // Disconnect from any previously connected app.
        if self.cdp_connected_app.is_some() {
            let _ = mcp.call_tool("cdp_disconnect", None).await;
            self.cdp_connected_app = None;
        }

        let port = if self.execution_mode == ExecutionMode::Test {
            // Try reusing an existing debug port before doing a full relaunch.
            // Skip reuse when an explicit Chrome profile is provided — we need
            // a fresh instance with that profile's --user-data-dir, not whatever
            // Chrome is currently running.
            let reused = if chrome_profile_path.is_none() {
                if let Some(existing_port) = existing_debug_port(app_name).await {
                    self.log(format!(
                        "'{}' already running with --remote-debugging-port={}, reusing",
                        app_name, existing_port
                    ));
                    if self.try_cdp_connect(app_name, existing_port, mcp).await {
                        self.write_decision_cache().cdp_port.insert(
                            app_name.to_string(),
                            CdpPort {
                                port: existing_port,
                            },
                        );
                        Some(existing_port)
                    } else {
                        self.log(format!(
                            "Existing debug port {} for '{}' was unreachable, relaunching",
                            existing_port, app_name
                        ));
                        None
                    }
                } else {
                    None
                }
            } else {
                None
            };

            if let Some(port) = reused {
                port
            } else {
                let port = clickweave_core::cdp::rand_ephemeral_port();
                self.log(format!(
                    "Restarting '{}' with DevTools enabled (port {})...",
                    app_name, port
                ));
                self.relaunch_with_debug_port(app_name, port, mcp, chrome_profile_path)
                    .await?;
                self.evict_app_cache(app_name);
                self.write_decision_cache()
                    .cdp_port
                    .insert(app_name.to_string(), CdpPort { port });
                self.cdp_connect_and_poll(app_name, port, mcp).await?;
                port
            }
        } else {
            // Run mode: read cached port, try connecting, relaunch if needed.
            let port = self
                .read_decision_cache()
                .cdp_port
                .get(app_name)
                .map(|e| e.port)
                .ok_or_else(|| {
                    ExecutorError::Cdp(format!(
                        "No cached CDP port for '{}'. Run in Test mode first.",
                        app_name
                    ))
                })?;

            if !self.try_cdp_connect(app_name, port, mcp).await {
                self.log(format!(
                    "CDP connection failed for '{}', relaunching with port {}...",
                    app_name, port
                ));
                self.relaunch_with_debug_port(app_name, port, mcp, chrome_profile_path)
                    .await?;
                self.evict_app_cache(app_name);
                self.cdp_connect_and_poll(app_name, port, mcp).await?;
            }
            port
        };

        self.log(format!("CDP connected to '{}' (port {})", app_name, port));
        self.record_event(
            node_run,
            "cdp_connected",
            serde_json::json!({
                "app_name": app_name,
                "port": port,
            }),
        );

        self.cdp_connected_app = Some((app_name.to_string(), pid));
        Ok(())
    }

    /// Connect to CDP with retries (the debug endpoint may not be ready
    /// immediately after app launch), then poll until pages are available.
    async fn cdp_connect_and_poll(
        &self,
        app_name: &str,
        port: u16,
        mcp: &(impl Mcp + ?Sized),
    ) -> ExecutorResult<()> {
        let connect_args = serde_json::json!({"port": port});
        let mut last_err = String::new();
        for attempt in 0..10 {
            if attempt > 0 {
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            }
            match mcp
                .call_tool("cdp_connect", Some(connect_args.clone()))
                .await
            {
                Ok(r) if r.is_error != Some(true) => {
                    return self.poll_cdp_ready(app_name, mcp, 30).await;
                }
                Ok(r) => {
                    last_err = Self::extract_result_text(&r);
                    tracing::debug!(
                        "cdp_connect attempt {} for '{}': {}",
                        attempt + 1,
                        app_name,
                        last_err
                    );
                }
                Err(e) => {
                    last_err = e.to_string();
                    tracing::debug!(
                        "cdp_connect attempt {} for '{}': {}",
                        attempt + 1,
                        app_name,
                        last_err
                    );
                }
            }
        }
        Err(ExecutorError::Cdp(format!(
            "Failed to connect CDP for '{}' after 10 attempts: {}",
            app_name, last_err
        )))
    }

    /// Try to connect CDP to an app, returning true on success.
    /// Disconnects on failure to avoid leaving a stale connection.
    async fn try_cdp_connect(&self, app_name: &str, port: u16, mcp: &(impl Mcp + ?Sized)) -> bool {
        let ok = matches!(
            mcp.call_tool("cdp_connect", Some(serde_json::json!({"port": port})))
                .await,
            Ok(r) if r.is_error != Some(true)
        );
        if !ok {
            return false;
        }
        if self.poll_cdp_ready(app_name, mcp, 5).await.is_ok() {
            true
        } else {
            let _ = mcp.call_tool("cdp_disconnect", None).await;
            false
        }
    }

    /// Quit the app, confirm it exited, relaunch with --remote-debugging-port.
    ///
    /// For Chrome-family apps with a configured profile: kills only the
    /// profile-specific Chrome instance and launches directly, leaving the
    /// user's default Chrome untouched.
    async fn relaunch_with_debug_port(
        &self,
        app_name: &str,
        port: u16,
        mcp: &(impl Mcp + ?Sized),
        chrome_profile_path: Option<&Path>,
    ) -> ExecutorResult<()> {
        let is_chrome = {
            let lower = app_name.to_lowercase();
            lower.contains("chrome") || lower.contains("chromium")
        };

        if let (true, Some(profile_path)) = (is_chrome, chrome_profile_path) {
            // Chrome with a configured profile: kill only the profile-specific
            // instance, then launch directly (bypasses MCP launch_app which
            // refuses when any Chrome is already running).
            let dir = profile_path.to_string_lossy().to_string();
            super::kill_chrome_profile_instance(&dir).await;

            super::launch_chrome_with_profile_and_debug_port(&dir, port)
                .await
                .map_err(|e| {
                    ExecutorError::Cdp(format!(
                        "Failed to launch '{}' with debug port: {}",
                        app_name, e
                    ))
                })?;
        } else {
            // Non-Chrome / no profile: quit via MCP, then relaunch via MCP.
            let quit_args = serde_json::json!({ "app_name": app_name });
            if let Err(e) = mcp.call_tool("quit_app", Some(quit_args)).await {
                self.log(format!(
                    "quit_app for '{}' failed (continuing): {}",
                    app_name, e
                ));
            }

            let poll_args = serde_json::json!({ "app_name": app_name, "user_apps_only": true });
            let mut quit_confirmed = false;
            for _ in 0..20 {
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                if let Ok(r) = mcp.call_tool("list_apps", Some(poll_args.clone())).await {
                    let text = Self::extract_result_text(&r);
                    if text.trim() == "[]" {
                        quit_confirmed = true;
                        break;
                    }
                }
            }

            if !quit_confirmed {
                self.log(format!(
                    "'{}' did not quit within 10s, force-killing",
                    app_name
                ));
                let force_args = serde_json::json!({ "app_name": app_name, "force": true });
                let _ = mcp.call_tool("quit_app", Some(force_args)).await;
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            }

            kill_all_processes(app_name).await;

            let args = vec![format!("--remote-debugging-port={}", port)];
            let launch_args = serde_json::json!({
                "app_name": app_name,
                "args": args,
            });
            let result = mcp
                .call_tool("launch_app", Some(launch_args))
                .await
                .map_err(|e| {
                    ExecutorError::Cdp(format!(
                        "Failed to launch '{}' with debug port: {}",
                        app_name, e
                    ))
                })?;

            if result.is_error == Some(true) {
                return Err(ExecutorError::Cdp(format!(
                    "launch_app error for '{}': {}",
                    app_name,
                    Self::extract_result_text(&result)
                )));
            }
        }

        // Wait for the app to start up.
        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
        Ok(())
    }

    /// Poll `list_pages` until it returns at least one page.
    pub(in crate::executor) async fn poll_cdp_ready(
        &self,
        app_name: &str,
        mcp: &(impl Mcp + ?Sized),
        timeout_secs: u64,
    ) -> ExecutorResult<()> {
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(timeout_secs);

        loop {
            match mcp
                .call_tool("cdp_list_pages", Some(serde_json::json!({})))
                .await
            {
                Ok(result) if result.is_error != Some(true) => {
                    let text = Self::extract_result_text(&result);
                    // Check for page entries in the response. Native-devtools
                    // uses "[N] url" format; accept any line with a bracketed index.
                    if text.lines().any(|l| {
                        let t = l.trim_start();
                        t.starts_with('[') && t.contains(']')
                    }) {
                        self.log(format!("CDP pages for '{}': {}", app_name, text.trim()));
                        return Ok(());
                    }
                    tracing::debug!(
                        "CDP list_pages for '{}' returned but no pages yet: {:?}",
                        app_name,
                        &text[..text.len().min(500)]
                    );
                }
                Ok(result) => {
                    let text = Self::extract_result_text(&result);
                    tracing::debug!(
                        "CDP list_pages error for '{}': {}",
                        app_name,
                        &text[..text.len().min(500)]
                    );
                }
                Err(e) => {
                    tracing::debug!("CDP list_pages call failed for '{}': {}", app_name, e);
                }
            }

            if tokio::time::Instant::now() >= deadline {
                return Err(ExecutorError::Cdp(format!(
                    "Timed out waiting for CDP to be ready for '{}' ({}s)",
                    app_name, timeout_secs
                )));
            }

            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        }
    }
}

/// Check if an app is already running with `--remote-debugging-port=<N>`.
/// Returns the port if found, so the caller can skip the quit/relaunch cycle.
async fn existing_debug_port(app_name: &str) -> Option<u16> {
    #[cfg(target_os = "windows")]
    return None;

    #[cfg(not(target_os = "windows"))]
    {
        let output = tokio::process::Command::new("pgrep")
            .args(["-x", app_name])
            .output()
            .await
            .ok()?;
        if !output.status.success() {
            tracing::info!(
                "existing_debug_port: pgrep -x '{}' found no processes",
                app_name
            );
            return None;
        }
        let pids = String::from_utf8_lossy(&output.stdout);
        tracing::info!(
            "existing_debug_port: pgrep -x '{}' found pids: {}",
            app_name,
            pids.trim()
        );
        for pid_str in pids.split_whitespace() {
            // The PID may have exited between pgrep and ps (TOCTOU); skip it
            // rather than returning None from the whole function.
            let Ok(args_output) = tokio::process::Command::new("ps")
                .args(["-p", pid_str, "-o", "args="])
                .output()
                .await
            else {
                continue;
            };
            let args = String::from_utf8_lossy(&args_output.stdout);
            tracing::info!("existing_debug_port: pid {} args: {}", pid_str, args.trim());
            if let Some(flag) = args
                .split_whitespace()
                .find(|a| a.starts_with("--remote-debugging-port="))
                && let Some(port_str) = flag.strip_prefix("--remote-debugging-port=")
                && let Ok(port) = port_str.parse::<u16>()
            {
                tracing::info!(
                    "existing_debug_port: found port {} for '{}'",
                    port,
                    app_name
                );
                return Some(port);
            }
        }
        tracing::info!(
            "existing_debug_port: no debug port found for '{}'",
            app_name
        );
        None
    }
}

/// Kill all processes matching `app_name` and wait for them to exit (up to 5s).
/// Used to ensure multi-process apps (e.g. Chrome) fully release their profile
/// lock before we relaunch with --remote-debugging-port.
async fn kill_all_processes(app_name: &str) {
    #[cfg(not(target_os = "windows"))]
    {
        // Anchor to the .app bundle path on macOS to avoid killing unrelated
        // processes that happen to mention the app name in their arguments.
        #[cfg(target_os = "macos")]
        let pattern = format!("{}.app/", app_name);
        #[cfg(not(target_os = "macos"))]
        let pattern = app_name.to_string();

        let _ = tokio::process::Command::new("pkill")
            .args(["-f", &pattern])
            .output()
            .await;

        for _ in 0..10 {
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            let still_alive = tokio::process::Command::new("pgrep")
                .args(["-f", &pattern])
                .output()
                .await
                .map(|o| o.status.success())
                .unwrap_or(false);
            if !still_alive {
                break;
            }
        }
    }
    #[cfg(target_os = "windows")]
    {
        for image in windows_process_image_candidates(app_name) {
            let _ = tokio::process::Command::new("taskkill")
                .args(["/F", "/T", "/IM", &image])
                .output()
                .await;
        }
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    }
}

/// Return likely Windows process image names for a given app label.
///
/// We include known Chrome-family mappings first, then a conservative fallback
/// using the label itself (with `.exe` suffix when needed).
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
fn windows_process_image_candidates(app_name: &str) -> Vec<String> {
    let lower = app_name.trim().to_ascii_lowercase();
    let mut out: Vec<String> = Vec::new();

    if lower.contains("chrome") || lower.contains("chromium") {
        out.push("chrome.exe".to_string());
    } else if lower.contains("edge") {
        out.push("msedge.exe".to_string());
    } else if lower.contains("brave") {
        out.push("brave.exe".to_string());
    } else if lower.contains("arc") {
        out.push("arc.exe".to_string());
    }

    let fallback = if lower.ends_with(".exe") {
        app_name.trim().to_string()
    } else {
        format!("{}.exe", app_name.trim())
    };
    if !fallback.is_empty()
        && !out
            .iter()
            .any(|existing| existing.eq_ignore_ascii_case(&fallback))
    {
        out.push(fallback);
    }

    out
}

#[cfg(test)]
mod tests {
    use super::windows_process_image_candidates;

    #[test]
    fn windows_image_candidates_map_known_browsers() {
        assert_eq!(
            windows_process_image_candidates("Google Chrome"),
            vec!["chrome.exe".to_string(), "Google Chrome.exe".to_string()]
        );
        assert_eq!(
            windows_process_image_candidates("Microsoft Edge"),
            vec!["msedge.exe".to_string(), "Microsoft Edge.exe".to_string()]
        );
    }

    #[test]
    fn windows_image_candidates_include_fallback() {
        assert_eq!(
            windows_process_image_candidates("Code.exe"),
            vec!["Code.exe".to_string()]
        );
        assert_eq!(
            windows_process_image_candidates("Some App"),
            vec!["Some App.exe".to_string()]
        );
    }
}
