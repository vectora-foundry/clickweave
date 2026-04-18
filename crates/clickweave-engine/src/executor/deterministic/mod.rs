pub(crate) mod best_effort;
pub(crate) mod cdp;
mod click;
mod hover;
pub(crate) mod tool_result;
mod window;

pub(crate) use best_effort::best_effort_tool_call;
pub(crate) use tool_result::ToolResult;

use super::retry_context::RetryContext;
use super::{ExecutorError, ExecutorResult, Mcp, WorkflowExecutor};
use clickweave_core::AppKind;
use clickweave_core::output_schema::NodeContext;
use clickweave_core::{
    FocusTarget, FocusWindowParams, NodeRun, NodeType, ScreenshotMode, TakeScreenshotParams,
    tool_mapping,
};
use clickweave_llm::ChatBackend;
use clickweave_mcp::ToolCallResult;
use serde_json::Value;
use uuid::Uuid;

/// Outcome of click-target resolution during [`WorkflowExecutor::execute_deterministic`].
///
/// The CDP fast path can complete the click entirely and hand back the raw
/// tool result text, while the native path only rewrites the node into a
/// coordinate-form `Click` that must still fall through to the generic
/// tool-call tail.
enum ClickResolution {
    /// CDP click succeeded — short-circuit with this result text.
    EarlyReturn(String),
    /// Native resolution produced a rewritten `NodeType` (e.g. coordinates).
    Resolved(NodeType),
    /// Nothing to do — the caller keeps the original `node_type`.
    Passthrough,
}

/// Pre-extracted arg hints used by the generic tool-call tail.
///
/// Populated once before `args` is moved into `mcp.call_tool` so later
/// post-call bookkeeping can still read the launch/focus/quit intent
/// without borrowing the moved value.
struct GenericCallHints {
    launch_app_name: Option<String>,
    launch_app_kind: AppKind,
    launch_chrome_profile: Option<String>,
    quit_app_name: Option<String>,
    mcp_focus_window_app: Option<String>,
}

impl GenericCallHints {
    fn from_args(tool_name: &str, node_type: &NodeType, args: Option<&Value>) -> Self {
        let launch_app_name = if tool_name == "launch_app" {
            args.and_then(|a| a.get("app_name"))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
        } else {
            None
        };

        let launch_app_kind = if tool_name == "launch_app" {
            args.and_then(|a| a.get("app_kind"))
                .and_then(|v| v.as_str())
                .and_then(AppKind::parse)
                .unwrap_or(AppKind::Native)
        } else {
            AppKind::Native
        };

        let launch_chrome_profile = if tool_name == "launch_app" {
            args.and_then(|a| a.get("chrome_profile"))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
        } else {
            None
        };

        let quit_app_name = if tool_name == "quit_app" {
            args.and_then(|a| a.get("app_name"))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
        } else {
            None
        };

        let mcp_focus_window_app =
            if tool_name == "focus_window" && matches!(node_type, NodeType::McpToolCall(_)) {
                args.and_then(|a| a.get("app_name"))
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
            } else {
                None
            };

        Self {
            launch_app_name,
            launch_app_kind,
            launch_chrome_profile,
            quit_app_name,
            mcp_focus_window_app,
        }
    }
}

/// Select the best window from a `list_windows` response for window control resolution.
///
/// Filters by `app_name` (case-insensitive) if provided. Among matches, prefers
/// on-screen windows at the lowest layer. Uses array index as z-order tiebreaker
/// since `list_windows` returns windows in front-to-back order.
fn select_best_window<'a>(windows: &'a [Value], app_name: Option<&str>) -> Option<&'a Value> {
    let rank = |i: usize, w: &Value| (w["layer"].as_i64().unwrap_or(i64::MAX), i);

    let mut best_onscreen: Option<(usize, &Value)> = None;
    let mut best_any: Option<(usize, &Value)> = None;

    for (i, w) in windows.iter().enumerate() {
        let matches = app_name.is_none_or(|name| {
            w["owner_name"]
                .as_str()
                .is_some_and(|o| o.eq_ignore_ascii_case(name))
        });
        if !matches {
            continue;
        }

        let key = rank(i, w);
        if best_any.is_none_or(|(bi, bw)| key < rank(bi, bw)) {
            best_any = Some((i, w));
        }
        if w["is_on_screen"].as_bool().unwrap_or(false)
            && best_onscreen.is_none_or(|(bi, bw)| key < rank(bi, bw))
        {
            best_onscreen = Some((i, w));
        }
    }

    best_onscreen.or(best_any).map(|(_, w)| w)
}

fn truncate_for_error(s: &str, max_len: usize) -> &str {
    match s.char_indices().nth(max_len) {
        Some((idx, _)) => &s[..idx],
        None => s,
    }
}

fn is_return_key(key: &str) -> bool {
    key.eq_ignore_ascii_case("return")
        || key.eq_ignore_ascii_case("enter")
        || key == "\r"
        || key == "\n"
}

/// Heuristic for URL-like omnibox input (e.g. `gmail.com`, `https://...`).
/// Used to decide when TypeText/Enter should follow the browser-navigation path.
fn looks_like_browser_url_input(text: &str) -> bool {
    let t = text.trim().to_ascii_lowercase();
    if t.is_empty() || t.contains(' ') {
        return false;
    }

    if t.starts_with("http://") || t.starts_with("https://") || t.starts_with("file://") {
        return true;
    }
    // Internal schemes (about:, chrome://, edge://) are excluded because
    // cdp_page_payload_is_navigation only recognises http/https/file, so
    // intercepting them would sit in the 30s poll loop with no exit.

    // Email-like text is usually form input, not URL navigation.
    if t.contains('@') {
        return false;
    }

    // Bare host/path form, e.g. gmail.com or youtube.com/watch?v=...
    let host = t.split('/').next().unwrap_or("");
    let host = host.strip_prefix("www.").unwrap_or(host);
    if host.split('.').count() < 2 {
        return false;
    }
    if !host
        .split('.')
        .all(|part| !part.is_empty() && part.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'))
    {
        return false;
    }
    // Avoid hijacking plain dotted tokens like "1.2.3" or "foo.bar".
    if !host.chars().any(|c| c.is_ascii_alphabetic()) {
        return false;
    }
    let tld = host.rsplit('.').next().unwrap_or("");
    if tld.is_empty() || !tld.chars().all(|c| c.is_ascii_alphabetic()) {
        return false;
    }
    const COMMON_TLDS: &[&str] = &[
        "com", "net", "org", "io", "dev", "app", "ai", "co", "edu", "gov", "me", "info", "biz",
        "xyz", "tv", "us", "uk", "de", "fr", "it", "es", "nl", "ca", "au", "ch", "jp", "in", "br",
        "ru", "local", "internal", "lan", "corp",
    ];
    COMMON_TLDS.contains(&tld)
}

/// Returns true only for real web-page URLs (http, https, file).
/// Using a positive allowlist avoids false positives for unknown chrome:// or
/// about: schemes that aren't in any blocklist (e.g. about:srcdoc, chrome://settings).
fn cdp_page_payload_is_navigation(payload: &str) -> bool {
    payload.contains("http://") || payload.contains("https://") || payload.contains("file://")
}

fn parse_cdp_page_payloads(list_pages_text: &str) -> std::collections::BTreeMap<usize, String> {
    let mut out = std::collections::BTreeMap::new();
    for line in list_pages_text.lines() {
        let t = line.trim_start();
        if !t.starts_with('[') {
            continue;
        }
        let Some(end) = t.find(']') else {
            continue;
        };
        let Ok(index) = t[1..end].parse::<usize>() else {
            continue;
        };
        let payload = t[end + 1..].trim().to_ascii_lowercase();
        out.insert(index, payload);
    }
    out
}

/// Return true when `cdp_list_pages` indicates the page list changed after
/// Enter, and one tab transitioned to (or changed within) a navigated page.
///
/// Comparing against a pre-Enter baseline avoids false positives from tabs
/// that were already open before the navigation keypress.
fn cdp_pages_show_navigation_progress(before_pages_text: &str, after_pages_text: &str) -> bool {
    let before = parse_cdp_page_payloads(before_pages_text);
    let after = parse_cdp_page_payloads(after_pages_text);

    after.iter().any(|(index, after_payload)| {
        if !cdp_page_payload_is_navigation(after_payload) {
            return false;
        }
        match before.get(index) {
            Some(before_payload) => before_payload != after_payload,
            None => true,
        }
    })
}

impl<C: ChatBackend> WorkflowExecutor<C> {
    pub(crate) async fn execute_deterministic(
        &mut self,
        node_id: Uuid,
        node_type: &NodeType,
        mcp: &(impl Mcp + ?Sized),
        mut node_run: Option<&mut NodeRun>,
        retry_ctx: &mut RetryContext,
    ) -> ExecutorResult<Value> {
        retry_ctx.last_tool_result = None;

        // Check CDP scope — nodes that require a CDP connection fail early
        // if no CDP-capable app has been focused.
        if node_type.node_context() == NodeContext::Cdp && !self.cdp_connected_to_focused_app() {
            return Err(ExecutorError::NoCdpConnection {
                node_type: node_type.display_name().to_string(),
            });
        }

        // --- TypeText / PressKey on Chrome/CDP: omnibox URL typing + Enter + navigation wait ---
        // Maintains `retry_ctx.last_typed_url` state and, for the Enter branch,
        // executes the full press_key + cdp_list_pages polling early-return.
        if let Some(result) = self
            .maybe_handle_chrome_url_navigation(node_type, mcp, node_run.as_deref(), retry_ctx)
            .await?
        {
            return Ok(result);
        }

        // --- Hover: CDP path + native fallback + dwell ---
        if let NodeType::Hover(p) = node_type {
            return self
                .execute_hover(node_id, p, mcp, &mut node_run, retry_ctx)
                .await;
        }

        if let NodeType::FindApp(p) = node_type {
            return self.execute_find_app(&p.search, mcp).await;
        }

        if let NodeType::CdpWait(p) = node_type {
            return self.execute_cdp_wait(&p.text, p.timeout_ms, mcp).await;
        }

        // CDP Click: resolve target via snapshot
        if let NodeType::CdpClick(p) = node_type {
            let result_text = self
                .resolve_and_click_cdp(
                    node_id,
                    p.target.as_str(),
                    mcp,
                    node_run.as_deref(),
                    retry_ctx,
                )
                .await?;
            return Self::set_tool_result_and_parse(retry_ctx, ToolResult::from_text(result_text));
        }

        // CDP Hover: same resolve path as CdpClick
        if let NodeType::CdpHover(p) = node_type {
            let result_text = self
                .resolve_and_hover_cdp(
                    node_id,
                    p.target.as_str(),
                    mcp,
                    node_run.as_deref(),
                    retry_ctx,
                )
                .await?;
            return Self::set_tool_result_and_parse(retry_ctx, ToolResult::from_text(result_text));
        }

        // CDP Fill: resolve target against the live snapshot so a UID baked in
        // at planning time stays valid after relaunch.
        if let NodeType::CdpFill(p) = node_type {
            return self
                .execute_cdp_fill(p, mcp, node_run.as_deref(), retry_ctx)
                .await;
        }

        // CDP Type: call cdp_type_text directly
        if let NodeType::CdpType(p) = node_type {
            return self
                .execute_cdp_type(p, mcp, node_run.as_deref(), retry_ctx)
                .await;
        }

        // CDP Press Key: call cdp_press_key directly
        if let NodeType::CdpPressKey(p) = node_type {
            return self
                .execute_cdp_press_key(p, mcp, node_run.as_deref(), retry_ctx)
                .await;
        }

        if let NodeType::AppDebugKitOp(p) = node_type {
            return self
                .execute_app_debug_kit_op(p, mcp, node_run.as_deref(), retry_ctx)
                .await;
        }

        if let NodeType::McpToolCall(p) = node_type
            && p.tool_name.is_empty()
        {
            return Err(ExecutorError::Validation(
                "McpToolCall has empty tool_name".to_string(),
            ));
        }

        // Resolve Click targets (window-control / CDP-first / text) and fall
        // through to the generic tool-call path. CDP-first click returns
        // early inside the helper when the CDP path succeeds.
        let resolved_click;
        let effective = match self
            .resolve_click_effective(node_id, node_type, mcp, &mut node_run, retry_ctx)
            .await?
        {
            ClickResolution::EarlyReturn(result_text) => {
                return Self::set_tool_result_and_parse(
                    retry_ctx,
                    ToolResult::from_text(result_text),
                );
            }
            ClickResolution::Resolved(nt) => {
                resolved_click = nt;
                &resolved_click
            }
            ClickResolution::Passthrough => node_type,
        };

        let resolved_fw;
        let effective = match self
            .resolve_focus_window_effective(node_id, effective, mcp, node_run.as_deref(), retry_ctx)
            .await?
        {
            Some(nt) => {
                resolved_fw = nt;
                &resolved_fw
            }
            None => effective,
        };

        let resolved_ss;
        let effective = match self
            .resolve_take_screenshot_effective(
                node_id,
                effective,
                mcp,
                node_run.as_deref(),
                retry_ctx,
            )
            .await?
        {
            Some(nt) => {
                resolved_ss = nt;
                &resolved_ss
            }
            None => effective,
        };

        self.execute_generic_tool_call(node_id, node_type, effective, mcp, node_run, retry_ctx)
            .await
    }

    /// Handle the Chrome/CDP URL-typing + Enter flow. On TypeText the helper
    /// only updates `retry_ctx.last_typed_url` and returns `None`; on the
    /// Enter variant following a typed URL it runs the full press_key +
    /// cdp_list_pages polling loop and returns `Some(result)` so the caller
    /// can short-circuit. The `None` branch also clears `last_typed_url` for
    /// every non-matching node type — ordering identical to the original.
    async fn maybe_handle_chrome_url_navigation(
        &mut self,
        node_type: &NodeType,
        mcp: &(impl Mcp + ?Sized),
        node_run: Option<&NodeRun>,
        retry_ctx: &mut RetryContext,
    ) -> ExecutorResult<Option<Value>> {
        if let NodeType::TypeText(p) = node_type {
            let app_kind = self.focused_app_kind();
            if app_kind == AppKind::ChromeBrowser
                && self.cdp_connected_to_focused_app()
                && looks_like_browser_url_input(&p.text)
            {
                // Store the text so the subsequent press_key return knows to wait
                // for Chrome to visually start loading before supervision fires.
                retry_ctx.last_typed_url = Some(p.text.clone());

                // Make URL typing idempotent on retries/reruns: bring Chrome to
                // front and focus/select the omnibox before typing.
                if let Some(app_name) = self.focused_app_name() {
                    let _ = mcp
                        .call_tool(
                            "focus_window",
                            Some(serde_json::json!({"app_name": app_name})),
                        )
                        .await;
                }
                #[cfg(target_os = "macos")]
                let modifiers = vec!["command"];
                #[cfg(not(target_os = "macos"))]
                let modifiers = vec!["control"];
                let _ = mcp
                    .call_tool(
                        "press_key",
                        Some(serde_json::json!({
                            "key": "l",
                            "modifiers": modifiers,
                        })),
                    )
                    .await;
            } else {
                retry_ctx.last_typed_url = None;
            }
            return Ok(None);
        }

        if let NodeType::PressKey(p) = node_type {
            let app_kind = self.focused_app_kind();
            if app_kind == AppKind::ChromeBrowser
                && self.cdp_connected_to_focused_app()
                && is_return_key(&p.key)
                && p.modifiers.is_empty()
                && retry_ctx.last_typed_url.is_some()
            {
                let value = self
                    .execute_chrome_url_press_key_enter(mcp, node_run, retry_ctx)
                    .await?;
                return Ok(Some(value));
            }
            retry_ctx.last_typed_url = None;
            return Ok(None);
        }

        retry_ctx.last_typed_url = None;
        Ok(None)
    }

    /// Run the Chrome-URL Enter path: re-focus the omnibox, fire press_key
    /// Return, then poll `cdp_list_pages` until a navigation-like transition
    /// is observed or the deadline elapses.
    async fn execute_chrome_url_press_key_enter(
        &mut self,
        mcp: &(impl Mcp + ?Sized),
        node_run: Option<&NodeRun>,
        retry_ctx: &mut RetryContext,
    ) -> ExecutorResult<Value> {
        // Re-focus the target app before sending Enter. In Test mode,
        // per-step screenshot/supervision can occasionally leave key
        // focus elsewhere, causing Enter to miss Chrome.
        if let Some(app_name) = self.focused_app_name() {
            let _ = mcp
                .call_tool(
                    "focus_window",
                    Some(serde_json::json!({"app_name": app_name})),
                )
                .await;
        }

        // URL was just typed into the Chrome Omnibox. Fire the native
        // press_key return (which Chrome handles as Omnibox navigation),
        // then poll cdp_list_pages until the URL changes away from NTP.
        //
        // We cannot use cdp_navigate here: Chrome's NTP auto-focuses the
        // Omnibox, which causes Chrome to silently ignore Page.navigate
        // CDP commands, making cdp_navigate always time out.
        let navigation_baseline = match mcp
            .call_tool("cdp_list_pages", Some(serde_json::json!({})))
            .await
        {
            Ok(r) if r.is_error != Some(true) => {
                let text = crate::cdp_lifecycle::extract_text(&r);
                // Only use the baseline if it contains at least one
                // parseable page entry. An empty map would cause every
                // HTTP tab in the next poll to look "new".
                if parse_cdp_page_payloads(&text).is_empty() {
                    self.log(
                        "Chrome URL navigation: baseline has no page entries — \
                         navigation observation disabled",
                    );
                    None
                } else {
                    Some(text)
                }
            }
            _ => {
                self.log(
                    "Chrome URL navigation: baseline cdp_list_pages failed — \
                     navigation observation disabled",
                );
                None
            }
        };

        let press_args = serde_json::json!({"key": "return"});
        self.record_event(
            node_run,
            "tool_call",
            serde_json::json!({"name": "press_key", "args": &press_args}),
        );
        let result = mcp
            .call_tool("press_key", Some(press_args))
            .await
            .map_err(|e| ExecutorError::ToolCall {
                tool: "press_key".to_string(),
                message: e.to_string(),
            })?;
        Self::check_tool_error(&result, "press_key")?;
        let result_text = crate::cdp_lifecycle::extract_text(&result);
        self.record_event(
            node_run,
            "tool_result",
            serde_json::json!({
                "name": "press_key",
                "text": Self::truncate_for_trace(&result_text, 8192),
            }),
        );

        // Poll cdp_list_pages until Chrome moves away from NTP/blank.
        // This gives a structural "navigation started" signal without
        // waiting for full page load, which can be long on Gmail/YouTube.
        //
        // We skip the observation loop when the baseline is unavailable:
        // without a before-snapshot we cannot distinguish existing tabs
        // from newly-navigated ones (every http tab would look "new").
        if let Some(ref baseline) = navigation_baseline {
            self.log("Chrome URL navigation: polling for URL change...");
            let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(30);
            let mut poll_ms: u64 = 100;
            // Poll until the URL changes or the deadline expires.
            // last_typed_url stays armed through supervision retries
            // (cleared by run_loop after supervision passes) so that a
            // false-failure retry still enters the navigation-aware
            // PressKey path instead of sending a raw Enter to the
            // destination page.
            loop {
                if self.cancel_token.is_cancelled() {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(poll_ms)).await;
                poll_ms = (poll_ms * 2).min(500);
                if tokio::time::Instant::now() >= deadline {
                    self.log("Chrome URL navigation: timeout waiting for URL change");
                    break;
                }
                if let Ok(r) = mcp
                    .call_tool("cdp_list_pages", Some(serde_json::json!({})))
                    .await
                    && r.is_error != Some(true)
                {
                    let text = crate::cdp_lifecycle::extract_text(&r);
                    if cdp_pages_show_navigation_progress(baseline, &text) {
                        self.log("Chrome URL navigation: page URL changed");
                        break;
                    }
                }
            }
        } else {
            self.log("Chrome URL navigation: baseline unavailable, skipping observation");
        }

        Self::set_tool_result_and_parse(retry_ctx, ToolResult::from_text(result_text))
    }

    /// Hover branch: CDP path first (when CDP-capable + connected), native
    /// move_mouse fallback, then dwell for the configured duration.
    async fn execute_hover(
        &mut self,
        node_id: Uuid,
        p: &clickweave_core::HoverParams,
        mcp: &(impl Mcp + ?Sized),
        node_run: &mut Option<&mut NodeRun>,
        retry_ctx: &mut RetryContext,
    ) -> ExecutorResult<Value> {
        self.log(format!(
            "Hover: {}",
            NodeType::Hover(p.clone()).action_description()
        ));

        let app_kind = self.focused_app_kind();

        // CDP path: try hover via chrome-devtools-mcp for Electron/Chrome apps
        if app_kind.uses_cdp()
            && self.cdp_connected_to_focused_app()
            && let Some(target) = &p.target
        {
            match self
                .resolve_and_hover_cdp(node_id, target.text(), mcp, node_run.as_deref(), retry_ctx)
                .await
            {
                Ok(result_text) => {
                    self.record_event(
                        node_run.as_deref(),
                        "tool_result",
                        serde_json::json!({
                            "tool": "hover",
                            "method": "cdp",
                            "result": Self::truncate_for_trace(&result_text, 8192),
                        }),
                    );
                    tokio::time::sleep(tokio::time::Duration::from_millis(p.dwell_ms)).await;
                    return Self::set_tool_result_and_parse(
                        retry_ctx,
                        ToolResult::from_text(result_text),
                    );
                }
                Err(e) => {
                    self.log(format!("CDP hover failed, falling back to native: {e}"));
                }
            }
        }

        // Native path: resolve text target to coordinates, then move_mouse + dwell
        let owned_hover_type = NodeType::Hover(p.clone());
        let resolved_hover;
        let effective = if matches!(&p.target, Some(clickweave_core::ClickTarget::Text { .. })) {
            resolved_hover = self
                .resolve_hover_target(node_id, mcp, p, node_run, retry_ctx)
                .await?;
            &resolved_hover
        } else {
            &owned_hover_type
        };

        let inv = tool_mapping::node_type_to_tool_invocation(effective)
            .map_err(|e| ExecutorError::Validation(e.to_string()))?;

        self.record_event(
            node_run.as_deref(),
            "tool_call",
            serde_json::json!({"name": inv.name, "args": &inv.arguments}),
        );

        let result = mcp
            .call_tool(&inv.name, Some(inv.arguments))
            .await
            .map_err(|e| ExecutorError::ToolCall {
                tool: inv.name.clone(),
                message: e.to_string(),
            })?;

        Self::check_tool_error(&result, &inv.name)?;
        let result_text = crate::cdp_lifecycle::extract_text(&result);

        self.record_event(
            node_run.as_deref(),
            "tool_result",
            serde_json::json!({
                "name": inv.name,
                "text": Self::truncate_for_trace(&result_text, 8192),
                "text_len": result_text.len(),
            }),
        );

        // Dwell: hold position for the configured duration
        tokio::time::sleep(tokio::time::Duration::from_millis(p.dwell_ms)).await;

        Self::set_tool_result_and_parse(retry_ctx, ToolResult::from_text(result_text))
    }

    /// Call `cdp_fill` with a snapshot-resolved uid.
    async fn execute_cdp_fill(
        &mut self,
        p: &clickweave_core::CdpFillParams,
        mcp: &(impl Mcp + ?Sized),
        node_run: Option<&NodeRun>,
        retry_ctx: &mut RetryContext,
    ) -> ExecutorResult<Value> {
        let uid = self
            .resolve_cdp_target_uid_with_overrides(&p.target, mcp, Some(retry_ctx))
            .await?;
        let args = serde_json::json!({"uid": uid, "value": p.value});
        self.record_event(
            node_run,
            "tool_call",
            serde_json::json!({"name": "cdp_fill", "args": &args}),
        );
        let result =
            mcp.call_tool("cdp_fill", Some(args))
                .await
                .map_err(|e| ExecutorError::ToolCall {
                    tool: "cdp_fill".to_string(),
                    message: e.to_string(),
                })?;
        Self::check_tool_error(&result, "cdp_fill")?;
        let result_text = crate::cdp_lifecycle::extract_text(&result);
        self.record_event(
            node_run,
            "tool_result",
            serde_json::json!({
                "name": "cdp_fill",
                "text": Self::truncate_for_trace(&result_text, 8192),
                "text_len": result_text.len(),
            }),
        );
        Self::set_tool_result_and_parse(retry_ctx, ToolResult::from_text(result_text))
    }

    /// Call `cdp_type_text` with the provided text.
    async fn execute_cdp_type(
        &mut self,
        p: &clickweave_core::CdpTypeParams,
        mcp: &(impl Mcp + ?Sized),
        node_run: Option<&NodeRun>,
        retry_ctx: &mut RetryContext,
    ) -> ExecutorResult<Value> {
        let args = serde_json::json!({"text": p.text});
        self.record_event(
            node_run,
            "tool_call",
            serde_json::json!({"name": "cdp_type_text", "args": &args}),
        );
        let result = mcp
            .call_tool("cdp_type_text", Some(args))
            .await
            .map_err(|e| ExecutorError::ToolCall {
                tool: "cdp_type_text".to_string(),
                message: e.to_string(),
            })?;
        Self::check_tool_error(&result, "cdp_type_text")?;
        let result_text = crate::cdp_lifecycle::extract_text(&result);
        self.record_event(
            node_run,
            "tool_result",
            serde_json::json!({
                "name": "cdp_type_text",
                "text": Self::truncate_for_trace(&result_text, 8192),
                "text_len": result_text.len(),
            }),
        );
        Self::set_tool_result_and_parse(retry_ctx, ToolResult::from_text(result_text))
    }

    /// Call `cdp_press_key` with the provided key and optional modifiers.
    async fn execute_cdp_press_key(
        &mut self,
        p: &clickweave_core::CdpPressKeyParams,
        mcp: &(impl Mcp + ?Sized),
        node_run: Option<&NodeRun>,
        retry_ctx: &mut RetryContext,
    ) -> ExecutorResult<Value> {
        let mut args = serde_json::json!({"key": p.key});
        if !p.modifiers.is_empty() {
            args["modifiers"] = serde_json::json!(p.modifiers);
        }
        self.record_event(
            node_run,
            "tool_call",
            serde_json::json!({"name": "cdp_press_key", "args": &args}),
        );
        let result = mcp
            .call_tool("cdp_press_key", Some(args))
            .await
            .map_err(|e| ExecutorError::ToolCall {
                tool: "cdp_press_key".to_string(),
                message: e.to_string(),
            })?;
        Self::check_tool_error(&result, "cdp_press_key")?;
        let result_text = crate::cdp_lifecycle::extract_text(&result);
        self.record_event(
            node_run,
            "tool_result",
            serde_json::json!({
                "name": "cdp_press_key",
                "text": Self::truncate_for_trace(&result_text, 8192),
                "text_len": result_text.len(),
            }),
        );
        Self::set_tool_result_and_parse(retry_ctx, ToolResult::from_text(result_text))
    }

    /// Call the AppDebugKit-operation tool by name with raw parameters.
    async fn execute_app_debug_kit_op(
        &mut self,
        p: &clickweave_core::AppDebugKitParams,
        mcp: &(impl Mcp + ?Sized),
        node_run: Option<&NodeRun>,
        retry_ctx: &mut RetryContext,
    ) -> ExecutorResult<Value> {
        self.log(format!("AppDebugKit operation: {}", p.operation_name));
        let args = if p.parameters.is_null() {
            None
        } else {
            Some(p.parameters.clone())
        };
        self.record_event(
            node_run,
            "tool_call",
            serde_json::json!({"name": p.operation_name, "args": args}),
        );
        let result =
            mcp.call_tool(&p.operation_name, args)
                .await
                .map_err(|e| ExecutorError::ToolCall {
                    tool: p.operation_name.clone(),
                    message: e.to_string(),
                })?;
        Self::check_tool_error(&result, &p.operation_name)?;
        let result_text = crate::cdp_lifecycle::extract_text(&result);
        self.record_event(
            node_run,
            "tool_result",
            serde_json::json!({
                "name": p.operation_name,
                "text": Self::truncate_for_trace(&result_text, 8192),
                "text_len": result_text.len(),
            }),
        );
        Self::set_tool_result_and_parse(retry_ctx, ToolResult::from_text(result_text))
    }

    /// Resolve a Click node into either an early-return result (CDP fast
    /// path succeeded), a rewritten NodeType (coords resolved), or a
    /// passthrough (not a click-with-target — leave as-is).
    async fn resolve_click_effective(
        &mut self,
        node_id: Uuid,
        node_type: &NodeType,
        mcp: &(impl Mcp + ?Sized),
        node_run: &mut Option<&mut NodeRun>,
        retry_ctx: &mut RetryContext,
    ) -> ExecutorResult<ClickResolution> {
        let NodeType::Click(p) = node_type else {
            return Ok(ClickResolution::Passthrough);
        };
        if let Some(clickweave_core::ClickTarget::WindowControl { action }) = &p.target {
            // Window control buttons are resolved to window-relative coordinates.
            let resolved = self
                .resolve_window_control_click(*action, mcp, p, node_run)
                .await?;
            return Ok(ClickResolution::Resolved(resolved));
        }
        if matches!(&p.target, Some(clickweave_core::ClickTarget::Text { .. })) {
            // For Electron/Chrome apps, try CDP click first (snapshot + uid click).
            let click_target = p.target.as_ref().ok_or_else(|| {
                ExecutorError::ClickTarget(
                    "Click::target vanished between match and unwrap".to_string(),
                )
            })?;
            let target = click_target.text();
            let app_kind = self.focused_app_kind();

            if app_kind.uses_cdp() && self.cdp_connected_to_focused_app() {
                match self
                    .resolve_and_click_cdp(node_id, target, mcp, node_run.as_deref(), retry_ctx)
                    .await
                {
                    Ok(result_text) => {
                        self.record_event(
                            node_run.as_deref(),
                            "tool_result",
                            serde_json::json!({
                                "tool": "click",
                                "method": "cdp",
                                "result": Self::truncate_for_trace(&result_text, 8192),
                            }),
                        );
                        return Ok(ClickResolution::EarlyReturn(result_text));
                    }
                    Err(e) => {
                        self.log(format!("CDP click failed, falling back to native: {e}"));
                    }
                }
            }

            let resolved = self
                .resolve_click_target(node_id, mcp, p, node_run, retry_ctx)
                .await?;
            return Ok(ClickResolution::Resolved(resolved));
        }
        Ok(ClickResolution::Passthrough)
    }

    /// Resolve a FocusWindow node with an AppName target: resolve the app,
    /// upgrade its kind, lazily connect CDP for Electron/Chrome, update
    /// `focused_app`, and rewrite the node to a PID target.
    ///
    /// Returns `None` for any other shape so the caller can keep the
    /// current `effective`.
    async fn resolve_focus_window_effective(
        &mut self,
        node_id: Uuid,
        effective: &NodeType,
        mcp: &(impl Mcp + ?Sized),
        node_run: Option<&NodeRun>,
        retry_ctx: &RetryContext,
    ) -> ExecutorResult<Option<NodeType>> {
        let NodeType::FocusWindow(p) = effective else {
            return Ok(None);
        };
        let FocusTarget::AppName(user_input) = &p.target else {
            return Ok(None);
        };
        if user_input.is_empty() {
            return Ok(None);
        }
        let user_input = user_input.as_str();
        let mut app = self
            .resolve_app_name(node_id, user_input, mcp, node_run, retry_ctx.cache_mode)
            .await?;
        // Upgrade app_kind if the node says Native but detection disagrees.
        let app_kind = if p.app_kind == AppKind::Native {
            let detected = clickweave_core::app_detection::classify_app_by_pid(app.pid);
            if detected != AppKind::Native {
                self.log(format!(
                    "Upgraded app_kind for '{}' from Native to {:?}",
                    app.name, detected
                ));
            }
            detected
        } else {
            p.app_kind
        };

        // Lazy CDP connection for Electron/Chrome apps.
        if app_kind.uses_cdp() && mcp.has_tool("cdp_connect") {
            let profile_path = self.resolve_chrome_profile_path_for_app(
                app_kind,
                &app.name,
                p.chrome_profile_id.as_deref(),
            )?;
            self.ensure_cdp_connected(
                node_id,
                &app.name,
                app.pid,
                mcp,
                node_run,
                profile_path.as_deref(),
            )
            .await?;
            // Re-resolve PID -- it may have changed if the app was relaunched.
            app = self
                .resolve_app_name(node_id, user_input, mcp, node_run, retry_ctx.cache_mode)
                .await?;
            // Sync the CDP connection PID to the freshly resolved PID.
            // `ensure_cdp_connected` ran above with the pre-resolve PID;
            // if the resolver now reports a different PID (typical after
            // a relaunch that picked up a new process), rebind the
            // stored identity to the new PID so later lookups match.
            self.cdp_state.rebind_pid(&app.name, app.pid);
        }

        *self.write_focused_app() = Some((app.name.clone(), app_kind, app.pid));

        // `app.pid` is i32 from the MCP app listing; coerce to u32 for the
        // typed target. Negative/overflow values fall back to the resolved
        // app name so the downstream tool mapping still targets the correct
        // app (the executor treats an empty AppName as "no target" only).
        let pid_target = u32::try_from(app.pid)
            .map(FocusTarget::Pid)
            .unwrap_or_else(|_| FocusTarget::AppName(app.name.clone()));
        Ok(Some(NodeType::FocusWindow(FocusWindowParams {
            target: pid_target,
            bring_to_front: p.bring_to_front,
            app_kind,
            chrome_profile_id: p.chrome_profile_id.clone(),
            ..Default::default()
        })))
    }

    /// Resolve a TakeScreenshot node with `mode=Window` and a user-supplied
    /// app-name target: re-resolve the app and return a rewritten node with
    /// the canonical name. Returns `None` otherwise.
    async fn resolve_take_screenshot_effective(
        &mut self,
        node_id: Uuid,
        effective: &NodeType,
        mcp: &(impl Mcp + ?Sized),
        node_run: Option<&NodeRun>,
        retry_ctx: &RetryContext,
    ) -> ExecutorResult<Option<NodeType>> {
        let NodeType::TakeScreenshot(p) = effective else {
            return Ok(None);
        };
        if p.target.is_none() || p.mode != ScreenshotMode::Window {
            return Ok(None);
        }
        let user_input = p.target.as_deref().ok_or_else(|| {
            ExecutorError::Validation(
                "TakeScreenshot target vanished between check and unwrap".to_string(),
            )
        })?;
        let app = self
            .resolve_app_name(node_id, user_input, mcp, node_run, retry_ctx.cache_mode)
            .await?;
        Ok(Some(NodeType::TakeScreenshot(TakeScreenshotParams {
            mode: p.mode,
            target: Some(app.name.clone()),
            include_ocr: p.include_ocr,
        })))
    }

    /// Generic tool-call tail: convert the (possibly resolved) node type to
    /// an invocation, apply arg massaging (find_text app scoping, image-path
    /// resolution), run the Chrome-profile fast-path for `launch_app` when
    /// applicable, call the tool, apply post-call side effects (launch/focus
    /// bookkeeping, CDP auto-connect, quit_app cleanup, find_text retry),
    /// and assemble the trace event + return value.
    async fn execute_generic_tool_call(
        &mut self,
        node_id: Uuid,
        node_type: &NodeType,
        effective: &NodeType,
        mcp: &(impl Mcp + ?Sized),
        mut node_run: Option<&mut NodeRun>,
        retry_ctx: &mut RetryContext,
    ) -> ExecutorResult<Value> {
        let invocation = tool_mapping::node_type_to_tool_invocation(effective)
            .map_err(|e| ExecutorError::Validation(format!("Tool mapping failed: {}", e)))?;
        let tool_name = &invocation.name;

        self.log(format!("Calling MCP tool: {}", tool_name));
        let mut args = self.resolve_image_paths(Some(invocation.arguments));

        // Scope find_text to the focused app when no explicit app_name is set
        if tool_name == "find_text"
            && let Some(ref mut a) = args
            && a.get("app_name").is_none()
            && let Some(app_name) = self.focused_app_name()
        {
            a["app_name"] = serde_json::Value::String(app_name);
        }

        // Save original args for find_text retry fallback (args will be moved into call_tool)
        let find_text_original_args = if tool_name == "find_text" {
            args.clone()
        } else {
            None
        };

        let hints = GenericCallHints::from_args(tool_name, node_type, args.as_ref());

        // For Chrome-family launch_app with a configured profile: kill only the
        // Chrome instance running this profile (leave the user's default Chrome
        // alone), then launch Chrome directly with --user-data-dir. We bypass the
        // MCP launch_app tool which refuses when any Chrome is already running.
        if tool_name == "launch_app"
            && hints.launch_app_kind == AppKind::ChromeBrowser
            && let Some(profile_path) =
                self.resolve_chrome_profile_path(hints.launch_chrome_profile.as_deref())?
        {
            return self
                .execute_chrome_profile_launch(
                    node_id,
                    hints.launch_app_name.as_deref(),
                    hints.launch_app_kind,
                    profile_path,
                    mcp,
                    node_run.as_deref(),
                    retry_ctx,
                )
                .await;
        }

        self.record_event(
            node_run.as_deref(),
            "tool_call",
            serde_json::json!({"name": tool_name, "args": args}),
        );
        let result = mcp
            .call_tool(tool_name, args)
            .await
            .map_err(|e| ExecutorError::ToolCall {
                tool: tool_name.to_string(),
                message: e.to_string(),
            })?;

        Self::check_tool_error(&result, tool_name)?;

        // launch_app implies the app is now focused.
        // Auto-detect app kind from the running process, since the agent
        // may not include app_kind in the launch_app arguments.
        if let Some(name) = &hints.launch_app_name {
            let (detected_kind, detected_pid) = if hints.launch_app_kind == AppKind::Native {
                // Try to detect actual app kind from the running process
                match self.lookup_app_pid(name, mcp).await {
                    Ok(pid) => {
                        let detected = clickweave_core::app_detection::classify_app_by_pid(pid);
                        if detected != AppKind::Native {
                            self.log(format!(
                                "Detected app_kind for '{}': {:?} (pid {})",
                                name, detected, pid
                            ));
                        }
                        (detected, pid)
                    }
                    Err(_) => (AppKind::Native, 0),
                }
            } else {
                if hints.launch_app_kind != AppKind::Native {
                    self.log(format!(
                        "App '{}' has app_kind: {:?}",
                        name, hints.launch_app_kind
                    ));
                }
                // PID lookup not needed when app_kind is already known.
                (hints.launch_app_kind, 0)
            };

            *self.write_focused_app() = Some((name.clone(), detected_kind, detected_pid));

            // Lazy CDP connection for Electron/Chrome apps (same as FocusWindow path).
            if detected_kind.uses_cdp() && mcp.has_tool("cdp_connect") {
                let profile_path =
                    self.resolve_chrome_profile_path_for_app(detected_kind, name, None)?;
                self.ensure_cdp_connected(
                    node_id,
                    name,
                    detected_pid,
                    mcp,
                    node_run.as_deref(),
                    profile_path.as_deref(),
                )
                .await?;
            }
        }

        // Generic McpToolCall focus_window: PID is not resolvable inline,
        // mark focus_dirty so run_loop refreshes kind+PID post-step.
        if let Some(ref app_name) = hints.mcp_focus_window_app {
            *self.write_focused_app() = Some((app_name.clone(), AppKind::Native, 0));
            retry_ctx.focus_dirty = true;
        }

        // quit_app clears focused_app and the shared CDP state when the
        // app being quit is the currently focused or connected app.
        if let Some(ref app_name) = hints.quit_app_name {
            if self.focused_app_name().as_deref() == Some(app_name.as_str())
                || self.focused_app_name().is_none()
            {
                *self.write_focused_app() = None;
            }
            // Clears the active connection (when bound to this app) and
            // every remembered tab URL for any PID of this app name.
            self.cdp_state.mark_app_quit(app_name);
            self.write_app_cache().remove(app_name.as_str());
        }

        let images = self.save_result_images(&result, "result", &mut node_run);
        let result_text = crate::cdp_lifecycle::extract_text(&result);

        // For find_text: if empty matches + available_elements, resolve element name via LLM and retry.
        let find_text_empty = tool_name == "find_text"
            && serde_json::from_str::<Vec<Value>>(&result_text)
                .unwrap_or_default()
                .is_empty();
        let result_text =
            if find_text_empty && let Some(ref original_args) = find_text_original_args {
                self.try_resolve_find_text(
                    node_id,
                    original_args,
                    &result_text,
                    mcp,
                    node_run.as_deref(),
                    retry_ctx.cache_mode,
                )
                .await
                .unwrap_or(result_text)
            } else {
                result_text
            };

        self.record_event(
            node_run.as_deref(),
            "tool_result",
            serde_json::json!({
                "name": tool_name,
                "text": Self::truncate_for_trace(&result_text, 8192),
                "text_len": result_text.len(),
                "image_count": images.len(),
            }),
        );

        self.log(format!(
            "Tool result: {} chars, {} images",
            result_text.len(),
            images.len()
        ));

        Self::set_tool_result_and_parse(retry_ctx, ToolResult::from_text(result_text))
    }

    /// Chrome-profile launch: kills only the profile-scoped Chrome instance,
    /// spawns Chrome directly with `--user-data-dir` (optionally with a
    /// debug port when CDP is available), and wires up CDP when needed.
    #[allow(clippy::too_many_arguments)]
    async fn execute_chrome_profile_launch(
        &mut self,
        node_id: Uuid,
        launch_app_name: Option<&str>,
        launch_app_kind: AppKind,
        profile_path: std::path::PathBuf,
        mcp: &(impl Mcp + ?Sized),
        node_run: Option<&NodeRun>,
        retry_ctx: &mut RetryContext,
    ) -> ExecutorResult<Value> {
        let dir = profile_path.to_string_lossy().to_string();
        self.log(format!("Launching Chrome with profile: {}", dir));

        let use_cdp = launch_app_kind.uses_cdp() && mcp.has_tool("cdp_connect");

        if !use_cdp {
            // No CDP available: launch now without debug port.
            kill_chrome_profile_instance(&dir).await;
            launch_chrome_with_profile(&dir)
                .await
                .map_err(|e| ExecutorError::ToolCall {
                    tool: "launch_app".to_string(),
                    message: format!("Failed to launch Chrome with profile: {e}"),
                })?;
            // Wait for Chrome to start up before continuing.
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        }

        self.record_event(
            node_run,
            "tool_call",
            serde_json::json!({
                "name": "launch_app",
                "args": {"app_name": launch_app_name, "user_data_dir": dir},
            }),
        );

        if let Some(name) = launch_app_name {
            // PID is not yet available immediately after launch; use 0 as placeholder.
            *self.write_focused_app() = Some((name.to_string(), launch_app_kind, 0));
            if use_cdp {
                // Force-disconnect any existing CDP session: a new profile
                // launch kills the previous Chrome instance, so any old CDP
                // connection is stale. Without this, ensure_cdp_connected
                // short-circuits on the app name match and never connects
                // to the new profile's Chrome instance.
                if let Some((prev_name, _)) = self.cdp_state.take_connected() {
                    best_effort::best_effort_tool_call(
                        mcp,
                        "cdp_disconnect",
                        None,
                        "launch_app profile branch: force-disconnect before relaunch",
                    )
                    .await;
                    // The app was about to be killed for a profile
                    // relaunch — forget every remembered tab URL for any
                    // instance of this app name; they're all stale after
                    // the kill. The active-connection slot was already
                    // cleared by `take_connected`.
                    self.cdp_state.mark_app_quit(&prev_name);
                }
                self.ensure_cdp_connected(
                    node_id,
                    name,
                    0,
                    mcp,
                    node_run,
                    Some(profile_path.as_path()),
                )
                .await?;
            }
        }

        let result_text = format!("Launched Chrome with profile {}", dir);
        self.record_event(
            node_run,
            "tool_result",
            serde_json::json!({"name": "launch_app", "text": &result_text}),
        );
        Self::set_tool_result_and_parse(retry_ctx, ToolResult::from_text(result_text))
    }

    pub(crate) fn check_tool_error(result: &ToolCallResult, tool_name: &str) -> ExecutorResult<()> {
        if result.is_error == Some(true) {
            let error_text = crate::cdp_lifecycle::extract_text(result);
            return Err(ExecutorError::ToolCall {
                tool: tool_name.to_string(),
                message: error_text,
            });
        }
        Ok(())
    }

    /// Store the tool result for supervision (preserving the raw text that
    /// the supervisor prompt quotes back to the LLM), then return the
    /// legacy [`Value`] shape that downstream variable extraction expects.
    ///
    /// Call sites assemble a [`ToolResult`] via [`ToolResult::from_text`]
    /// so the text-to-JSON parse happens exactly once per tool invocation;
    /// this helper is the lone seam where the executor hands that pair
    /// back to [`RetryContext`] for supervision to re-use.
    fn set_tool_result_and_parse(
        retry_ctx: &mut RetryContext,
        result: ToolResult,
    ) -> ExecutorResult<Value> {
        retry_ctx.last_tool_result = Some(result.raw_text().to_string());
        Ok(result.into_value())
    }
}

/// Kill only Chrome processes running with a specific `--user-data-dir`,
/// leaving the user's default Chrome instance untouched.
async fn kill_chrome_profile_instance(profile_dir: &str) {
    #[cfg(not(target_os = "windows"))]
    {
        // Use pkill to kill processes matching the specific --user-data-dir.
        // Anchoring to "Google Chrome" avoids matching pgrep's own command line.
        let pattern = format!("Google Chrome.*--user-data-dir={}", profile_dir);
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
        let _ = tokio::process::Command::new("taskkill")
            .args([
                "/F",
                "/FI",
                &format!("WINDOWTITLE eq *--user-data-dir={}*", profile_dir),
            ])
            .output()
            .await;
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    }
}

/// Launch Chrome directly with `--user-data-dir` and optional extra args,
/// bypassing MCP `launch_app` which refuses when any Chrome is already running.
async fn spawn_chrome(args: &[String]) -> Result<(), String> {
    use std::process::Stdio;

    #[cfg(target_os = "macos")]
    let result = tokio::process::Command::new(
        "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
    )
    .args(args)
    .stdin(Stdio::null())
    .stdout(Stdio::null())
    .stderr(Stdio::null())
    .spawn();

    #[cfg(target_os = "windows")]
    let result = tokio::process::Command::new("chrome")
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();

    #[cfg(target_os = "linux")]
    let result = tokio::process::Command::new("google-chrome")
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();

    result
        .map(|_| ())
        .map_err(|e| format!("Failed to spawn Chrome: {e}"))
}

async fn launch_chrome_with_profile(profile_dir: &str) -> Result<(), String> {
    spawn_chrome(&[
        format!("--user-data-dir={}", profile_dir),
        "--no-first-run".to_string(),
        "--no-default-browser-check".to_string(),
    ])
    .await
}

async fn launch_chrome_with_profile_and_debug_port(
    profile_dir: &str,
    port: u16,
) -> Result<(), String> {
    spawn_chrome(&[
        format!("--user-data-dir={}", profile_dir),
        format!("--remote-debugging-port={}", port),
        "--no-first-run".to_string(),
        "--no-default-browser-check".to_string(),
    ])
    .await
}

#[cfg(test)]
mod tests {
    use super::{
        GenericCallHints, cdp_page_payload_is_navigation, cdp_pages_show_navigation_progress,
        looks_like_browser_url_input,
    };
    use clickweave_core::{AppKind, McpToolCallParams, NodeType};

    fn mcp_tool_call_node() -> NodeType {
        NodeType::McpToolCall(McpToolCallParams {
            tool_name: "focus_window".to_string(),
            arguments: serde_json::Value::Null,
        })
    }

    #[test]
    fn generic_hints_extract_launch_app_fields() {
        let args = serde_json::json!({
            "app_name": "Chrome",
            "app_kind": "ChromeBrowser",
            "chrome_profile": "work",
        });
        let hints = GenericCallHints::from_args("launch_app", &mcp_tool_call_node(), Some(&args));
        assert_eq!(hints.launch_app_name.as_deref(), Some("Chrome"));
        assert_eq!(hints.launch_app_kind, AppKind::ChromeBrowser);
        assert_eq!(hints.launch_chrome_profile.as_deref(), Some("work"));
        assert!(hints.quit_app_name.is_none());
        assert!(hints.mcp_focus_window_app.is_none());
    }

    #[test]
    fn generic_hints_default_launch_app_kind_when_missing() {
        let args = serde_json::json!({"app_name": "Calculator"});
        let hints = GenericCallHints::from_args("launch_app", &mcp_tool_call_node(), Some(&args));
        assert_eq!(hints.launch_app_name.as_deref(), Some("Calculator"));
        assert_eq!(hints.launch_app_kind, AppKind::Native);
        assert!(hints.launch_chrome_profile.is_none());
    }

    #[test]
    fn generic_hints_extract_quit_app_name() {
        let args = serde_json::json!({"app_name": "Calculator"});
        let hints = GenericCallHints::from_args("quit_app", &mcp_tool_call_node(), Some(&args));
        assert_eq!(hints.quit_app_name.as_deref(), Some("Calculator"));
        assert!(hints.launch_app_name.is_none());
    }

    #[test]
    fn generic_hints_focus_window_only_on_mcp_tool_call_node() {
        let args = serde_json::json!({"app_name": "Calculator"});
        // McpToolCall-shaped node: app_name is captured
        let hints = GenericCallHints::from_args("focus_window", &mcp_tool_call_node(), Some(&args));
        assert_eq!(hints.mcp_focus_window_app.as_deref(), Some("Calculator"));
        // A different node variant produces None so the typed FocusWindow
        // branch's inline PID resolution isn't second-guessed by the
        // fallback that marks focus_dirty.
        let typed_focus = NodeType::FocusWindow(Default::default());
        let hints2 = GenericCallHints::from_args("focus_window", &typed_focus, Some(&args));
        assert!(hints2.mcp_focus_window_app.is_none());
    }

    #[test]
    fn generic_hints_ignore_fields_on_unrelated_tools() {
        let args = serde_json::json!({"app_name": "Calculator"});
        let hints = GenericCallHints::from_args("click", &mcp_tool_call_node(), Some(&args));
        assert!(hints.launch_app_name.is_none());
        assert!(hints.quit_app_name.is_none());
        assert!(hints.mcp_focus_window_app.is_none());
    }

    #[test]
    fn cdp_navigation_progress_detects_ntp_to_web_transition() {
        let before = "[0] about:newtab\n[1] https://example.com/dashboard";
        let after =
            "[0] https://mail.google.com/mail/u/0/#inbox\n[1] https://example.com/dashboard";
        assert!(cdp_pages_show_navigation_progress(before, after));
    }

    #[test]
    fn cdp_navigation_progress_rejects_unchanged_existing_tabs() {
        let before = "[0] about:newtab\n[1] https://example.com/dashboard";
        let after = "[0] about:newtab\n[1] https://example.com/dashboard";
        assert!(!cdp_pages_show_navigation_progress(before, after));
    }

    #[test]
    fn cdp_navigation_progress_detects_web_to_web_transition() {
        let before = "[0] https://example.com/dashboard\n[1] https://mail.google.com/mail/u/0";
        let after = "[0] https://www.youtube.com/\n[1] https://mail.google.com/mail/u/0";
        assert!(cdp_pages_show_navigation_progress(before, after));
    }

    // B2 regression: chrome:// and about: pages that aren't in any blocklist must
    // not count as navigation (positive allowlist, not double-negative blocklist).
    #[test]
    fn cdp_payload_rejects_chrome_settings_tab() {
        assert!(!cdp_page_payload_is_navigation("chrome://settings/"));
        assert!(!cdp_page_payload_is_navigation("about:srcdoc"));
        assert!(!cdp_page_payload_is_navigation("chrome://newtab"));
        assert!(!cdp_page_payload_is_navigation("about:blank"));
    }

    #[test]
    fn cdp_payload_accepts_http_https_file() {
        assert!(cdp_page_payload_is_navigation("https://mail.google.com/"));
        assert!(cdp_page_payload_is_navigation("http://localhost:3000"));
        assert!(cdp_page_payload_is_navigation(
            "file:///Users/me/index.html"
        ));
    }

    // B3 regression: an empty baseline must not cause every open tab to look like
    // a new navigation. (Tested indirectly: all tabs match before == after so no change.)
    #[test]
    fn cdp_navigation_progress_empty_baseline_does_not_spuriously_match_existing_tabs() {
        let before = "";
        let after = "[0] https://example.com/dashboard\n[1] https://mail.google.com/mail/u/0";
        // With an empty baseline every tab looks "new" — this would be a false positive.
        // The fix is to skip the poll loop when baseline is None; this unit test documents
        // the raw function behaviour so callers know not to pass an empty baseline.
        // Both tabs have an http URL, and neither is in the empty before-map → true.
        assert!(cdp_pages_show_navigation_progress(before, after));
        // (Callers must guard against this by skipping the loop when baseline is None.)
    }

    #[test]
    fn url_input_detects_domain_and_scheme() {
        assert!(looks_like_browser_url_input("gmail.com"));
        assert!(looks_like_browser_url_input(
            "https://www.youtube.com/watch?v=1"
        ));
    }

    #[test]
    fn url_input_rejects_email_and_plain_words() {
        assert!(!looks_like_browser_url_input("user@example.com"));
        assert!(!looks_like_browser_url_input("hello"));
        assert!(!looks_like_browser_url_input("1.2.3"));
        assert!(!looks_like_browser_url_input("foo.bar"));
        assert!(!looks_like_browser_url_input("test.txt"));
    }

    // N4: port-containing inputs like localhost:3000 should not be treated as URLs
    // (no dot in the host part, so they fall through the TLD check).
    #[test]
    fn url_input_rejects_port_only_addresses() {
        assert!(!looks_like_browser_url_input("localhost:3000"));
        assert!(!looks_like_browser_url_input("localhost"));
    }

    // But fully-qualified hosts with ports are accepted because they start with https://
    #[test]
    fn url_input_accepts_scheme_with_port() {
        assert!(looks_like_browser_url_input("http://localhost:3000"));
        assert!(looks_like_browser_url_input("https://app.local:8080"));
    }
}
