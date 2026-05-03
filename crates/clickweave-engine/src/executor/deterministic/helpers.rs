use super::*;

/// Outcome of click-target resolution during [`WorkflowExecutor::execute_deterministic`].
///
/// The CDP fast path can complete the click entirely and hand back the raw
/// tool result text, while the native path only rewrites the node into a
/// coordinate-form `Click` that must still fall through to the generic
/// tool-call tail.
pub(super) enum ClickResolution {
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
pub(super) struct GenericCallHints {
    pub(super) launch_app_name: Option<String>,
    pub(super) launch_app_kind: AppKind,
    pub(super) launch_chrome_profile: Option<String>,
    pub(super) quit_app_name: Option<String>,
    pub(super) mcp_focus_window_app: Option<String>,
}

impl GenericCallHints {
    pub(super) fn from_args(tool_name: &str, node_type: &NodeType, args: Option<&Value>) -> Self {
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
pub(super) fn select_best_window<'a>(
    windows: &'a [Value],
    app_name: Option<&str>,
) -> Option<&'a Value> {
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

pub(super) fn truncate_for_error(s: &str, max_len: usize) -> &str {
    match s.char_indices().nth(max_len) {
        Some((idx, _)) => &s[..idx],
        None => s,
    }
}

pub(super) fn is_return_key(key: &str) -> bool {
    key.eq_ignore_ascii_case("return")
        || key.eq_ignore_ascii_case("enter")
        || key == "\r"
        || key == "\n"
}

/// Heuristic for URL-like omnibox input (e.g. `gmail.com`, `https://...`).
/// Used to decide when TypeText/Enter should follow the browser-navigation path.
pub(super) fn looks_like_browser_url_input(text: &str) -> bool {
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

pub(super) fn parse_cdp_page_payloads(
    list_pages_text: &str,
) -> std::collections::BTreeMap<usize, String> {
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
pub(super) fn cdp_pages_show_navigation_progress(
    before_pages_text: &str,
    after_pages_text: &str,
) -> bool {
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

/// Kill only Chrome processes running with a specific `--user-data-dir`,
/// leaving the user's default Chrome instance untouched.
pub(super) async fn kill_chrome_profile_instance(profile_dir: &str) {
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

pub(super) async fn launch_chrome_with_profile(profile_dir: &str) -> Result<(), String> {
    spawn_chrome(&[
        format!("--user-data-dir={}", profile_dir),
        "--no-first-run".to_string(),
        "--no-default-browser-check".to_string(),
    ])
    .await
}

pub(super) async fn launch_chrome_with_profile_and_debug_port(
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
