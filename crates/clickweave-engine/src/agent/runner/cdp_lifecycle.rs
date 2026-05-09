use super::*;

impl StateRunner {
    // -----------------------------------------------------------------
    // Task 3a.6: CDP auto-connect + synthetic focus_window skip
    // -----------------------------------------------------------------

    /// Record a per-app `kind` hint learned from a structured MCP
    /// response or `probe_app`. Port of the legacy
    /// `AgentRunner::record_app_kind`.
    pub(super) fn record_app_kind(&mut self, app_name: &str, kind: &str) {
        self.known_app_kinds
            .insert(app_name.to_string(), kind.to_string());
    }

    /// Compute the per-turn `<tools_in_scope>` subset from the current
    /// world-model state. No focused app yet → empty `Vec` → caller
    /// renders no block, so the LLM falls back to the system prompt's
    /// full `Available tools:` listing.
    pub(super) fn compute_tools_in_scope(&self, advertised_tool_names: &[String]) -> Vec<String> {
        crate::agent::prompt::tools_in_scope(
            self.world_model.focused_app_kind(),
            self.world_model.is_cdp_attached(),
            advertised_tool_names,
        )
    }

    /// True when `(tool_name, result_text)` identifies a runner-skipped
    /// `focus_window` — one of the synthetic successes that
    /// [`Self::should_skip_focus_window`] emits. Post-step bookkeeping
    /// (CDP auto-connect, workflow-node creation) consults this so the
    /// skipped call stays invisible to both the CDP lifecycle and the
    /// graph. Port of `AgentRunner::is_synthetic_focus_skip`.
    pub(crate) fn is_synthetic_focus_skip(tool_name: &str, result_text: &str) -> bool {
        tool_name == "focus_window" && FocusSkipReason::from_llm_message(result_text).is_some()
    }

    /// Decide whether to suppress a `focus_window` MCP call. Returns a
    /// [`FocusSkipReason`] in three cases: (1) operator set
    /// `allow_focus_window = false`, (2) Native app with full AX
    /// dispatch toolset, (3) Electron / Chrome with a live CDP session
    /// and the minimum CDP dispatch toolset. Otherwise `None` —
    /// fall-through to the real MCP call.
    ///
    /// Port of the legacy `AgentRunner::should_skip_focus_window`.
    pub(super) fn should_skip_focus_window<M: Mcp + ?Sized>(
        &self,
        arguments: &Value,
        mcp: &M,
    ) -> Option<FocusSkipReason> {
        // User-policy short-circuit takes precedence over kind / toolset
        // checks — the operator explicitly asked for "no focus changes,
        // ever".
        if !self.config.allow_focus_window {
            return Some(FocusSkipReason::PolicyDisabled);
        }
        let app_name = arguments.get("app_name").and_then(Value::as_str)?;
        match self.known_app_kinds.get(app_name).map(String::as_str) {
            Some("Native") if mcp_has_toolset(mcp, AX_DISPATCH_TOOLSET) => {
                Some(FocusSkipReason::AxAvailable)
            }
            Some("ElectronApp" | "ChromeBrowser")
                if self.cdp_state.is_connected_to(app_name, 0)
                    && mcp_has_toolset(mcp, CDP_DISPATCH_TOOLSET) =>
            {
                Some(FocusSkipReason::CdpLive)
            }
            // Pre-CDP-connect: kind is Electron/Chrome and the server
            // can attach via `cdp_connect`. The post-tool hook's
            // `auto_connect_cdp` will discover the debug port (or
            // quit + relaunch with one) on its own, so a preceding
            // `focus_window` is unnecessary and only steals foreground.
            Some("ElectronApp" | "ChromeBrowser") if mcp.has_tool("cdp_connect") => {
                Some(FocusSkipReason::CdpAttachable)
            }
            _ => None,
        }
    }

    /// Return the app target whose CDP session should be acquired after a
    /// synthetic `focus_window` skip. `CdpAttachable` always promises this
    /// path. `PolicyDisabled` also needs it for background Electron/Chrome
    /// work: suppressing the focus steal must not suppress the app-scoped
    /// CDP lifecycle that would otherwise attach to the target.
    pub(super) fn cdp_target_for_skipped_focus_window<M: Mcp + ?Sized>(
        &self,
        reason: FocusSkipReason,
        arguments: &Value,
        mcp: &M,
    ) -> Option<(String, Option<String>)> {
        if !mcp.has_tool("cdp_connect") {
            return None;
        }
        let app_name = arguments.get("app_name").and_then(Value::as_str)?;
        if self.cdp_state.is_connected_to(app_name, 0) {
            return None;
        }
        let kind_hint = self.known_app_kinds.get(app_name).cloned();
        match reason {
            FocusSkipReason::CdpAttachable => Some((app_name.to_string(), kind_hint)),
            FocusSkipReason::PolicyDisabled => match kind_hint.as_deref() {
                Some("Native") => None,
                Some("ElectronApp" | "ChromeBrowser" | "electron_app" | "chrome_browser") => {
                    Some((app_name.to_string(), kind_hint))
                }
                // Unknown kind: let `auto_connect_cdp` probe. Native apps
                // short-circuit there; Electron/Chrome targets get an
                // app-scoped debug session without a foreground focus steal.
                None => Some((app_name.to_string(), None)),
                Some(_) => None,
            },
            _ => None,
        }
    }

    /// Under the no-focus policy, suppress a no-args `launch_app` when
    /// the target process is already running. Native-devtools treats that
    /// shape as "bring the app to the front"; for CDP-capable apps the
    /// runner can attach in the background instead.
    pub(super) async fn running_app_for_no_focus_launch<M: Mcp + ?Sized>(
        &self,
        arguments: &Value,
        mcp: &M,
    ) -> Option<RunningAppInfo> {
        if self.config.allow_focus_window || !mcp.has_tool("list_apps") {
            return None;
        }
        if launch_app_has_launch_only_args(arguments) {
            return None;
        }
        let app_name = arguments.get("app_name").and_then(Value::as_str)?;
        if app_name.trim().is_empty() {
            return None;
        }

        let list_args = serde_json::json!({
            "app_name": app_name,
            "user_apps_only": true,
        });
        match mcp.call_tool("list_apps", Some(list_args)).await {
            Ok(result) if result.is_error != Some(true) => {
                let text = extract_result_text(&result);
                let entries: Vec<Value> = match serde_json::from_str(&text) {
                    Ok(entries) => entries,
                    Err(e) => {
                        debug!(
                            app = app_name,
                            error = %e,
                            "state-spine: list_apps parse failed during no-focus launch guard"
                        );
                        return None;
                    }
                };
                entries.into_iter().find_map(|entry| {
                    let name = entry.get("name").and_then(Value::as_str)?;
                    if !name.eq_ignore_ascii_case(app_name) {
                        return None;
                    }
                    let pid = entry
                        .get("pid")
                        .and_then(Value::as_i64)
                        .and_then(|pid| i32::try_from(pid).ok());
                    let kind = entry
                        .get("kind")
                        .and_then(Value::as_str)
                        .filter(|kind| !kind.trim().is_empty())
                        .map(str::to_string);
                    Some(RunningAppInfo {
                        name: name.to_string(),
                        pid,
                        kind,
                    })
                })
            }
            Ok(result) => {
                debug!(
                    app = app_name,
                    error = %extract_result_text(&result),
                    "state-spine: list_apps returned error during no-focus launch guard"
                );
                None
            }
            Err(e) => {
                debug!(
                    app = app_name,
                    error = %e,
                    "state-spine: list_apps failed during no-focus launch guard"
                );
                None
            }
        }
    }

    pub(super) fn skipped_launch_result_text(info: &RunningAppInfo) -> String {
        let mut body = serde_json::Map::new();
        body.insert("app_name".to_string(), Value::String(info.name.clone()));
        body.insert(
            "message".to_string(),
            Value::String(
                "launch_app skipped: app is already running; foreground focus not required"
                    .to_string(),
            ),
        );
        if let Some(pid) = info.pid {
            body.insert("pid".to_string(), Value::Number(pid.into()));
        }
        if let Some(kind) = &info.kind {
            body.insert("kind".to_string(), Value::String(kind.clone()));
        }
        Value::Object(body).to_string()
    }

    /// Block raw model-authored CDP lifecycle operations. The agent runner
    /// owns app-scoped CDP acquisition so the model cannot attach to an
    /// unrelated app listening on a guessed port like 9222.
    pub(super) fn raw_cdp_lifecycle_blocked(tool_name: &str, arguments: &Value) -> Option<String> {
        match tool_name {
            "cdp_connect" => {
                let port = arguments
                    .get("port")
                    .and_then(Value::as_u64)
                    .map(|p| format!(" Requested port was {p}."))
                    .unwrap_or_default();
                Some(format!(
                    "raw cdp_connect blocked: CDP connection lifecycle is runtime-managed. \
                     Do not guess debug ports.{port} Use launch_app or focus_window for the \
                     target Electron/Chrome app; the runner will reuse an existing \
                     --remote-debugging-port or relaunch that app with an ephemeral debug port, \
                     then attach CDP."
                ))
            }
            "cdp_disconnect" => Some(
                "raw cdp_disconnect blocked: CDP connection lifecycle is runtime-managed. \
                 The runner disconnects or reattaches when the target app changes; choose the \
                 next app action or agent_replan instead."
                    .to_string(),
            ),
            _ => None,
        }
    }

    /// Reject a coordinate-primitive tool (`click` / `type_text` /
    /// `press_key` / `move_mouse` / `scroll` / `drag`) when a structured
    /// surface is wired for the current focused app: a live CDP page, or
    /// a Native focus with the full AX dispatch toolset advertised.
    ///
    /// Defense-in-depth behind the per-turn `<tools_in_scope>` filter:
    /// the filter narrows the LLM's *advertised* tool list, but this
    /// guard rejects the *dispatched* call so a wrong-family choice
    /// (malformed turn, future replay path, future LLM regression)
    /// cannot reach MCP. Returns `Some(reason)` when the
    /// dispatch must be blocked; `None` otherwise.
    pub(super) fn coordinate_primitive_blocked<M: Mcp + ?Sized>(
        &self,
        tool_name: &str,
        mcp: &M,
    ) -> Option<String> {
        use crate::agent::world_model::AppKind;
        if !is_coordinate_primitive(tool_name) {
            return None;
        }
        let Some(kind) = self.world_model.focused_app_kind() else {
            // Without a known focused-app kind we cannot tell which
            // structured surface (if any) is wired — defer to legacy
            // behavior.
            return None;
        };
        match kind {
            AppKind::ElectronApp | AppKind::ChromeBrowser
                if self.world_model.cdp_page.is_some() =>
            {
                Some(format!(
                    "coordinate primitive `{tool_name}` blocked: focused app is \
                     CDP-backed and a `cdp_page` is live in <world_model>. Coordinate \
                     clicks bypass the page's event loop and steal foreground. Use \
                     `cdp_click` / `cdp_fill` / `cdp_type_text` / `cdp_press_key` \
                     against `d<N>` uids returned by `cdp_find_elements`."
                ))
            }
            AppKind::Native if mcp_has_toolset(mcp, AX_DISPATCH_TOOLSET) => Some(format!(
                "coordinate primitive `{tool_name}` blocked: focused app is Native and \
                 AX dispatch is wired. Coordinate primitives steal focus and produce \
                 no `a<N>` uids the next turn can target. Call `take_ax_snapshot` then \
                 `ax_click` / `ax_set_value` / `ax_select` against the `a<N>` uids."
            )),
            _ => None,
        }
    }

    /// Resolve the app identity for CDP probing from a successful
    /// `focus_window` / `launch_app` call. Returns `(app_name, kind)`
    /// where `kind` is a pre-classified `AppKind` string
    /// (`"ElectronApp"`, `"ChromeBrowser"`, `"Native"`) when the MCP
    /// server already told us. Port of the legacy
    /// `AgentRunner::resolve_cdp_target`.
    pub(super) async fn resolve_cdp_target<M: Mcp + ?Sized>(
        arguments: &Value,
        result_text: &str,
        mcp: &M,
    ) -> Option<(String, Option<String>)> {
        // 1. Structured MCP response (modern focus_window / launch_app).
        if let Ok(parsed) = serde_json::from_str::<Value>(result_text)
            && let Some(name) = parsed.get("app_name").and_then(Value::as_str)
            && !name.is_empty()
        {
            let kind = parsed
                .get("kind")
                .and_then(Value::as_str)
                .map(str::to_owned);
            return Some((name.to_string(), kind));
        }
        // 2. Direct argument (fast, backwards-compatible).
        if let Some(name) = arguments["app_name"].as_str() {
            return Some((name.to_string(), None));
        }
        // 3. pid → list_apps fallback.
        if let Some(pid) = arguments["pid"].as_u64()
            && mcp.has_tool("list_apps")
        {
            match mcp
                .call_tool("list_apps", Some(serde_json::json!({})))
                .await
            {
                Ok(r) if r.is_error != Some(true) => {
                    let text = crate::cdp_lifecycle::extract_text(&r);
                    if let Ok(entries) = serde_json::from_str::<Vec<Value>>(&text)
                        && let Some(name) = entries.iter().find_map(|entry| {
                            if entry["pid"].as_u64() == Some(pid) {
                                entry["name"].as_str().map(str::to_owned)
                            } else {
                                None
                            }
                        })
                    {
                        return Some((name, None));
                    }
                    debug!(pid, "state-spine: list_apps returned no entry matching pid");
                }
                Ok(r) => {
                    debug!(
                        error = %extract_result_text(&r),
                        "state-spine: list_apps returned error during CDP app-name resolution",
                    );
                }
                Err(e) => {
                    debug!(error = %e, "state-spine: list_apps call failed during CDP app-name resolution");
                }
            }
        }
        // 4. window_id → list_windows fallback.
        if let Some(window_id) = arguments["window_id"].as_u64()
            && mcp.has_tool("list_windows")
        {
            match mcp
                .call_tool("list_windows", Some(serde_json::json!({})))
                .await
            {
                Ok(r) if r.is_error != Some(true) => {
                    let text = crate::cdp_lifecycle::extract_text(&r);
                    if let Ok(entries) = serde_json::from_str::<Vec<Value>>(&text)
                        && let Some(name) = entries.iter().find_map(|entry| {
                            if entry["id"].as_u64() == Some(window_id) {
                                entry["owner_name"]
                                    .as_str()
                                    .or_else(|| entry["name"].as_str())
                                    .map(str::to_owned)
                            } else {
                                None
                            }
                        })
                    {
                        return Some((name, None));
                    }
                    debug!(
                        window_id,
                        "state-spine: list_windows returned no entry matching window_id",
                    );
                }
                Ok(r) => {
                    debug!(
                        error = %extract_result_text(&r),
                        "state-spine: list_windows returned error during CDP app-name resolution",
                    );
                }
                Err(e) => {
                    debug!(
                        error = %e,
                        "state-spine: list_windows call failed during CDP app-name resolution",
                    );
                }
            }
        }
        None
    }

    /// Post-connect bookkeeping: mark `(app_name, 0)` as the active CDP
    /// target and record the currently-selected page URL. Port of the
    /// legacy `AgentRunner::on_cdp_connected`.
    pub(super) async fn on_cdp_connected<M: Mcp + ?Sized>(
        &mut self,
        app_name: &str,
        _port: u16,
        mcp: &M,
    ) {
        self.cdp_state.set_connected(app_name, 0);
        // Successful connect supersedes any prior failure status — clear
        // it so the next turn's render does not show a stale error
        // alongside the now-live `cdp_page`.
        self.world_model.cdp_connect_status = None;
        crate::cdp_lifecycle::snapshot_selected_page_url(mcp, &mut self.cdp_state, app_name, 0)
            .await;
    }

    /// Record a permanent `auto_connect_cdp` failure on the world model.
    /// Called from each terminal error path in `auto_connect_cdp` so the
    /// next turn's state block surfaces the reason — without this, the
    /// LLM cannot distinguish "auto-connect hasn't fired yet" (no
    /// `cdp_page`, no status) from "auto-connect tried and failed" (no
    /// `cdp_page`, status present) and may keep waiting forever.
    pub(super) fn record_cdp_connect_failure(&mut self, reason: String) {
        use crate::agent::world_model::{Fresh, FreshnessSource};
        self.world_model.cdp_connect_status = Some(Fresh {
            value: reason,
            written_at: self.step_index,
            source: FreshnessSource::DirectObservation,
            ttl_steps: None,
        });
    }

    /// After a successful `launch_app` / `focus_window`, probe the app
    /// type and auto-connect CDP for Electron / Chrome targets. Returns
    /// `Some(port)` on success, `None` otherwise. Port of the legacy
    /// `AgentRunner::auto_connect_cdp`. Keeps best-effort semantics —
    /// every failure path logs and falls through.
    pub(super) async fn auto_connect_cdp<M: Mcp + ?Sized>(
        &mut self,
        app_name: &str,
        kind_hint: Option<&str>,
        mcp: &M,
    ) -> Option<u16> {
        if !mcp.has_tool("cdp_connect") {
            return None;
        }

        if !self.cdp_capable_app(app_name, kind_hint, mcp).await {
            return None;
        }

        tracing::info!(
            app = app_name,
            "state-spine: detected Electron/Chrome app, connecting CDP"
        );

        if let Some(port) = self.try_existing_cdp_port(app_name, mcp).await {
            return Some(port);
        }

        let port = clickweave_core::cdp::rand_ephemeral_port();
        if !self.relaunch_for_cdp(app_name, port, mcp).await {
            return None;
        }

        self.connect_relaunched_cdp(app_name, port, mcp).await
    }

    pub(super) async fn cdp_capable_app<M: Mcp + ?Sized>(
        &mut self,
        app_name: &str,
        kind_hint: Option<&str>,
        mcp: &M,
    ) -> bool {
        if matches!(kind_hint, Some("ElectronApp" | "ChromeBrowser")) {
            return true;
        }
        if matches!(kind_hint, Some("Native")) {
            debug!(
                app = app_name,
                "state-spine: kind hint says Native, skipping CDP"
            );
            return false;
        }
        if !mcp.has_tool("probe_app") {
            return false;
        }

        let Some(discovered_kind) = self.probe_app_kind_for_cdp(app_name, mcp).await else {
            return false;
        };
        self.record_probe_discovered_kind(app_name, discovered_kind);
        true
    }

    pub(super) async fn probe_app_kind_for_cdp<M: Mcp + ?Sized>(
        &mut self,
        app_name: &str,
        mcp: &M,
    ) -> Option<&'static str> {
        let probe_args = serde_json::json!({"app_name": app_name});
        self.emit_event(AgentEvent::SubAction {
            tool_name: "probe_app".to_string(),
            summary: format!("Auto: probing {} for CDP support", app_name),
        })
        .await;
        let probe_text = match mcp.call_tool("probe_app", Some(probe_args)).await {
            Ok(r) => {
                self.emit_event(AgentEvent::SubAction {
                    tool_name: "probe_app".to_string(),
                    summary: format!("Auto: probed {} (ok)", app_name),
                })
                .await;
                extract_result_text(&r)
            }
            Err(e) => {
                self.emit_event(AgentEvent::SubAction {
                    tool_name: "probe_app".to_string(),
                    summary: format!("Auto: probe_app failed for {}: {}", app_name, e),
                })
                .await;
                debug!(app = app_name, error = %e, "state-spine: probe_app failed, skipping CDP");
                self.record_cdp_connect_failure(format!("probe_app failed for {app_name}: {e}",));
                return None;
            }
        };

        if probe_text.contains("ChromeBrowser") {
            Some("ChromeBrowser")
        } else if probe_text.contains("ElectronApp") {
            Some("ElectronApp")
        } else {
            debug!(
                app = app_name,
                "state-spine: not an Electron/Chrome app, skipping CDP"
            );
            None
        }
    }

    pub(super) fn record_probe_discovered_kind(
        &mut self,
        app_name: &str,
        discovered_kind: &'static str,
    ) {
        // Keep `known_app_kinds` + `world_model.focused_app.kind` aligned with
        // the probe result. Otherwise unstructured launch/focus paths can leave
        // focused_app.kind = Native even after CDP attaches.
        self.record_app_kind(app_name, discovered_kind);
        if let Some(f) = self.world_model.focused_app.as_mut()
            && f.value.name == app_name
        {
            use crate::agent::world_model::AppKind;
            f.value.kind = match discovered_kind {
                "ChromeBrowser" => AppKind::ChromeBrowser,
                "ElectronApp" => AppKind::ElectronApp,
                _ => unreachable!("discovered_kind is constrained by probe_app_kind_for_cdp"),
            };
        }
    }

    pub(super) async fn try_existing_cdp_port<M: Mcp + ?Sized>(
        &mut self,
        app_name: &str,
        mcp: &M,
    ) -> Option<u16> {
        let port = crate::executor::cdp_helpers::existing_debug_port(app_name).await?;
        tracing::info!(
            app = app_name,
            port,
            "state-spine: reusing existing debug port"
        );
        if crate::cdp_lifecycle::connect_with_retries(mcp, port)
            .await
            .is_ok()
        {
            self.on_cdp_connected(app_name, port, mcp).await;
            Some(port)
        } else {
            None
        }
    }

    pub(super) async fn relaunch_for_cdp<M: Mcp + ?Sized>(
        &mut self,
        app_name: &str,
        port: u16,
        mcp: &M,
    ) -> bool {
        use crate::cdp_lifecycle;

        self.emit_event(AgentEvent::SubAction {
            tool_name: "quit_app".to_string(),
            summary: format!("Auto: quitting {} for CDP relaunch", app_name),
        })
        .await;
        let quit_outcome = cdp_lifecycle::quit_and_wait(mcp, app_name, &mut self.cdp_state).await;
        let quit_summary = match quit_outcome {
            cdp_lifecycle::QuitOutcome::Graceful => format!("Auto: {} quit confirmed", app_name),
            cdp_lifecycle::QuitOutcome::TimedOut => {
                format!("Auto: {} did not quit gracefully, force-killing", app_name)
            }
        };
        self.emit_event(AgentEvent::SubAction {
            tool_name: "quit_app".to_string(),
            summary: quit_summary,
        })
        .await;

        if matches!(quit_outcome, cdp_lifecycle::QuitOutcome::TimedOut) {
            warn!(
                app = app_name,
                "state-spine: app did not quit gracefully, force-killing"
            );
            cdp_lifecycle::force_quit(mcp, app_name).await;
        }

        self.emit_event(AgentEvent::SubAction {
            tool_name: "launch_app".to_string(),
            summary: format!("Auto: relaunching {} with debug port {}", app_name, port),
        })
        .await;
        match cdp_lifecycle::launch_with_debug_port(mcp, app_name, port).await {
            Ok(()) => {
                self.emit_event(AgentEvent::SubAction {
                    tool_name: "launch_app".to_string(),
                    summary: format!("Auto: relaunched {} (ok)", app_name),
                })
                .await;
            }
            Err(err) => {
                warn!(
                    app = app_name,
                    error = %err,
                    "state-spine: relaunch with debug port failed"
                );
                self.emit_event(AgentEvent::SubAction {
                    tool_name: "launch_app".to_string(),
                    summary: format!("Auto: relaunch failed for {}: {}", app_name, err),
                })
                .await;
                let fallback = serde_json::json!({"app_name": app_name});
                crate::executor::best_effort::best_effort_tool_call(
                    mcp,
                    "launch_app",
                    Some(fallback),
                    "state-spine fallback relaunch (debug-port launch failed)",
                )
                .await;
                self.record_cdp_connect_failure(format!(
                    "relaunch with debug port {port} failed for {app_name}: {err}",
                ));
                return false;
            }
        }

        cdp_lifecycle::warmup_after_relaunch().await;
        true
    }

    pub(super) async fn connect_relaunched_cdp<M: Mcp + ?Sized>(
        &mut self,
        app_name: &str,
        port: u16,
        mcp: &M,
    ) -> Option<u16> {
        self.emit_event(AgentEvent::SubAction {
            tool_name: "cdp_connect".to_string(),
            summary: format!("Auto: connecting CDP on port {}", port),
        })
        .await;
        match crate::cdp_lifecycle::connect_with_retries(mcp, port).await {
            Ok(()) => {
                tracing::info!(app = app_name, port, "state-spine: CDP connected");
                self.emit_event(AgentEvent::SubAction {
                    tool_name: "cdp_connect".to_string(),
                    summary: format!("Auto: CDP connected on port {} (ok)", port),
                })
                .await;
                self.on_cdp_connected(app_name, port, mcp).await;
                Some(port)
            }
            Err(last_err) => {
                warn!(
                    app = app_name,
                    port,
                    error = %last_err,
                    "state-spine: CDP connection failed after retries",
                );
                self.emit_event(AgentEvent::SubAction {
                    tool_name: "cdp_connect".to_string(),
                    summary: format!("Auto: CDP connect failed on port {}", port),
                })
                .await;
                self.record_cdp_connect_failure(format!(
                    "cdp_connect failed after retries on port {port} for {app_name}: {last_err}",
                ));
                None
            }
        }
    }

    /// Post-tool hook: after a successful `launch_app` / `focus_window`,
    /// auto-connect CDP and refresh the MCP tool-cache so observation
    /// gates see the newly-surfaced CDP tools. Also keeps `cdp_state`
    /// in lock-step with `quit_app`. Port of the legacy
    /// `AgentRunner::maybe_cdp_connect`.
    pub(super) async fn maybe_cdp_connect<M: Mcp + ?Sized>(
        &mut self,
        tool_name: &str,
        arguments: &Value,
        result_text: &str,
        mcp: &M,
    ) {
        if tool_name != "launch_app" && tool_name != "focus_window" {
            // Keep CDP state and `world_model.focused_app` in lock-step
            // with the underlying process.
            if tool_name == "quit_app"
                && let Some(name) = arguments.get("app_name").and_then(Value::as_str)
            {
                self.cdp_state.mark_app_quit(name);
                if self
                    .world_model
                    .focused_app
                    .as_ref()
                    .is_some_and(|f| f.value.name == name)
                {
                    self.world_model.focused_app = None;
                    // Status was bound to the now-departed focused app;
                    // a quit_app result should not leave its failure
                    // reason hanging on the next turn's render.
                    self.world_model.cdp_connect_status = None;
                }
            }
            return;
        }
        let Some((app_name, kind_hint)) =
            Self::resolve_cdp_target(arguments, result_text, mcp).await
        else {
            return;
        };
        // Stash the kind BEFORE the CDP decision so the record is
        // present even when CDP is skipped (Native short-circuit).
        if let Some(kind) = kind_hint.as_deref() {
            self.record_app_kind(&app_name, kind);
        }
        // Mirror the focus into `world_model.focused_app` so the per-turn
        // `<tools_in_scope>` filter sees the current focus state across
        // turns. Runs whether or not CDP attaches — the AX / pre-connect
        // arms key on focused-app kind alone.
        {
            use crate::agent::world_model::{AppKind, FocusedApp, Fresh, FreshnessSource};
            let kind = match kind_hint.as_deref() {
                Some("ElectronApp") | Some("electron_app") => AppKind::ElectronApp,
                Some("ChromeBrowser") | Some("chrome_browser") => AppKind::ChromeBrowser,
                _ => AppKind::Native,
            };
            let pid = serde_json::from_str::<Value>(result_text)
                .ok()
                .and_then(|v| v.get("pid").and_then(Value::as_i64))
                .map(|p| p as i32)
                .unwrap_or(0);
            self.world_model.focused_app = Some(Fresh {
                value: FocusedApp {
                    name: app_name.clone(),
                    kind,
                    pid,
                },
                written_at: self.step_index,
                source: FreshnessSource::DirectObservation,
                ttl_steps: None,
            });
        }
        // Clear any prior auto-connect status before the next attempt.
        // Without this, a successful focus to a different app would keep
        // showing the previous app's failure reason; auto_connect_cdp
        // either succeeds (cleared in `on_cdp_connected`), fails (set
        // in this attempt's terminal path), or short-circuits (no new
        // status is appropriate, so the old one must not survive).
        self.world_model.cdp_connect_status = None;
        if let Some(cdp_port) = self
            .auto_connect_cdp(&app_name, kind_hint.as_deref(), mcp)
            .await
        {
            self.finalize_cdp_connected(&app_name, cdp_port, mcp).await;
        }
    }

    /// Post-`auto_connect_cdp` housekeeping shared between the
    /// `maybe_cdp_connect` post-tool path and the dispatch-site
    /// `CdpAttachable` synthetic-skip path. Emits the `CdpConnected`
    /// event so the UI surfaces the connect, then refreshes the
    /// client-side tool cache so observation gates (notably
    /// `fetch_cdp_page_summary`'s `cdp_summarize_page` lookup) see the
    /// CDP tools the server surfaced post-connect.
    pub(super) async fn finalize_cdp_connected<M: Mcp + ?Sized>(
        &self,
        app_name: &str,
        cdp_port: u16,
        mcp: &M,
    ) {
        self.emit_event(AgentEvent::CdpConnected {
            app_name: app_name.to_string(),
            port: cdp_port,
        })
        .await;
        if let Err(e) = mcp.refresh_server_tool_list().await {
            warn!(
                error = %e,
                "state-spine: post-CDP-connect tool-cache refresh failed",
            );
        }
    }
}
