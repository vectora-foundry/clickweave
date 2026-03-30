use super::Mcp;
use super::{ExecutorError, ExecutorResult, ResolvedApp, WorkflowExecutor};
use clickweave_core::decision_cache::{self, AppResolution};
use clickweave_core::{ExecutionMode, FocusMethod, NodeRun, NodeType};
use clickweave_llm::{ChatBackend, Message};
use serde_json::Value;
use tracing::debug;
use uuid::Uuid;

impl<C: ChatBackend> WorkflowExecutor<C> {
    /// Resolve a user-provided app name (e.g. "chrome", "my editor") to a concrete
    /// running application by asking the orchestrator LLM to match against the
    /// live list of apps and windows.  Results are cached so repeated references
    /// to the same user string only incur one LLM call.
    pub(crate) async fn resolve_app_name(
        &self,
        node_id: Uuid,
        user_input: &str,
        mcp: &(impl Mcp + ?Sized),
        node_run: Option<&NodeRun>,
        force_resolve: bool,
    ) -> ExecutorResult<ResolvedApp> {
        // Check in-memory cache first (populated during this execution).
        // Clone the cached value out before any .await to avoid holding the
        // RwLockReadGuard across an await point (which breaks Send).
        let cached_app = self.read_app_cache().get(user_input).cloned();
        if let Some(cached) = cached_app {
            debug!(user_input, resolved_name = %cached.name, "app_cache hit");
            // Verify the cached PID is still valid — the app may have been quit and relaunched
            if let Ok(fresh_pid) = self.lookup_app_pid(&cached.name, mcp).await {
                if fresh_pid == cached.pid {
                    self.log(format!(
                        "App resolved (cached): \"{}\" -> {} (pid {})",
                        user_input, cached.name, cached.pid
                    ));
                    return Ok(cached);
                }
                // PID changed — app was restarted; update cache and return fresh entry
                debug!(
                    user_input,
                    old_pid = cached.pid,
                    new_pid = fresh_pid,
                    "app_cache PID stale, updating"
                );
                let updated = ResolvedApp {
                    name: cached.name.clone(),
                    pid: fresh_pid,
                };
                self.write_app_cache()
                    .insert(user_input.to_string(), updated.clone());
                self.log(format!(
                    "App resolved (cached, refreshed PID): \"{}\" -> {} (pid {})",
                    user_input, updated.name, updated.pid
                ));
                return Ok(updated);
            }
            // App is no longer running — evict stale entry and fall through to full resolution
            debug!(
                user_input,
                "app_cache hit but app no longer running, evicting"
            );
            self.write_app_cache().remove(user_input);
        }

        // Check persistent decision cache (replays Test-mode app name decisions).
        // Skip when force_resolve is set so a retry after eviction re-resolves via LLM.
        // Clone the cached value out before any .await to avoid holding the
        // RwLockReadGuard across an await point (which breaks Send).
        let ck = decision_cache::cache_key(node_id, user_input, None);
        let cached_app = if force_resolve {
            None
        } else {
            self.read_decision_cache().app_resolution.get(&ck).cloned()
        };
        if let Some(cached) = cached_app {
            debug!(user_input, resolved_name = %cached.resolved_name, "decision_cache app hit");
            // We have the app name but need a fresh PID — look it up
            match self.lookup_app_pid(&cached.resolved_name, mcp).await {
                Ok(pid) => {
                    let resolved = ResolvedApp {
                        name: cached.resolved_name.clone(),
                        pid,
                    };
                    self.log(format!(
                        "App resolved (decision cache): \"{}\" -> {} (pid {})",
                        user_input, resolved.name, resolved.pid
                    ));
                    self.write_app_cache()
                        .insert(user_input.to_string(), resolved.clone());
                    return Ok(resolved);
                }
                Err(e) => {
                    debug!(
                        user_input,
                        cached_name = %cached.resolved_name,
                        error = %e,
                        "decision_cache app hit but PID lookup failed, falling through to LLM"
                    );
                }
            }
        }

        let apps_result = mcp
            .call_tool(
                "list_apps",
                Some(serde_json::json!({"user_apps_only": true})),
            )
            .await
            .map_err(|e| ExecutorError::AppResolution(format!("Failed to list apps: {}", e)))?;
        let windows_result = mcp
            .call_tool("list_windows", None)
            .await
            .map_err(|e| ExecutorError::AppResolution(format!("Failed to list windows: {}", e)))?;

        let apps_text = Self::extract_result_text(&apps_result);
        let windows_text = Self::extract_result_text(&windows_result);

        // Short-circuit: if no apps are running, don't ask the LLM — it will hallucinate.
        let apps_trimmed = apps_text.trim();
        if apps_trimmed.is_empty() || apps_trimmed == "[]" || apps_trimmed == "No apps found" {
            return Err(ExecutorError::AppResolution(format!(
                "App \"{}\" is not running (no matching apps found). \
                 Use launch_app to start it first.",
                user_input
            )));
        }

        let prompt = format!(
            "You are resolving an application name. The user wrote: \"{user_input}\"\n\
             \n\
             Running apps:\n\
             {apps_text}\n\
             \n\
             Visible windows:\n\
             {windows_text}\n\
             \n\
             Which running application does the user mean? Return ONLY a JSON object:\n\
             {{\"name\": \"<exact app name from the list above>\", \"pid\": <pid>}}\n\
             \n\
             IMPORTANT: The name MUST be an exact match from the Running apps list above.\n\
             Do NOT guess or invent app names. Do NOT return an unrelated app.\n\
             If no running app is a plausible match, return:\n\
             {{\"name\": null, \"pid\": null}}"
        );

        let messages = vec![Message::user(prompt)];
        let response = self
            .reasoning_backend()
            .chat(messages, None)
            .await
            .map_err(|e| ExecutorError::AppResolution(format!("LLM error: {}", e)))?;

        let choice = response.choices.first().ok_or_else(|| {
            ExecutorError::AppResolution("No response from LLM during app resolution".to_string())
        })?;

        let raw_text = choice.message.content_text().ok_or_else(|| {
            ExecutorError::AppResolution(
                "LLM returned empty content during app resolution".to_string(),
            )
        })?;

        let json_text = parse_llm_json_response(raw_text).ok_or_else(|| {
            ExecutorError::AppResolution(format!(
                "No JSON object found in LLM response (raw: {})",
                raw_text
            ))
        })?;

        let parsed: Value = serde_json::from_str(json_text).map_err(|e| {
            ExecutorError::AppResolution(format!(
                "Failed to parse LLM response as JSON: {} (raw: {})",
                e, raw_text
            ))
        })?;

        let name = parsed["name"].as_str().ok_or_else(|| {
            ExecutorError::AppResolution(format!(
                "App \"{}\" is not running (LLM found no match). \
                 Use launch_app to start it first.",
                user_input
            ))
        })?;

        // Post-validate: ensure the LLM returned a name that actually appears in the app list.
        // Parse the JSON array so we match against individual app name entries rather than
        // doing a raw substring match on the full JSON text (which would accept "Code" as a
        // match for "Visual Studio Code").
        let app_names: Vec<String> = serde_json::from_str::<Vec<Value>>(&apps_text)
            .unwrap_or_default()
            .into_iter()
            .filter_map(|v| v["name"].as_str().map(|s| s.to_string()))
            .collect();
        let name_lower = name.to_lowercase();
        let validated = app_names.iter().any(|n| n.to_lowercase() == name_lower);
        if !validated {
            return Err(ExecutorError::AppResolution(format!(
                "App \"{}\" is not running (resolved name \"{}\" not found in app list). \
                 Use launch_app to start it first.",
                user_input, name
            )));
        }

        let pid = parsed["pid"].as_i64().ok_or_else(|| {
            ExecutorError::AppResolution(format!(
                "LLM resolved name \"{}\" for \"{}\" but returned no PID",
                name, user_input
            ))
        })? as i32;

        let resolved = ResolvedApp {
            name: name.to_string(),
            pid,
        };

        self.record_event(
            node_run,
            "app_resolved",
            serde_json::json!({
                "user_input": user_input,
                "resolved_name": resolved.name,
                "resolved_pid": resolved.pid,
            }),
        );

        self.log(format!(
            "App resolved: \"{}\" -> {} (pid {})",
            user_input, resolved.name, resolved.pid
        ));

        self.write_app_cache()
            .insert(user_input.to_string(), resolved.clone());

        // Record in decision cache for replay in Run mode (name only, not PID)
        if self.execution_mode == ExecutionMode::Test {
            self.write_decision_cache().app_resolution.insert(
                ck,
                AppResolution {
                    user_input: user_input.to_string(),
                    resolved_name: resolved.name.clone(),
                },
            );
        }

        Ok(resolved)
    }

    /// Look up a PID for an app by its exact name via `list_apps`.
    pub(super) async fn lookup_app_pid(
        &self,
        app_name: &str,
        mcp: &(impl Mcp + ?Sized),
    ) -> ExecutorResult<i32> {
        let result = mcp
            .call_tool(
                "list_apps",
                Some(serde_json::json!({"app_name": app_name, "user_apps_only": true})),
            )
            .await
            .map_err(|e| ExecutorError::AppResolution(format!("Failed to list apps: {}", e)))?;
        let text = Self::extract_result_text(&result);
        let apps: Vec<Value> = serde_json::from_str(&text).unwrap_or_default();
        for app in &apps {
            if app["name"].as_str() == Some(app_name)
                && let Some(pid) = app["pid"].as_i64()
            {
                return Ok(pid as i32);
            }
        }
        Err(ExecutorError::AppResolution(format!(
            "App \"{}\" is not running (not found in app list)",
            app_name
        )))
    }

    /// Remove a cached app resolution so the next attempt re-resolves via LLM.
    pub(crate) fn evict_app_cache(&self, user_input: &str) {
        if self.write_app_cache().remove(user_input).is_some() {
            debug!(user_input, "evicted app_cache entry");
            self.log(format!("App cache evicted for \"{}\"", user_input));
        }
    }

    /// Evict any app-name and element-name cache entries associated with a
    /// node type, so that retries re-resolve via LLM.
    pub(crate) fn evict_caches_for_node(&self, node_type: &NodeType) {
        let key = match node_type {
            NodeType::FocusWindow(p) if p.method == FocusMethod::AppName => p.value.as_deref(),
            NodeType::TakeScreenshot(p) => p.target.as_deref(),
            _ => None,
        };
        if let Some(key) = key {
            self.evict_app_cache(key);
        }

        // Evict element cache before clearing focused_app, so the cache key
        // still contains the correct app name.
        let (element_target, explicit_app) = match node_type {
            NodeType::Click(p) => (p.target.as_ref().map(|t| t.text()), None),
            NodeType::FindText(p) => (Some(p.search_text.as_str()), p.scope.as_deref()),
            NodeType::McpToolCall(p) if p.tool_name == "find_text" => (
                p.arguments.get("text").and_then(|v| v.as_str()),
                p.arguments.get("app_name").and_then(|v| v.as_str()),
            ),
            _ => (None, None),
        };
        if let Some(target) = element_target {
            // Prefer explicit app_name from call args; fall back to focused_app.
            let app_name = explicit_app
                .map(|s| s.to_string())
                .or_else(|| self.focused_app_name());
            self.evict_element_cache(target, app_name.as_deref());
        }

        if matches!(node_type, NodeType::FocusWindow(_)) {
            *self.write_focused_app() = None;
        }
    }
}

/// Extract the first top-level `{…}` JSON object from `text`, ignoring any
/// leading or trailing prose the LLM may have added around it.
pub(crate) fn extract_json_object(text: &str) -> Option<&str> {
    let start = text.find('{')?;
    let mut depth = 0i32;
    let mut in_string = false;
    let mut escape_next = false;
    for (i, ch) in text[start..].char_indices() {
        if escape_next {
            escape_next = false;
            continue;
        }
        if in_string {
            match ch {
                '\\' => escape_next = true,
                '"' => in_string = false,
                _ => {}
            }
            continue;
        }
        match ch {
            '"' => in_string = true,
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&text[start..start + i + 1]);
                }
            }
            _ => {}
        }
    }
    None
}

/// Strip markdown code fences and extract the first JSON object from an LLM response.
pub(crate) fn parse_llm_json_response(raw: &str) -> Option<&str> {
    extract_json_object(strip_code_block(raw))
}

/// Strip optional markdown code fences (```` ```json ... ``` ```` or ```` ``` ... ``` ````)
/// so we can parse the inner JSON.
pub(crate) fn strip_code_block(text: &str) -> &str {
    let trimmed = text.trim();
    let Some(rest) = trimmed.strip_prefix("```") else {
        return trimmed;
    };
    // Skip any language tag on the opening fence line
    let rest = match rest.find('\n') {
        Some(pos) => &rest[pos + 1..],
        None => rest,
    };
    rest.strip_suffix("```").unwrap_or(rest).trim()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_code_block_bare_json() {
        let input = r#"{"name": "Foo", "pid": 1}"#;
        assert_eq!(strip_code_block(input), input);
    }

    #[test]
    fn strip_code_block_with_json_fence() {
        let input = "```json\n{\"name\": \"Foo\", \"pid\": 1}\n```";
        assert_eq!(strip_code_block(input), r#"{"name": "Foo", "pid": 1}"#);
    }

    #[test]
    fn strip_code_block_with_plain_fence() {
        let input = "```\n{\"name\": \"Bar\", \"pid\": 42}\n```";
        assert_eq!(strip_code_block(input), r#"{"name": "Bar", "pid": 42}"#);
    }

    #[test]
    fn strip_code_block_with_extra_whitespace() {
        let input = "  \n```json\n  {\"name\": \"Baz\", \"pid\": 7}  \n```\n  ";
        assert_eq!(strip_code_block(input), r#"{"name": "Baz", "pid": 7}"#);
    }

    #[test]
    fn strip_code_block_uppercase_json_tag() {
        let input = "```JSON\n{\"name\": \"Qux\", \"pid\": 99}\n```";
        assert_eq!(strip_code_block(input), r#"{"name": "Qux", "pid": 99}"#);
    }

    #[test]
    fn strip_code_block_missing_closing_fence() {
        let input = "```json\n{\"name\": \"Open\", \"pid\": 5}";
        assert_eq!(strip_code_block(input), r#"{"name": "Open", "pid": 5}"#);
    }

    #[test]
    fn strip_code_block_multiline_json() {
        let input = "```json\n{\n  \"name\": \"Multi\",\n  \"pid\": 3\n}\n```";
        let expected = "{\n  \"name\": \"Multi\",\n  \"pid\": 3\n}";
        assert_eq!(strip_code_block(input), expected);
    }

    #[test]
    fn strip_code_block_arbitrary_language_tag() {
        let input = "```text\n{\"name\": \"Any\", \"pid\": 10}\n```";
        assert_eq!(strip_code_block(input), r#"{"name": "Any", "pid": 10}"#);
    }

    #[test]
    fn strip_code_block_only_whitespace_around_bare_json() {
        let input = "   {\"name\": \"Trim\", \"pid\": 0}   ";
        assert_eq!(strip_code_block(input), r#"{"name": "Trim", "pid": 0}"#);
    }

    #[test]
    fn extract_json_object_clean() {
        let input = r#"{"name": "Calculator", "pid": 70392}"#;
        assert_eq!(extract_json_object(input), Some(input));
    }

    #[test]
    fn extract_json_object_with_trailing_prose() {
        let input = r#"{"name": "Calculator", "pid": 70392}

The user's query specifically mentions the application name as "Calculator"."#;
        assert_eq!(
            extract_json_object(input),
            Some(r#"{"name": "Calculator", "pid": 70392}"#)
        );
    }

    #[test]
    fn extract_json_object_with_leading_prose() {
        let input = r#"Here is the result:
{"name": "Safari", "pid": 1234}"#;
        assert_eq!(
            extract_json_object(input),
            Some(r#"{"name": "Safari", "pid": 1234}"#)
        );
    }

    #[test]
    fn extract_json_object_with_nested_braces_in_string() {
        let input = r#"{"name": "App {v2}", "pid": 42} trailing"#;
        assert_eq!(
            extract_json_object(input),
            Some(r#"{"name": "App {v2}", "pid": 42}"#)
        );
    }

    #[test]
    fn extract_json_object_with_escaped_quotes() {
        let input = r#"{"name": "say \"hello\"", "pid": 7} extra"#;
        assert_eq!(
            extract_json_object(input),
            Some(r#"{"name": "say \"hello\"", "pid": 7}"#)
        );
    }

    #[test]
    fn extract_json_object_no_object() {
        assert_eq!(extract_json_object("no json here"), None);
    }

    #[test]
    fn extract_json_object_unclosed() {
        assert_eq!(extract_json_object("{\"name\": \"bad\""), None);
    }
}
