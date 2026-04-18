use std::path::Path;

use super::super::retry_context::RetryContext;
use super::super::{CdpCandidate, ExecutorError, ExecutorResult, Mcp, WorkflowExecutor};
use crate::cdp_lifecycle::{
    self, APP_QUIT_MAX_ATTEMPTS, APP_QUIT_POLL_INTERVAL, CDP_CONNECT_MAX_ATTEMPTS,
    CDP_READY_TIMEOUT_AFTER_RELAUNCH_SECS, CDP_READY_TIMEOUT_REUSE_SECS, QuitOutcome,
};
use clickweave_core::NodeRun;
use clickweave_llm::ChatBackend;
use uuid::Uuid;

impl<C: ChatBackend> WorkflowExecutor<C> {
    /// Simple element resolution: take a snapshot, find the first matching
    /// element by text, and return its UID. No LLM disambiguation or
    /// multi-tier fallbacks — the agent architecture handles that.
    #[cfg(test)]
    pub(in crate::executor) async fn resolve_cdp_element_uid(
        &self,
        target: &str,
        mcp: &(impl Mcp + ?Sized),
    ) -> ExecutorResult<String> {
        self.resolve_cdp_element_uid_with_overrides(target, mcp, None)
            .await
    }

    /// Variant that lets the caller pass a target→uid override map. When an
    /// override is present for `target`, the snapshot/MCP round-trip is
    /// skipped and the pre-chosen uid is returned directly. Used by the
    /// agent-disambiguation retry path.
    pub(in crate::executor) async fn resolve_cdp_element_uid_with_overrides(
        &self,
        target: &str,
        mcp: &(impl Mcp + ?Sized),
        overrides: Option<&RetryContext>,
    ) -> ExecutorResult<String> {
        if target.trim().is_empty() {
            return Err(ExecutorError::Cdp(
                "CDP target is empty; expected a non-empty label or text".to_string(),
            ));
        }

        if let Some(ctx) = overrides
            && let Some(uid) = ctx.cdp_ambiguity_overrides.get(target).cloned()
        {
            self.log(format!(
                "CDP: using agent-picked uid='{}' for '{}' (ambiguity override)",
                uid, target
            ));
            return Ok(uid);
        }

        // Refresh page list to verify CDP connection is healthy.
        let _ = mcp
            .call_tool("cdp_list_pages", Some(serde_json::json!({})))
            .await;

        // Take snapshot with retry (DOM may still be settling).
        let max_attempts = 3;
        for attempt in 0..max_attempts {
            if attempt > 0 {
                self.log(format!(
                    "CDP: snapshot retry {}/{} for '{}' (waiting for DOM to settle)",
                    attempt,
                    max_attempts - 1,
                    target
                ));
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            }

            self.log(format!("CDP: taking snapshot to find '{}'", target));
            let snapshot_result = mcp
                .call_tool("cdp_take_snapshot", Some(serde_json::json!({})))
                .await
                .map_err(|e| {
                    ExecutorError::CdpSnapshotFailed(format!("take_snapshot failed: {e}"))
                })?;

            if snapshot_result.is_error == Some(true) {
                let error_text = cdp_lifecycle::extract_text(&snapshot_result);
                return Err(ExecutorError::CdpSnapshotFailed(format!(
                    "take_snapshot error: {}",
                    error_text
                )));
            }

            let snapshot_text = cdp_lifecycle::extract_text(&snapshot_result);
            let mut candidates = collect_snapshot_candidates(&snapshot_text, target);

            if candidates.is_empty() {
                continue;
            }
            if candidates.len() == 1 {
                let uid = candidates.swap_remove(0).uid;
                self.log(format!("CDP: resolved '{}' -> uid='{}'", target, uid));
                return Ok(uid);
            }
            self.log(format!(
                "CDP: ambiguous target '{}' matched {} elements",
                target,
                candidates.len()
            ));
            return Err(ExecutorError::CdpAmbiguousTarget {
                target: target.to_string(),
                candidates,
            });
        }

        Err(ExecutorError::CdpNotFound {
            target: target.to_string(),
        })
    }

    /// Resolve a CDP element and perform an action (click or hover) on it.
    /// Returns the action result text.
    pub(in crate::executor) async fn execute_cdp_action(
        &mut self,
        action: &str,
        _node_id: Uuid,
        target: &str,
        mcp: &(impl Mcp + ?Sized),
        node_run: Option<&NodeRun>,
        retry_ctx: &RetryContext,
    ) -> ExecutorResult<String> {
        let uid = self
            .resolve_cdp_element_uid_with_overrides(target, mcp, Some(retry_ctx))
            .await?;

        self.log(format!("CDP: {} element uid='{}'", action, uid));
        let tool_name = format!("cdp_{action}");
        let result = mcp
            .call_tool(&tool_name, Some(serde_json::json!({ "uid": uid })))
            .await
            .map_err(|e| ExecutorError::ToolCall {
                tool: tool_name.clone(),
                message: e.to_string(),
            })?;

        if result.is_error == Some(true) {
            return Err(ExecutorError::ToolCall {
                tool: tool_name,
                message: cdp_lifecycle::extract_text(&result),
            });
        }

        // `action` is always "click" or "hover" from this private helper —
        // map it to the typed variant and fall back to Unknown for safety.
        let event_kind = match action {
            "click" => clickweave_core::TraceEventKind::CdpClick,
            "hover" => clickweave_core::TraceEventKind::CdpHover,
            "fill" => clickweave_core::TraceEventKind::CdpFill,
            _ => clickweave_core::TraceEventKind::Unknown,
        };
        self.record_event(
            node_run,
            event_kind,
            serde_json::json!({ "target": target, "uid": uid }),
        );

        Ok(cdp_lifecycle::extract_text(&result))
    }

    /// Resolve a CDP element and click it. Returns the click result text.
    pub(in crate::executor) async fn resolve_and_click_cdp(
        &mut self,
        node_id: Uuid,
        target: &str,
        mcp: &(impl Mcp + ?Sized),
        node_run: Option<&NodeRun>,
        retry_ctx: &RetryContext,
    ) -> ExecutorResult<String> {
        self.execute_cdp_action("click", node_id, target, mcp, node_run, retry_ctx)
            .await
    }

    /// Resolve a CdpTarget to a concrete UID for use with `cdp_fill` or similar
    /// UID-only tools. `ResolvedUid` and obvious UID-shaped labels pass through
    /// untouched; `Intent` and free-form labels go through snapshot resolution
    /// so the UID is refreshed against the live DOM.
    #[cfg(test)]
    pub(in crate::executor) async fn resolve_cdp_target_uid(
        &self,
        target: &clickweave_core::CdpTarget,
        mcp: &(impl Mcp + ?Sized),
    ) -> ExecutorResult<String> {
        self.resolve_cdp_target_uid_with_overrides(target, mcp, None)
            .await
    }

    /// Override-aware variant of `resolve_cdp_target_uid`.
    pub(in crate::executor) async fn resolve_cdp_target_uid_with_overrides(
        &self,
        target: &clickweave_core::CdpTarget,
        mcp: &(impl Mcp + ?Sized),
        overrides: Option<&RetryContext>,
    ) -> ExecutorResult<String> {
        use clickweave_core::CdpTarget;
        match target {
            CdpTarget::ResolvedUid(uid) => Ok(uid.clone()),
            CdpTarget::ExactLabel(s) if looks_like_cdp_uid(s) => Ok(s.clone()),
            CdpTarget::ExactLabel(label) => {
                self.resolve_cdp_element_uid_with_overrides(label, mcp, overrides)
                    .await
            }
            CdpTarget::Intent(intent) => {
                self.resolve_cdp_element_uid_with_overrides(intent, mcp, overrides)
                    .await
            }
        }
    }

    /// Resolve a CDP element and hover it. Returns the hover result text.
    pub(in crate::executor) async fn resolve_and_hover_cdp(
        &mut self,
        node_id: Uuid,
        target: &str,
        mcp: &(impl Mcp + ?Sized),
        node_run: Option<&NodeRun>,
        retry_ctx: &RetryContext,
    ) -> ExecutorResult<String> {
        self.execute_cdp_action("hover", node_id, target, mcp, node_run, retry_ctx)
            .await
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

        // Already have a CDP connection for this exact app instance --
        // nothing to do. Name + PID must both match; same-name instances
        // are distinct for CDP purposes.
        if self
            .cdp_state
            .connected_app
            .as_ref()
            .is_some_and(|(n, p)| n == app_name && *p == pid)
        {
            return Ok(());
        }

        // Disconnect from any previously connected app.
        if let Some((prev_name, prev_pid)) = self.cdp_state.take_connected() {
            // Capture the currently-selected page URL before tearing down so
            // a future reconnect to this app instance can restore the same tab.
            self.snapshot_selected_page_url(&prev_name, prev_pid, mcp)
                .await;
            super::best_effort::best_effort_tool_call(
                mcp,
                "cdp_disconnect",
                None,
                "ensure_cdp_connected: pre-disconnect before new connect",
            )
            .await;
        }

        let port = if self.execution_mode == ExecutionMode::Test {
            // Try reusing an existing debug port before doing a full relaunch.
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
                    ExecutorError::Validation(format!(
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

        self.cdp_state.set_connected(app_name, pid);

        // Restore the previously-selected tab (or remember whatever
        // `cdp_connect` auto-selected if we had no prior record).
        self.restore_or_record_selected_page(app_name, pid, mcp)
            .await;

        Ok(())
    }

    /// List pages, pick the best match for this app instance's remembered
    /// URL, and call `cdp_select_page`. If no URL is remembered or no match
    /// is found, record whichever page is currently marked as selected so
    /// the next reconnect can restore it.
    ///
    /// Keyed by `(app_name, pid)` — two instances of the same-named app
    /// (default Chrome vs. profile-scoped Chrome) must not overwrite each
    /// other's remembered tab.
    ///
    /// Failure is logged but never propagated — the connection is already
    /// established; a bad select is strictly a "wrong tab" regression, not
    /// a hard failure. Callers fall through to the first-page auto-selection
    /// that `cdp_connect` already performed.
    pub(in crate::executor) async fn restore_or_record_selected_page(
        &mut self,
        app_name: &str,
        pid: i32,
        mcp: &(impl Mcp + ?Sized),
    ) {
        use clickweave_core::cdp::{
            current_selected_page_url, parse_cdp_page_list, pick_page_index_for_url,
        };

        let list_result = match mcp
            .call_tool("cdp_list_pages", Some(serde_json::json!({})))
            .await
        {
            Ok(r) if r.is_error != Some(true) => r,
            Ok(r) => {
                self.log(format!(
                    "CDP page restore: cdp_list_pages returned error for '{}': {}",
                    app_name,
                    cdp_lifecycle::extract_text(&r)
                ));
                return;
            }
            Err(e) => {
                self.log(format!(
                    "CDP page restore: cdp_list_pages call failed for '{}': {}",
                    app_name, e
                ));
                return;
            }
        };

        let list_text = cdp_lifecycle::extract_text(&list_result);
        let pages = parse_cdp_page_list(&list_text);
        if pages.is_empty() {
            return;
        }

        let remembered = self
            .cdp_state
            .remembered_url(app_name, pid)
            .map(str::to_owned);
        if let Some(target_url) = remembered.as_deref()
            && let Some(target_index) = pick_page_index_for_url(&pages, target_url)
        {
            // Already on the right page — skip the no-op call.
            let already_selected = pages
                .iter()
                .find(|p| p.index == target_index)
                .is_some_and(|p| p.selected);
            if already_selected {
                self.cdp_state
                    .record_selected_page(app_name, pid, target_url.to_string());
                return;
            }

            match mcp
                .call_tool(
                    "cdp_select_page",
                    Some(serde_json::json!({ "page_idx": target_index })),
                )
                .await
            {
                Ok(r) if r.is_error != Some(true) => {
                    self.log(format!(
                        "CDP: restored page [{}] {} for '{}'",
                        target_index, target_url, app_name
                    ));
                    self.cdp_state
                        .record_selected_page(app_name, pid, target_url.to_string());
                    return;
                }
                Ok(r) => {
                    self.log(format!(
                        "CDP page restore: cdp_select_page rejected for '{}': {}",
                        app_name,
                        cdp_lifecycle::extract_text(&r)
                    ));
                }
                Err(e) => {
                    self.log(format!(
                        "CDP page restore: cdp_select_page call failed for '{}': {}",
                        app_name, e
                    ));
                }
            }
            // Select failed — fall through to recording whatever is selected.
        } else if remembered.is_some() {
            self.log(format!(
                "CDP page restore: no match for remembered URL on '{}' (tab closed or navigated away); \
                 using auto-selected page",
                app_name
            ));
        }

        // No prior record (first connect) or restore failed — snapshot the
        // currently-selected page so future reconnects have something to aim
        // at.
        if let Some(url) = current_selected_page_url(&pages) {
            self.cdp_state.record_selected_page(app_name, pid, url);
        }
    }

    /// Capture the currently-selected CDP page URL for `(app_name, pid)` so
    /// it can be restored on a future reconnect. Delegates to the shared
    /// [`cdp_lifecycle::snapshot_selected_page_url`] helper.
    pub(in crate::executor) async fn snapshot_selected_page_url(
        &mut self,
        app_name: &str,
        pid: i32,
        mcp: &(impl Mcp + ?Sized),
    ) {
        cdp_lifecycle::snapshot_selected_page_url(mcp, &mut self.cdp_state, app_name, pid).await;
    }

    /// Connect to CDP with retries (the debug endpoint may not be ready
    /// immediately after app launch), then poll until pages are available.
    ///
    /// Delegates the retry loop to [`cdp_lifecycle::connect_with_retries`]
    /// and the readiness poll to [`cdp_lifecycle::poll_cdp_ready`], then
    /// wraps any failure in the executor's typed error variant.
    async fn cdp_connect_and_poll(
        &self,
        app_name: &str,
        port: u16,
        mcp: &(impl Mcp + ?Sized),
    ) -> ExecutorResult<()> {
        if let Err(last_err) = cdp_lifecycle::connect_with_retries(mcp, port).await {
            return Err(ExecutorError::CdpConnectTimeout {
                app_name: app_name.to_string(),
                attempts: CDP_CONNECT_MAX_ATTEMPTS,
                last_error: last_err,
            });
        }
        let ready =
            cdp_lifecycle::poll_cdp_ready(mcp, app_name, CDP_READY_TIMEOUT_AFTER_RELAUNCH_SECS)
                .await;
        match ready {
            Ok(()) => Ok(()),
            // The connect handshake itself returned Ok, but the server
            // never reported a page within the readiness window — same
            // user-visible failure shape as a connect timeout.
            Err(msg) => Err(ExecutorError::CdpConnectTimeout {
                app_name: app_name.to_string(),
                attempts: CDP_CONNECT_MAX_ATTEMPTS,
                last_error: msg,
            }),
        }
    }

    /// Try to connect CDP to an app, returning true on success.
    /// Disconnects on failure to avoid leaving a stale connection.
    async fn try_cdp_connect(&self, app_name: &str, port: u16, mcp: &(impl Mcp + ?Sized)) -> bool {
        let ok = matches!(
            mcp.call_tool("cdp_connect", Some(cdp_lifecycle::connect_args(port)))
                .await,
            Ok(r) if r.is_error != Some(true)
        );
        if !ok {
            return false;
        }
        if cdp_lifecycle::poll_cdp_ready(mcp, app_name, CDP_READY_TIMEOUT_REUSE_SECS)
            .await
            .is_ok()
        {
            true
        } else {
            super::best_effort::best_effort_tool_call(
                mcp,
                "cdp_disconnect",
                None,
                "try_cdp_connect: disconnect after poll_cdp_ready timeout",
            )
            .await;
            false
        }
    }

    /// Quit the app, confirm it exited, relaunch with --remote-debugging-port.
    ///
    /// For Chrome-family apps with a configured profile: kills only the
    /// profile-specific Chrome instance and launches directly, leaving the
    /// user's default Chrome untouched.
    ///
    /// Delegates the generic quit/poll/force-quit/launch sequence to
    /// [`cdp_lifecycle`]; the Chrome-profile branch is kept inline because
    /// it drives platform-specific `spawn_chrome` rather than the MCP
    /// `launch_app` tool.
    async fn relaunch_with_debug_port(
        &mut self,
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
            let dir = profile_path.to_string_lossy().to_string();
            super::kill_chrome_profile_instance(&dir).await;

            super::launch_chrome_with_profile_and_debug_port(&dir, port)
                .await
                .map_err(|e| ExecutorError::CdpRelaunchFailed {
                    app_name: app_name.to_string(),
                    message: format!("Failed to launch with debug port: {}", e),
                })?;
        } else {
            let quit_outcome =
                cdp_lifecycle::quit_and_wait(mcp, app_name, &mut self.cdp_state).await;

            if matches!(quit_outcome, QuitOutcome::TimedOut) {
                self.log(format!(
                    "'{}' did not quit within {}s, force-killing",
                    app_name,
                    (APP_QUIT_POLL_INTERVAL.as_millis() as u32 * APP_QUIT_MAX_ATTEMPTS) / 1000,
                ));
                cdp_lifecycle::force_quit(mcp, app_name).await;
            }

            kill_all_processes(app_name).await;

            cdp_lifecycle::launch_with_debug_port(mcp, app_name, port)
                .await
                .map_err(|message| ExecutorError::CdpRelaunchFailed {
                    app_name: app_name.to_string(),
                    message,
                })?;
        }

        // Wait for the app to start up.
        cdp_lifecycle::warmup_after_relaunch().await;
        Ok(())
    }
}

/// Parse a UID from a CDP snapshot line.
/// Handles both `uid=1_0 ...` and `[uid="e1"] ...` formats.
fn parse_snapshot_uid(line: &str) -> Option<String> {
    let uid_pos = line.find("uid=")?;
    let rest = &line[uid_pos + 4..];
    if let Some(quoted) = rest.strip_prefix('"') {
        let end = quoted.find('"')?;
        let uid = &quoted[..end];
        if uid.is_empty() {
            return None;
        }
        Some(uid.to_string())
    } else {
        let end = rest.find(' ').unwrap_or(rest.len());
        let uid = &rest[..end];
        if uid.is_empty() {
            return None;
        }
        Some(uid.to_string())
    }
}

/// Scan a CDP snapshot for lines whose label contains `target` (case-insensitive)
/// and carry a UID. Leaf text nodes (`StaticText`, `InlineTextBox`) are skipped
/// — those repeat the parent's label and would inflate the candidate count.
fn collect_snapshot_candidates(snapshot_text: &str, target: &str) -> Vec<CdpCandidate> {
    let target_lower = target.to_lowercase();
    let mut out = Vec::new();
    for line in snapshot_text.lines() {
        let Some(uid) = parse_snapshot_uid(line) else {
            continue;
        };
        let after_uid = line.trim_start();
        if after_uid.contains("StaticText") || after_uid.contains("InlineTextBox") {
            continue;
        }
        if line.to_lowercase().contains(&target_lower) {
            out.push(CdpCandidate {
                uid,
                snippet: after_uid.to_string(),
            });
        }
    }
    out
}

/// Detect UID-shaped strings: an AX/DOM prefix letter plus digits (`a5`, `d12`,
/// `e1`), or a two-number backend-node form (`1_0`). Anything else is treated
/// as a human-visible label or intent that must be re-resolved at runtime.
fn looks_like_cdp_uid(s: &str) -> bool {
    let s = s.trim();
    if s.is_empty() {
        return false;
    }
    // `a5`, `d12`, `e1`: single lowercase letter followed only by digits.
    let mut chars = s.chars();
    if let Some(first) = chars.next()
        && first.is_ascii_lowercase()
        && chars.clone().count() > 0
        && chars.all(|c| c.is_ascii_digit())
    {
        return true;
    }
    // `1_0`: digits, single underscore, digits.
    if let Some((lhs, rhs)) = s.split_once('_')
        && !lhs.is_empty()
        && !rhs.is_empty()
        && lhs.chars().all(|c| c.is_ascii_digit())
        && rhs.chars().all(|c| c.is_ascii_digit())
    {
        return true;
    }
    false
}

/// Check if an app is already running with `--remote-debugging-port=<N>`.
/// Returns the port if found, so the caller can skip the quit/relaunch cycle.
pub(crate) async fn existing_debug_port(app_name: &str) -> Option<u16> {
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
async fn kill_all_processes(app_name: &str) {
    #[cfg(not(target_os = "windows"))]
    {
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
    use super::{
        collect_snapshot_candidates, looks_like_cdp_uid, parse_snapshot_uid,
        windows_process_image_candidates,
    };

    #[test]
    fn looks_like_cdp_uid_accepts_prefixed_form() {
        assert!(looks_like_cdp_uid("a5"));
        assert!(looks_like_cdp_uid("d12"));
        assert!(looks_like_cdp_uid("e1"));
    }

    #[test]
    fn looks_like_cdp_uid_accepts_backend_node_form() {
        assert!(looks_like_cdp_uid("1_0"));
        assert!(looks_like_cdp_uid("42_7"));
    }

    #[test]
    fn looks_like_cdp_uid_rejects_labels() {
        assert!(!looks_like_cdp_uid("message input"));
        assert!(!looks_like_cdp_uid("Send"));
        assert!(!looks_like_cdp_uid(""));
        assert!(!looks_like_cdp_uid("a"));
        assert!(!looks_like_cdp_uid("5"));
        assert!(!looks_like_cdp_uid("a5b"));
        assert!(!looks_like_cdp_uid("_5"));
        assert!(!looks_like_cdp_uid("5_"));
        assert!(!looks_like_cdp_uid("A5"));
    }

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

    #[test]
    fn parse_snapshot_uid_handles_both_formats() {
        assert_eq!(
            parse_snapshot_uid("[uid=\"a5\"] button \"Submit\""),
            Some("a5".to_string())
        );
        assert_eq!(
            parse_snapshot_uid("  uid=1_0 button \"Submit\""),
            Some("1_0".to_string())
        );
        assert_eq!(parse_snapshot_uid("no uid here"), None);
    }

    #[test]
    fn collect_snapshot_candidates_skips_leaf_text_nodes() {
        let snapshot = concat!(
            "[uid=\"a1\"] button \"Submit\"\n",
            "[uid=\"a2\"] StaticText \"Submit\"\n",
            "[uid=\"a3\"] InlineTextBox \"Submit\"\n",
        );
        let candidates = collect_snapshot_candidates(snapshot, "Submit");
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].uid, "a1");
    }

    #[test]
    fn collect_snapshot_candidates_matches_case_insensitively() {
        let snapshot = "[uid=\"a1\"] button \"Submit\"";
        assert_eq!(collect_snapshot_candidates(snapshot, "submit").len(), 1);
        assert_eq!(collect_snapshot_candidates(snapshot, "SUBMIT").len(), 1);
    }

    #[test]
    fn collect_snapshot_candidates_returns_all_matches_for_disambiguation() {
        let snapshot = concat!(
            "[uid=\"a1\"] button \"Save\"\n",
            "[uid=\"a2\"] button \"Save\"\n",
            "[uid=\"a3\"] button \"Save\"\n",
        );
        let candidates = collect_snapshot_candidates(snapshot, "Save");
        assert_eq!(candidates.len(), 3);
        let uids: Vec<_> = candidates.iter().map(|c| c.uid.as_str()).collect();
        assert_eq!(uids, vec!["a1", "a2", "a3"]);
    }
}
