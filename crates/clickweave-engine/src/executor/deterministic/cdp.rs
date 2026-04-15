use std::path::Path;

use super::super::retry_context::RetryContext;
use super::super::{CdpCandidate, ExecutorError, ExecutorResult, Mcp, WorkflowExecutor};
use clickweave_core::NodeRun;
use clickweave_llm::ChatBackend;
use uuid::Uuid;

impl<C: ChatBackend> WorkflowExecutor<C> {
    /// Simple element resolution: take a snapshot, find the first matching
    /// element by text, and return its UID. No LLM disambiguation or
    /// multi-tier fallbacks — the agent architecture handles that.
    pub(in crate::executor) async fn resolve_cdp_element_uid(
        &self,
        target: &str,
        mcp: &(impl Mcp + ?Sized),
    ) -> ExecutorResult<String> {
        if target.trim().is_empty() {
            return Err(ExecutorError::Cdp(
                "CDP target is empty; expected a non-empty label or text".to_string(),
            ));
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
                .map_err(|e| ExecutorError::Cdp(format!("take_snapshot failed: {e}")))?;

            if snapshot_result.is_error == Some(true) {
                let error_text = Self::extract_result_text(&snapshot_result);
                return Err(ExecutorError::Cdp(format!(
                    "take_snapshot error: {}",
                    error_text
                )));
            }

            let snapshot_text = Self::extract_result_text(&snapshot_result);
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

        Err(ExecutorError::Cdp(format!(
            "No matching element for '{}' in CDP snapshot",
            target
        )))
    }

    /// Resolve a CDP element and perform an action (click or hover) on it.
    /// Returns the action result text.
    pub(in crate::executor) async fn execute_cdp_action(
        &self,
        action: &str,
        _node_id: Uuid,
        target: &str,
        mcp: &(impl Mcp + ?Sized),
        node_run: Option<&NodeRun>,
        _retry_ctx: &RetryContext,
    ) -> ExecutorResult<String> {
        let uid = self.resolve_cdp_element_uid(target, mcp).await?;

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
    pub(in crate::executor) async fn resolve_cdp_target_uid(
        &self,
        target: &clickweave_core::CdpTarget,
        mcp: &(impl Mcp + ?Sized),
    ) -> ExecutorResult<String> {
        use clickweave_core::CdpTarget;
        match target {
            CdpTarget::ResolvedUid(uid) => Ok(uid.clone()),
            CdpTarget::ExactLabel(s) if looks_like_cdp_uid(s) => Ok(s.clone()),
            CdpTarget::ExactLabel(label) => self.resolve_cdp_element_uid(label, mcp).await,
            CdpTarget::Intent(intent) => self.resolve_cdp_element_uid(intent, mcp).await,
        }
    }

    /// Resolve a CDP element and hover it. Returns the hover result text.
    pub(in crate::executor) async fn resolve_and_hover_cdp(
        &self,
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

        // Already have a CDP connection for this exact app instance -- nothing to do.
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
