use super::super::{ExecutorError, ExecutorResult, WorkflowExecutor};
use clickweave_core::ClickTarget;
use clickweave_core::NodeRun;
use clickweave_core::cdp::{SnapshotMatch, find_elements_in_snapshot};
use clickweave_llm::ChatBackend;
use clickweave_mcp::{McpRouter, ToolProvider};
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

/// Pick a random port in the ephemeral range (49152-65535).
fn rand_ephemeral_port() -> u16 {
    use std::time::SystemTime;
    let seed = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos();
    let raw = seed.wrapping_mul(1664525).wrapping_add(1013904223);
    let range = 65535 - 49152;
    49152 + (raw % range) as u16
}

/// Build the McpServerConfig for a chrome-devtools-mcp connected to a specific port.
fn cdp_server_config(server_name: &str, port: u16) -> clickweave_mcp::McpServerConfig {
    clickweave_mcp::McpServerConfig {
        name: server_name.to_string(),
        command: "npx".into(),
        args: vec![
            "-y".into(),
            "chrome-devtools-mcp".into(),
            format!("--browserUrl=http://127.0.0.1:{}", port),
        ],
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
        cdp_server: &str,
        mcp: &(impl ToolProvider + ?Sized),
    ) -> ExecutorResult<String> {
        // 1. Ensure a page is selected (list_pages triggers auto-selection
        //    inside chrome-devtools-mcp).
        let _ = mcp
            .call_tool_on(cdp_server, "list_pages", Some(serde_json::json!({})))
            .await;

        // 2. Take CDP snapshot
        self.log(format!("CDP: taking snapshot to find '{}'", target));
        let snapshot_result = mcp
            .call_tool_on(cdp_server, "take_snapshot", Some(serde_json::json!({})))
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

        // 3. Find matching elements
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
        cdp_server: &str,
        mcp: &(impl ToolProvider + ?Sized),
        node_run: Option<&NodeRun>,
    ) -> ExecutorResult<String> {
        let uid = self
            .resolve_cdp_element_uid(target, expected, cdp_server, mcp)
            .await?;

        self.log(format!("CDP: {} element uid='{}'", action, uid));
        let result = mcp
            .call_tool_on(cdp_server, action, Some(serde_json::json!({ "uid": uid })))
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
        cdp_server: &str,
        mcp: &(impl ToolProvider + ?Sized),
        node_run: Option<&NodeRun>,
    ) -> ExecutorResult<String> {
        self.execute_cdp_action("click", target, expected, cdp_server, mcp, node_run)
            .await
    }

    /// Resolve a CDP element and hover it. Returns the hover result text.
    pub(in crate::executor) async fn resolve_and_hover_cdp(
        &self,
        target: &str,
        expected: &CdpExpected<'_>,
        cdp_server: &str,
        mcp: &(impl ToolProvider + ?Sized),
        node_run: Option<&NodeRun>,
    ) -> ExecutorResult<String> {
        self.execute_cdp_action("hover", target, expected, cdp_server, mcp, node_run)
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

    /// Ensure a CDP server is available for the given Electron/Chrome app.
    ///
    /// If no CDP server is registered for this app:
    /// - Test mode: quit the app, relaunch with --remote-debugging-port, spawn
    ///   a chrome-devtools-mcp server, poll until ready, store port in cache.
    /// - Run mode: read port from decision cache, try connecting, relaunch if needed.
    ///
    /// Returns the CDP server name on success.
    pub(in crate::executor) async fn ensure_cdp_server(
        &mut self,
        _node_id: Uuid,
        app_name: &str,
        mcp: &mut McpRouter,
        node_run: Option<&NodeRun>,
    ) -> ExecutorResult<String> {
        use clickweave_core::ExecutionMode;
        use clickweave_core::cdp::cdp_server_name;
        use clickweave_core::decision_cache::CdpPort;

        let server_name = cdp_server_name(app_name);

        // Already have a CDP server for this app -- nothing to do.
        if self.cdp_servers.contains_key(app_name) {
            return Ok(server_name);
        }

        let port = if self.execution_mode == ExecutionMode::Test {
            // Test mode: pick a random port, relaunch the app.
            let port = rand_ephemeral_port();
            self.log(format!(
                "Restarting '{}' with DevTools enabled (port {})...",
                app_name, port
            ));
            self.relaunch_with_debug_port(app_name, port, mcp).await?;
            // App was restarted -- evict stale PID from app cache.
            self.evict_app_cache(app_name);
            // Store in decision cache for Run mode replay.
            self.write_decision_cache()
                .cdp_port
                .insert(app_name.to_string(), CdpPort { port });
            port
        } else {
            // Run mode: read cached port, try connecting, relaunch if needed.
            let cached = self
                .read_decision_cache()
                .cdp_port
                .get(app_name)
                .map(|e| e.port);

            let port = cached.ok_or_else(|| {
                ExecutorError::Cdp(format!(
                    "No cached CDP port for '{}'. Run in Test mode first.",
                    app_name
                ))
            })?;

            // Try spawning CDP server with cached port (app may still be running).
            let config = cdp_server_config(&server_name, port);
            let connect_ok = mcp.spawn_server(&config).await.is_ok()
                && self.poll_cdp_ready(&server_name, mcp, 5).await.is_ok();

            if !connect_ok {
                self.log(format!(
                    "CDP connection failed for '{}', relaunching with port {}...",
                    app_name, port
                ));
                self.relaunch_with_debug_port(app_name, port, mcp).await?;
                // App was restarted -- evict stale PID from app cache.
                self.evict_app_cache(app_name);
            }
            port
        };

        // Spawn the CDP server if not already connected.
        if !mcp.has_server(&server_name) {
            let config = cdp_server_config(&server_name, port);
            mcp.spawn_server(&config).await.map_err(|e| {
                ExecutorError::Cdp(format!(
                    "Failed to start CDP server for '{}': {}",
                    app_name, e
                ))
            })?;
        }

        // Poll until the app is ready for CDP.
        self.poll_cdp_ready(&server_name, mcp, 30).await?;

        self.log(format!(
            "CDP connected to '{}' (port {}, server '{}')",
            app_name, port, server_name
        ));
        self.record_event(
            node_run,
            "cdp_connected",
            serde_json::json!({
                "app_name": app_name,
                "port": port,
                "server_name": server_name,
            }),
        );

        self.cdp_servers
            .insert(app_name.to_string(), server_name.clone());
        Ok(server_name)
    }

    /// Quit the app, confirm it exited, relaunch with --remote-debugging-port.
    async fn relaunch_with_debug_port(
        &self,
        app_name: &str,
        port: u16,
        mcp: &McpRouter,
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

        // Relaunch with debug port.
        let launch_args = serde_json::json!({
            "app_name": app_name,
            "args": [format!("--remote-debugging-port={}", port)],
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

    /// Poll `list_pages` on a CDP server until it returns at least one page.
    pub(in crate::executor) async fn poll_cdp_ready(
        &self,
        server_name: &str,
        mcp: &McpRouter,
        timeout_secs: u64,
    ) -> ExecutorResult<()> {
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(timeout_secs);

        loop {
            match mcp
                .call_tool_on(server_name, "list_pages", Some(serde_json::json!({})))
                .await
            {
                Ok(result) if result.is_error != Some(true) => {
                    let text = Self::extract_result_text(&result);
                    // Page index may be 0-based or 1-based depending on MCP
                    // server version -- check for any "N: <url>" page entry.
                    if text.lines().any(|l| {
                        l.as_bytes().first().is_some_and(|b| b.is_ascii_digit()) && l.contains(": ")
                    }) {
                        self.log(format!("CDP pages for '{}': {}", server_name, text.trim()));
                        return Ok(());
                    }
                    tracing::debug!(
                        "CDP list_pages for '{}' returned but no pages yet: {:?}",
                        server_name,
                        &text[..text.len().min(500)]
                    );
                }
                Ok(result) => {
                    let text = Self::extract_result_text(&result);
                    tracing::debug!(
                        "CDP list_pages error for '{}': {}",
                        server_name,
                        &text[..text.len().min(500)]
                    );
                }
                Err(e) => {
                    tracing::debug!("CDP list_pages call failed for '{}': {}", server_name, e);
                }
            }

            if tokio::time::Instant::now() >= deadline {
                return Err(ExecutorError::Cdp(format!(
                    "Timed out waiting for CDP server '{}' to be ready ({}s)",
                    server_name, timeout_secs
                )));
            }

            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        }
    }
}
