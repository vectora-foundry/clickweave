use super::super::{ExecutorError, ExecutorResult, Mcp, WorkflowExecutor};
use clickweave_core::ClickTarget;
use clickweave_core::NodeRun;
use clickweave_core::cdp::{SnapshotMatch, find_elements_in_snapshot};
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

impl<'a> CdpExpected<'a> {
    pub fn from_click_target(target: &'a ClickTarget) -> Self {
        match target {
            ClickTarget::CdpElement {
                role,
                href,
                parent_role,
                parent_name,
                ..
            } => Self {
                role: role.as_deref(),
                href: href.as_deref(),
                parent_role: parent_role.as_deref(),
                parent_name: parent_name.as_deref(),
            },
            _ => Self::default(),
        }
    }
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

        // Find matching elements
        let mut matches = find_elements_in_snapshot(&snapshot_text, target);
        clickweave_core::cdp::narrow_matches(&mut matches, expected.role, expected.href);
        clickweave_core::cdp::narrow_by_parent(
            &mut matches,
            expected.parent_role,
            expected.parent_name,
        );

        if matches.is_empty() {
            self.log(format!(
                "CDP: no exact match for '{}', trying LLM resolution",
                target
            ));
            self.resolve_cdp_element_name(target, &snapshot_text).await
        } else if matches.len() == 1 {
            Ok(matches[0].uid.clone())
        } else {
            self.log(format!(
                "CDP: {} matches for '{}', disambiguating",
                matches.len(),
                target
            ));
            self.disambiguate_cdp_elements(target, &matches).await
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
    ) -> ExecutorResult<String> {
        let uid = self.resolve_cdp_element_uid(target, expected, mcp).await?;

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
    ) -> ExecutorResult<String> {
        self.execute_cdp_action("click", target, expected, mcp, node_run)
            .await
    }

    /// Resolve a CDP element and hover it. Returns the hover result text.
    pub(in crate::executor) async fn resolve_and_hover_cdp(
        &self,
        target: &str,
        expected: &CdpExpected<'_>,
        mcp: &(impl Mcp + ?Sized),
        node_run: Option<&NodeRun>,
    ) -> ExecutorResult<String> {
        self.execute_cdp_action("hover", target, expected, mcp, node_run)
            .await
    }

    /// Ask the LLM to find the best matching element in the CDP snapshot.
    async fn resolve_cdp_element_name(
        &self,
        target: &str,
        snapshot_text: &str,
    ) -> ExecutorResult<String> {
        let truncated = &snapshot_text[..snapshot_text.floor_char_boundary(4000)];

        let prompt = format!(
            "Find the element in this page snapshot that best matches the target '{target}'.\n\
             Return ONLY the uid value, nothing else.\n\n\
             Page snapshot:\n{truncated}"
        );

        let response = self
            .reasoning_backend()
            .chat(vec![clickweave_llm::Message::user(prompt)], None)
            .await
            .map_err(|e| ExecutorError::Cdp(format!("LLM resolution failed: {e}")))?;

        let raw_text = response
            .choices
            .first()
            .and_then(|c| c.message.content_text())
            .ok_or_else(|| ExecutorError::Cdp("LLM returned empty content".to_string()))?;

        let uid = raw_text.trim().trim_matches('"').to_string();
        if uid.is_empty() {
            return Err(ExecutorError::Cdp(format!(
                "LLM could not resolve '{}' in CDP snapshot",
                target
            )));
        }

        // Validate that the UID actually appears in the snapshot.
        let uid_exists = snapshot_text.contains(&format!("uid=\"{}\"", uid))
            || snapshot_text.contains(&format!("uid={} ", uid))
            || snapshot_text.ends_with(&format!("uid={}", uid));
        if !uid_exists {
            return Err(ExecutorError::Cdp(format!(
                "LLM returned uid '{}' which does not exist in the CDP snapshot",
                uid
            )));
        }

        self.log(format!("CDP: LLM resolved '{}' -> uid='{}'", target, uid));
        Ok(uid)
    }

    /// Disambiguate between multiple CDP element matches using the LLM.
    async fn disambiguate_cdp_elements(
        &self,
        target: &str,
        matches: &[SnapshotMatch],
    ) -> ExecutorResult<String> {
        let valid_uids: std::collections::HashSet<&str> =
            matches.iter().map(|m| m.uid.as_str()).collect();

        let options: Vec<String> = matches
            .iter()
            .enumerate()
            .map(|(i, m)| format!("{}: uid={} — {}", i + 1, m.uid, m.label))
            .collect();

        let hint_context = self.format_supervision_hint("A previous click attempt failed. ");

        let tried_context = {
            let tried = self.read_tried_cdp_uids();
            Self::format_tried_context(&tried, "UIDs")
        };

        let prompt = format!(
            "Multiple elements match the target '{target}'. Which one is the best match?\n\
             Return ONLY the uid value, nothing else.\n\n{}{hint_context}{tried_context}",
            options.join("\n")
        );

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

        let uid = raw_text.trim().trim_matches('"').to_string();
        if valid_uids.contains(uid.as_str()) {
            self.write_tried_cdp_uids().push(uid.clone());
            Ok(uid)
        } else {
            self.log(format!(
                "CDP: LLM returned '{}' which is not in candidate set, using first match",
                uid
            ));
            Ok(matches[0].uid.clone())
        }
    }

    /// Ensure a CDP connection is available for the given Electron/Chrome app.
    ///
    /// If no CDP connection is active for this app:
    /// - Test mode: quit the app, relaunch with --remote-debugging-port, connect
    ///   via cdp_connect, poll until ready, store port in cache.
    /// - Run mode: read port from decision cache, try connecting, relaunch if needed.
    pub(in crate::executor) async fn ensure_cdp_connected(
        &mut self,
        _node_id: Uuid,
        app_name: &str,
        mcp: &(impl Mcp + ?Sized),
        node_run: Option<&NodeRun>,
    ) -> ExecutorResult<()> {
        use clickweave_core::ExecutionMode;
        use clickweave_core::decision_cache::CdpPort;

        // Already have a CDP connection for this app -- nothing to do.
        if self.cdp_connected_app.as_deref() == Some(app_name) {
            return Ok(());
        }

        // Disconnect from any previously connected app.
        if self.cdp_connected_app.is_some() {
            let _ = mcp.call_tool("cdp_disconnect", None).await;
            self.cdp_connected_app = None;
        }

        let port = if self.execution_mode == ExecutionMode::Test {
            // Check if app is already running with a debug port — skip the
            // quit/relaunch cycle and reuse the existing port.
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
                    existing_port
                } else {
                    // If the discovered port is stale/unreachable, fall back to
                    // the standard relaunch path.
                    self.log(format!(
                        "Existing debug port {} for '{}' was unreachable, relaunching",
                        existing_port, app_name
                    ));
                    let port = clickweave_core::cdp::rand_ephemeral_port();
                    self.log(format!(
                        "Restarting '{}' with DevTools enabled (port {})...",
                        app_name, port
                    ));
                    self.relaunch_with_debug_port(app_name, port, mcp).await?;
                    self.evict_app_cache(app_name);
                    self.write_decision_cache()
                        .cdp_port
                        .insert(app_name.to_string(), CdpPort { port });
                    self.cdp_connect_and_poll(app_name, port, mcp).await?;
                    port
                }
            } else {
                // Test mode: pick a random port, relaunch the app, connect.
                let port = clickweave_core::cdp::rand_ephemeral_port();
                self.log(format!(
                    "Restarting '{}' with DevTools enabled (port {})...",
                    app_name, port
                ));
                self.relaunch_with_debug_port(app_name, port, mcp).await?;
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
                self.relaunch_with_debug_port(app_name, port, mcp).await?;
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

        self.cdp_connected_app = Some(app_name.to_string());
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
        let ok = match mcp
            .call_tool("cdp_connect", Some(serde_json::json!({"port": port})))
            .await
        {
            Ok(r) if r.is_error != Some(true) => true,
            _ => false,
        };
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
    async fn relaunch_with_debug_port(
        &self,
        app_name: &str,
        port: u16,
        mcp: &(impl Mcp + ?Sized),
    ) -> ExecutorResult<()> {
        // Quit (best-effort -- app might not be running).
        let quit_args = serde_json::json!({ "app_name": app_name });
        if let Err(e) = mcp.call_tool("quit_app", Some(quit_args)).await {
            self.log(format!(
                "quit_app for '{}' failed (continuing): {}",
                app_name, e
            ));
        }

        // Poll list_apps until the app is no longer running (up to 10s).
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

        // Kill any remaining helper processes (e.g. Chrome GPU/renderer/crashpad).
        // Multi-process apps keep their profile lock until all sub-processes exit,
        // which prevents the relaunched instance from opening --remote-debugging-port.
        kill_all_processes(app_name).await;

        // Relaunch with debug port.
        // Chrome 136+ requires --user-data-dir to point to a non-default directory for
        // --remote-debugging-port to open. We use a persistent per-app debug profile dir
        // and copy session cookies from the user's real Chrome profile so they stay
        // logged in. Electron apps don't need this treatment.
        let mut args = vec![format!("--remote-debugging-port={}", port)];
        if let Some(debug_dir) = chrome_debug_profile_dir(app_name) {
            copy_chrome_session_cookies(app_name, &debug_dir).await;
            args.push(format!("--user-data-dir={}", debug_dir));
            args.push("--no-first-run".to_string());
            args.push("--no-default-browser-check".to_string());
        }
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
        // Use -f (full command line) rather than -x (process name) because Chrome
        // spawns sub-processes with names like "Google Chrome Helper (GPU)" that
        // all contain the app name in their command line. -x would only match the
        // main process, leaving helpers alive and holding the profile lock.
        let _ = tokio::process::Command::new("pkill")
            .args(["-f", app_name])
            .output()
            .await;

        for _ in 0..10 {
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            let still_alive = tokio::process::Command::new("pgrep")
                .args(["-f", app_name])
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

/// Return a persistent non-default debug profile directory for Chrome-family browsers,
/// or `None` for non-Chrome apps (e.g. Electron).
///
/// Chrome 136+ blocks `--remote-debugging-port` when the user-data-dir is the default
/// profile. We use a separate persistent directory so the debug port opens, while
/// keeping the user logged in by copying their session cookies into it.
fn chrome_debug_profile_dir(app_name: &str) -> Option<String> {
    let lower = app_name.to_lowercase();
    if !lower.contains("chrome") && !lower.contains("chromium") {
        return None;
    }

    #[cfg(target_os = "macos")]
    {
        std::env::var("HOME").ok().map(|home| {
            format!(
                "{}/Library/Application Support/Google/Chrome-Clickweave",
                home
            )
        })
    }
    #[cfg(target_os = "windows")]
    {
        std::env::var("LOCALAPPDATA")
            .ok()
            .map(|local| format!("{}\\Google\\Chrome-Clickweave", local))
    }
    #[cfg(target_os = "linux")]
    {
        std::env::var("HOME")
            .ok()
            .map(|home| format!("{}/.config/google-chrome-clickweave", home))
    }
}

/// Copy session cookies from the user's real Chrome profile into the debug profile
/// directory so the user stays logged in when Chrome opens the debug profile.
///
/// Chrome on macOS encrypts cookies using a machine-wide key in the system Keychain,
/// so cookies copied from any profile are decryptable in any other profile on the same
/// machine. This is a best-effort operation — failures are silently ignored.
///
/// Callers must only call this for Chrome-family apps (already guaranteed by the
/// `chrome_debug_profile_dir` check upstream that gates the call).
async fn copy_chrome_session_cookies(_app_name: &str, debug_dir: &str) {
    use std::path::Path;

    #[cfg(target_os = "macos")]
    let source_dir = match std::env::var("HOME").ok() {
        Some(home) => Path::new(&home)
            .join("Library/Application Support/Google/Chrome")
            .into_os_string()
            .into_string()
            .unwrap_or_default(),
        None => return,
    };
    #[cfg(target_os = "windows")]
    let source_dir = match std::env::var("LOCALAPPDATA").ok() {
        Some(local) => Path::new(&local)
            .join("Google")
            .join("Chrome")
            .join("User Data")
            .into_os_string()
            .into_string()
            .unwrap_or_default(),
        None => return,
    };
    #[cfg(target_os = "linux")]
    let source_dir = match std::env::var("HOME").ok() {
        Some(home) => Path::new(&home)
            .join(".config/google-chrome")
            .into_os_string()
            .into_string()
            .unwrap_or_default(),
        None => return,
    };

    if source_dir.is_empty() {
        return;
    }

    // Chrome 97+ stores cookies in Default/Network/Cookies.
    // Older versions use Default/Cookies. Copy from both locations if present.
    //
    // Do NOT copy Preferences: it contains signed-in account metadata that causes
    // Chrome to run account re-verification on the new profile path. That verification
    // strips the .google.com session cookies (SID, SSID, APISID, etc.) as a security
    // measure, leaving the user signed out. Without Preferences, Chrome treats the
    // debug profile as a plain session and preserves all copied cookies as-is.
    let cookie_locations: &[&str] = &["Default/Network/Cookies", "Default/Cookies"];
    for rel in cookie_locations {
        let src = Path::new(&source_dir).join(rel);
        let dst = Path::new(debug_dir).join(rel);
        if let Some(parent) = dst.parent() {
            let _ = tokio::fs::create_dir_all(parent).await;
        }
        let _ = tokio::fs::copy(&src, &dst).await;
    }
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
