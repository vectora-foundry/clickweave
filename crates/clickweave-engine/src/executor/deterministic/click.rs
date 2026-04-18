use super::super::Mcp;
use super::super::app_resolve::CacheMode;
use super::super::retry_context::RetryContext;
use super::super::{ExecutorError, ExecutorResult, WorkflowExecutor};
use super::truncate_for_error;
use clickweave_core::decision_cache::cache_key;
use clickweave_core::{ClickParams, ClickTarget, NodeRun, NodeType};
use clickweave_llm::ChatBackend;
use serde_json::Value;
use uuid::Uuid;

impl<C: ChatBackend> WorkflowExecutor<C> {
    pub(in crate::executor) async fn resolve_click_target(
        &mut self,
        node_id: Uuid,
        mcp: &(impl Mcp + ?Sized),
        params: &ClickParams,
        node_run: &mut Option<&mut NodeRun>,
        retry_ctx: &mut RetryContext,
    ) -> ExecutorResult<NodeType> {
        let target = params.target.as_ref().map(|t| t.text()).ok_or_else(|| {
            ExecutorError::ClickTarget("resolve_click_target called with no target".to_string())
        })?;
        let (x, y) = self
            .resolve_target_by_text(node_id, target, mcp, node_run, retry_ctx)
            .await?;
        Ok(NodeType::Click(ClickParams {
            target: Some(ClickTarget::Coordinates { x, y }),
            button: params.button,
            click_count: params.click_count,
            ..Default::default()
        }))
    }

    /// Resolve a text target to screen coordinates via find_text + disambiguation.
    ///
    /// Shared by click and hover target resolution. Returns `(x, y)` coordinates.
    pub(in crate::executor) async fn resolve_target_by_text(
        &mut self,
        node_id: Uuid,
        target: &str,
        mcp: &(impl Mcp + ?Sized),
        node_run: &mut Option<&mut NodeRun>,
        retry_ctx: &mut RetryContext,
    ) -> ExecutorResult<(f64, f64)> {
        let scoped_app = self.focused_app_name();

        // Use cached element resolution if available (e.g. x -> Multiply) to
        // avoid matching display text that happens to contain the symbol.
        let element_key = (target.to_string(), scoped_app.clone());
        let search_text = self
            .read_element_cache()
            .get(&element_key)
            .cloned()
            .unwrap_or_else(|| target.to_string());

        let mut find_args = serde_json::json!({"text": search_text});
        if let Some(ref app_name) = scoped_app {
            find_args["app_name"] = serde_json::Value::String(app_name.clone());
        }

        match &scoped_app {
            Some(app) => self.log(format!("Resolving target: '{}' in '{}'", target, app)),
            None => self.log(format!("Resolving target: '{}' (screen-wide)", target)),
        }

        let find_result = mcp
            .call_tool("find_text", Some(find_args.clone()))
            .await
            .map_err(|e| {
                ExecutorError::ClickTarget(format!("find_text for '{}' failed: {}", target, e))
            })?;

        Self::check_tool_error(&find_result, "find_text")?;

        let result_text = crate::cdp_lifecycle::extract_text(&find_result);
        let mut matches: Vec<Value> = serde_json::from_str(&result_text).unwrap_or_default();

        // Fallback: if no matches but available_elements present, ask LLM to resolve
        if matches.is_empty()
            && let Some(retry_text) = self
                .try_resolve_find_text(
                    node_id,
                    &find_args,
                    &result_text,
                    mcp,
                    node_run.as_deref(),
                    retry_ctx.cache_mode,
                )
                .await
        {
            matches = serde_json::from_str(&retry_text).unwrap_or_default();
        }

        let best = if matches.is_empty() {
            return Err(ExecutorError::ClickTarget(format!(
                "Could not find text '{}' on screen (find_text returned: {})",
                target,
                truncate_for_error(&result_text, 120),
            )));
        } else if matches.len() == 1 {
            &matches[0]
        } else {
            // Check decision cache first.
            // When multiple live matches share the same text and role, use the
            // cached coordinates as a tiebreaker (closest Euclidean distance wins).
            let ck = cache_key(node_id, target, scoped_app.as_deref());
            let cached_idx = self
                .read_decision_cache()
                .click_disambiguation
                .get(&ck)
                .cloned()
                .and_then(|cached| {
                    let text_role_matches: Vec<(usize, &Value)> = matches
                        .iter()
                        .enumerate()
                        .filter(|(_, m)| {
                            m["text"].as_str() == Some(cached.chosen_text.as_str())
                                && m["role"].as_str() == Some(cached.chosen_role.as_str())
                        })
                        .collect();

                    match text_role_matches.len() {
                        0 => None,
                        1 => Some(text_role_matches[0].0),
                        _ => {
                            // Multiple matches share text+role — use coordinates as tiebreaker.
                            if let (Some(cx), Some(cy)) = (cached.chosen_x, cached.chosen_y) {
                                text_role_matches
                                    .into_iter()
                                    .min_by_key(|(_, m)| {
                                        let dx = m["x"].as_f64().unwrap_or(0.0) - cx;
                                        let dy = m["y"].as_f64().unwrap_or(0.0) - cy;
                                        // Use integer distance² for ordering (no need for sqrt)
                                        ((dx * dx + dy * dy) * 1000.0) as i64
                                    })
                                    .map(|(idx, _)| idx)
                            } else {
                                // No cached coordinates — fall back to first match
                                Some(text_role_matches[0].0)
                            }
                        }
                    }
                });

            let idx = if let Some(idx) = cached_idx {
                self.log(format!("Using cached disambiguation for '{}'", target));
                idx
            } else {
                self.disambiguate_click_matches(
                    node_id,
                    target,
                    &matches,
                    scoped_app.as_deref(),
                    node_run.as_deref(),
                    retry_ctx,
                )
                .await?
            };
            &matches[idx]
        };

        let x = best["x"].as_f64().ok_or_else(|| {
            ExecutorError::ClickTarget(format!(
                "find_text match for '{}' missing 'x' coordinate",
                target
            ))
        })?;
        let y = best["y"].as_f64().ok_or_else(|| {
            ExecutorError::ClickTarget(format!(
                "find_text match for '{}' missing 'y' coordinate",
                target
            ))
        })?;

        self.log(format!("Resolved target '{}' -> ({}, {})", target, x, y));

        self.record_event(
            node_run.as_deref(),
            "target_resolved",
            serde_json::json!({
                "target": target,
                "x": x,
                "y": y,
                "app_name": scoped_app,
            }),
        );

        Ok((x, y))
    }

    /// Try to resolve a failed find_text query by asking the LLM to match
    /// against available accessibility element names, then retry.
    /// Returns the retry result text on success, or None if resolution wasn't
    /// possible or the retry also failed.
    ///
    /// Preserves the original call arguments (e.g. `app_name`, `match_mode`)
    /// and only replaces the `text` field with the resolved name.
    pub(in crate::executor) async fn try_resolve_find_text(
        &mut self,
        node_id: Uuid,
        original_args: &Value,
        original_result_text: &str,
        mcp: &(impl Mcp + ?Sized),
        node_run: Option<&NodeRun>,
        cache_mode: CacheMode,
    ) -> Option<String> {
        let retry_args = self
            .prepare_find_text_retry(
                node_id,
                original_args,
                original_result_text,
                node_run,
                cache_mode,
            )
            .await?;
        let retry_result = mcp.call_tool("find_text", Some(retry_args)).await.ok()?;
        if retry_result.is_error == Some(true) {
            return None;
        }
        Some(crate::cdp_lifecycle::extract_text(&retry_result))
    }

    /// Parse available_elements from a failed find_text response, resolve the
    /// element name via LLM, and build retry arguments.
    ///
    /// Returns `Some(retry_args)` with the resolved name swapped in, or `None`
    /// if resolution wasn't possible. This is the pure-logic core of the
    /// find_text fallback path, separated from the MCP I/O for testability.
    pub(crate) async fn prepare_find_text_retry(
        &mut self,
        node_id: Uuid,
        original_args: &Value,
        original_result_text: &str,
        node_run: Option<&NodeRun>,
        cache_mode: CacheMode,
    ) -> Option<Value> {
        let target = original_args.get("text")?.as_str()?;
        let available =
            super::super::element_resolve::parse_available_elements(original_result_text)?;
        // Prefer explicit app_name from call args; fall back to focused_app.
        let scoped_app = original_args
            .get("app_name")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .or_else(|| self.focused_app_name());
        let resolved_name = self
            .resolve_element_name(
                node_id,
                target,
                &available,
                scoped_app.as_deref(),
                node_run,
                cache_mode,
            )
            .await
            .ok()?;

        self.log(format!(
            "Retrying find_text with resolved name '{}' for '{}'",
            resolved_name, target
        ));

        let mut retry_args = original_args.clone();
        retry_args["text"] = Value::String(resolved_name);
        Some(retry_args)
    }
}
