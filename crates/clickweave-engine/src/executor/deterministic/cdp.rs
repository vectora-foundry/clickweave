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
            self.log(format!(
                "CDP: no exact match for '{}', resolving via element inventory",
                target
            ));
            let extra_inputs = self.query_contenteditable_elements(mcp).await;
            let mut resolved = self
                .resolve_via_inventory(target, &snapshot_text, &extra_inputs)
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

    /// Resolve a CDP element and perform an action (click or hover) on it.
    /// Returns the action result text.
    pub(in crate::executor) async fn execute_cdp_action(
        &self,
        action: &str,
        target: &str,
        expected: &CdpExpected<'_>,
        mcp: &(impl Mcp + ?Sized),
        node_run: Option<&NodeRun>,
        retry_ctx: &RetryContext,
    ) -> ExecutorResult<String> {
        let uid = self
            .resolve_cdp_element_uid(target, expected, mcp, retry_ctx)
            .await?;

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
        target: &str,
        expected: &CdpExpected<'_>,
        mcp: &(impl Mcp + ?Sized),
        node_run: Option<&NodeRun>,
        retry_ctx: &RetryContext,
    ) -> ExecutorResult<String> {
        self.execute_cdp_action("click", target, expected, mcp, node_run, retry_ctx)
            .await
    }

    /// Resolve a CDP element and hover it. Returns the hover result text.
    pub(in crate::executor) async fn resolve_and_hover_cdp(
        &self,
        target: &str,
        expected: &CdpExpected<'_>,
        mcp: &(impl Mcp + ?Sized),
        node_run: Option<&NodeRun>,
        retry_ctx: &RetryContext,
    ) -> ExecutorResult<String> {
        self.execute_cdp_action("hover", target, expected, mcp, node_run, retry_ctx)
            .await
    }

    /// Query the DOM for contenteditable elements that the accessibility tree
    /// might represent as `generic` instead of `textbox`. Returns a list of
    /// labels suitable for appending to the inventory prompt.
    async fn query_contenteditable_elements(&self, mcp: &(impl Mcp + ?Sized)) -> Vec<String> {
        // Walk all DOM elements and find editable ones (contenteditable,
        // textarea, text inputs). CSS selectors miss inherited contenteditable
        // (e.g. Quill editors), so we check isContentEditable on each element
        // and skip children of editable parents to avoid duplicates.
        let js = r#"() => {
            const results = [];
            const all = document.querySelectorAll('*');
            for (const el of all) {
                const isInput = el.tagName === 'TEXTAREA' ||
                    (el.tagName === 'INPUT' && (!el.type || el.type === 'text' || el.type === 'search' || el.type === 'url' || el.type === 'email'));
                const isEditable = el.isContentEditable && el.parentElement && !el.parentElement.isContentEditable;
                if (!isInput && !isEditable) continue;
                const label = el.getAttribute('placeholder') || el.getAttribute('aria-label') || el.getAttribute('data-placeholder') || '';
                if (!label) continue;
                results.push({ label, role: isEditable ? 'contenteditable' : el.tagName.toLowerCase() });
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
                    let labels: Vec<String> = entries
                        .iter()
                        .filter_map(|e| {
                            let label = e.get("label")?.as_str()?;
                            let role = e.get("role")?.as_str().unwrap_or("input");
                            if label.is_empty() {
                                return None;
                            }
                            Some(format!("{} ({})", label, role))
                        })
                        .collect();
                    if !labels.is_empty() {
                        self.log(format!(
                            "CDP: found {} contenteditable elements via JS: {}",
                            labels.len(),
                            labels.join(", ")
                        ));
                    }
                    labels
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
            return None;
        }
        let pids = String::from_utf8_lossy(&output.stdout);
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
            if let Some(flag) = args
                .split_whitespace()
                .find(|a| a.starts_with("--remote-debugging-port="))
                && let Some(port_str) = flag.strip_prefix("--remote-debugging-port=")
                && let Ok(port) = port_str.parse::<u16>()
            {
                return Some(port);
            }
        }
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
