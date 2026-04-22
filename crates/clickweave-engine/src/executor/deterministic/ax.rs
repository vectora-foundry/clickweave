//! Executor dispatch for macOS AX tools (`ax_click`, `ax_set_value`,
//! `ax_select`) from `native-devtools-mcp` v0.9.0+.
//!
//! **The hard invariant:** every AX dispatch is preceded by a fresh
//! `take_ax_snapshot` in the same tool sequence. The server tags every
//! snapshot with a generation suffix (`a42g3`), and dispatching with a uid
//! from a prior generation fails with `snapshot_expired`. The helpers here
//! snapshot, resolve the node's [`AxTarget`] descriptor to a fresh uid, and
//! dispatch — with one automatic retry when the server reports
//! `snapshot_expired` mid-flight.

use super::super::retry_context::RetryContext;
use super::super::{ExecutorError, ExecutorResult, Mcp, WorkflowExecutor};
use super::tool_result::ToolResult;
use clickweave_core::{AxTarget, NodeRun, TraceEventKind};
use clickweave_llm::ChatBackend;
use serde_json::Value;
use uuid::Uuid;

/// Maximum retry attempts when the server reports `snapshot_expired` — one
/// retry covers the window between our snapshot and the dispatch that a
/// concurrent focus change could close; anything more is almost certainly a
/// descriptor that no longer matches anything in the tree.
const AX_DISPATCH_MAX_ATTEMPTS: u32 = 2;

/// One line of a parsed `take_ax_snapshot` result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AxSnapshotEntry {
    pub uid: String,
    pub role: String,
    pub name: Option<String>,
    pub depth: u32,
    /// Role+name of the nearest ancestor with a name — useful as a
    /// tie-breaker when many rows share the same (role, name) under
    /// different parents (e.g. sidebar sections).
    pub parent_name: Option<String>,
}

impl<C: ChatBackend> WorkflowExecutor<C> {
    /// Resolve an [`AxTarget`] to a fresh uid by taking a new AX snapshot.
    /// A `Descriptor` is matched by role + name (+ parent_name if set); a
    /// `ResolvedUid` is looked up verbatim and fails cleanly if absent.
    #[cfg(test)]
    pub(in crate::executor) async fn resolve_ax_target_uid(
        &self,
        target: &AxTarget,
        mcp: &(impl Mcp + ?Sized),
    ) -> ExecutorResult<String> {
        let snapshot_text = self.take_ax_snapshot(mcp).await?;
        self.resolve_ax_target_uid_from_snapshot(target, &snapshot_text)
    }

    /// Variant that takes a pre-captured snapshot. Splitting the lookup from
    /// the MCP call keeps [`resolve_and_ax_dispatch`] able to reuse the same
    /// snapshot for the first attempt and request a fresh one on retry.
    fn resolve_ax_target_uid_from_snapshot(
        &self,
        target: &AxTarget,
        snapshot_text: &str,
    ) -> ExecutorResult<String> {
        let entries = parse_ax_snapshot(snapshot_text);
        match target {
            AxTarget::ResolvedUid(uid) => {
                if uid.trim().is_empty() {
                    return Err(ExecutorError::Validation(
                        "AX target is empty; expected a uid or descriptor".to_string(),
                    ));
                }
                if entries.iter().any(|e| e.uid == *uid) {
                    Ok(uid.clone())
                } else {
                    Err(ExecutorError::AxNotFound {
                        target: uid.clone(),
                    })
                }
            }
            AxTarget::Descriptor {
                role,
                name,
                parent_name,
            } => {
                // Role normalization: descriptors may carry the raw macOS AX
                // role (`AXButton`) from walkthrough capture, while the
                // server's `take_ax_snapshot` emits CDP-style roles
                // (`button`). Compare normalized forms so either producer
                // matches.
                let want_role = normalize_ax_role(role);
                let matched = entries.iter().find(|e| {
                    normalize_ax_role(&e.role) == want_role
                        && e.name.as_deref().unwrap_or("") == name.as_str()
                        && match parent_name {
                            Some(pn) => e.parent_name.as_deref() == Some(pn.as_str()),
                            None => true,
                        }
                });
                matched
                    .map(|e| e.uid.clone())
                    .ok_or_else(|| ExecutorError::AxNotFound {
                        target: format!(
                            "{}{}{}",
                            role,
                            if name.is_empty() { "" } else { ":" },
                            name
                        ),
                    })
            }
        }
    }

    /// Take a fresh AX snapshot and return the raw text payload. Wraps MCP
    /// transport errors and server-side tool errors into
    /// [`ExecutorError::AxSnapshotFailed`].
    async fn take_ax_snapshot(&self, mcp: &(impl Mcp + ?Sized)) -> ExecutorResult<String> {
        self.log("AX: taking snapshot".to_string());
        let result = mcp
            .call_tool("take_ax_snapshot", Some(serde_json::json!({})))
            .await
            .map_err(|e| {
                ExecutorError::AxSnapshotFailed(format!("take_ax_snapshot failed: {e}"))
            })?;
        if result.is_error == Some(true) {
            let text = crate::cdp_lifecycle::extract_text(&result);
            return Err(ExecutorError::AxSnapshotFailed(format!(
                "take_ax_snapshot error: {text}"
            )));
        }
        Ok(crate::cdp_lifecycle::extract_text(&result))
    }

    /// Resolve the target, call the AX dispatch tool, retry once on
    /// `snapshot_expired`. Returns the dispatch tool's raw result text on
    /// success (JSON payload: `{ ok: true, dispatched_via, bbox }`).
    ///
    /// `tool_name` is one of `"ax_click"` / `"ax_set_value"` / `"ax_select"`;
    /// `value` is supplied only for `ax_set_value`.
    pub(in crate::executor) async fn resolve_and_ax_dispatch(
        &mut self,
        tool_name: &'static str,
        _node_id: Uuid,
        target: &AxTarget,
        value: Option<&str>,
        mcp: &(impl Mcp + ?Sized),
        node_run: Option<&NodeRun>,
    ) -> ExecutorResult<String> {
        // Typed trace event kind — computed once; the retry loop reuses it so
        // we don't re-classify the same tool name on every pass.
        let event_kind = match tool_name {
            "ax_click" => TraceEventKind::AxClick,
            "ax_set_value" => TraceEventKind::AxSetValue,
            "ax_select" => TraceEventKind::AxSelect,
            _ => TraceEventKind::Unknown,
        };

        let mut snapshot_text = self.take_ax_snapshot(mcp).await?;

        for attempt in 0..AX_DISPATCH_MAX_ATTEMPTS {
            if attempt > 0 {
                // Retry: force a fresh snapshot so the server's generation
                // counter moves in lock-step with ours.
                self.log(format!(
                    "AX: {} retry (snapshot_expired) for target '{}'",
                    tool_name,
                    target.as_str()
                ));
                snapshot_text = self.take_ax_snapshot(mcp).await?;
            }

            let uid = self.resolve_ax_target_uid_from_snapshot(target, &snapshot_text)?;
            let args = build_dispatch_args(&uid, value);

            self.log(format!(
                "AX: {} uid='{}' (target='{}')",
                tool_name,
                uid,
                target.as_str()
            ));
            let result = mcp.call_tool(tool_name, Some(args)).await.map_err(|e| {
                ExecutorError::ToolCall {
                    tool: tool_name.to_string(),
                    message: e.to_string(),
                }
            })?;
            let result_text = crate::cdp_lifecycle::extract_text(&result);

            if result.is_error == Some(true) {
                let (code, message, fallback) = parse_ax_error(&result_text);
                if code.as_deref() == Some("snapshot_expired")
                    && attempt + 1 < AX_DISPATCH_MAX_ATTEMPTS
                {
                    continue;
                }
                return Err(ExecutorError::AxDispatch {
                    tool: tool_name.to_string(),
                    code: code.unwrap_or_else(|| "unknown".to_string()),
                    message: message.unwrap_or_else(|| result_text.clone()),
                    fallback,
                });
            }

            self.record_event(
                node_run,
                event_kind,
                serde_json::json!({ "target": target.as_str(), "uid": uid }),
            );
            return Ok(result_text);
        }

        // Unreachable: the loop either returns Ok, a non-snapshot_expired
        // Err, or continues. Belt-and-braces in case the constants change.
        Err(ExecutorError::AxDispatch {
            tool: tool_name.to_string(),
            code: "snapshot_expired".to_string(),
            message: "exceeded retry budget".to_string(),
            fallback: None,
        })
    }

    /// Short helper wrappers that read like the CDP equivalents.
    pub(in crate::executor) async fn resolve_and_ax_click(
        &mut self,
        node_id: Uuid,
        target: &AxTarget,
        mcp: &(impl Mcp + ?Sized),
        node_run: Option<&NodeRun>,
        retry_ctx: &mut RetryContext,
    ) -> ExecutorResult<Value> {
        let text = self
            .resolve_and_ax_dispatch("ax_click", node_id, target, None, mcp, node_run)
            .await?;
        Self::set_tool_result_and_parse(retry_ctx, ToolResult::from_text(text))
    }

    pub(in crate::executor) async fn resolve_and_ax_set_value(
        &mut self,
        node_id: Uuid,
        target: &AxTarget,
        value: &str,
        mcp: &(impl Mcp + ?Sized),
        node_run: Option<&NodeRun>,
        retry_ctx: &mut RetryContext,
    ) -> ExecutorResult<Value> {
        let text = self
            .resolve_and_ax_dispatch("ax_set_value", node_id, target, Some(value), mcp, node_run)
            .await?;
        Self::set_tool_result_and_parse(retry_ctx, ToolResult::from_text(text))
    }

    pub(in crate::executor) async fn resolve_and_ax_select(
        &mut self,
        node_id: Uuid,
        target: &AxTarget,
        mcp: &(impl Mcp + ?Sized),
        node_run: Option<&NodeRun>,
        retry_ctx: &mut RetryContext,
    ) -> ExecutorResult<Value> {
        let text = self
            .resolve_and_ax_dispatch("ax_select", node_id, target, None, mcp, node_run)
            .await?;
        Self::set_tool_result_and_parse(retry_ctx, ToolResult::from_text(text))
    }
}

/// Build the JSON args for an AX dispatch call. `value` is included only for
/// `ax_set_value`; `ax_click` and `ax_select` omit it.
fn build_dispatch_args(uid: &str, value: Option<&str>) -> Value {
    match value {
        Some(v) => serde_json::json!({ "uid": uid, "value": v }),
        None => serde_json::json!({ "uid": uid }),
    }
}

/// Extract the typed error shape from an AX dispatch error payload:
/// `{"error": {"code": "...", "message": "...", "fallback": {"x": 12, "y": 34} | null}}`.
/// Returns `(code, message, fallback)` with any missing field as `None`.
fn parse_ax_error(body: &str) -> (Option<String>, Option<String>, Option<(f64, f64)>) {
    let Ok(v) = serde_json::from_str::<Value>(body) else {
        return (None, None, None);
    };
    let err = &v["error"];
    let code = err["code"].as_str().map(str::to_owned);
    let message = err["message"].as_str().map(str::to_owned);
    let fallback = match (err["fallback"]["x"].as_f64(), err["fallback"]["y"].as_f64()) {
        (Some(x), Some(y)) => Some((x, y)),
        _ => None,
    };
    (code, message, fallback)
}

/// Parse a `take_ax_snapshot` text payload into one [`AxSnapshotEntry`] per
/// line. Tracks depth via leading-space count (each AX indent level is two
/// spaces) so `parent_name` can be derived by walking the ancestor stack.
pub(crate) fn parse_ax_snapshot(text: &str) -> Vec<AxSnapshotEntry> {
    let mut out = Vec::new();
    // Stack of (depth, name) pairs — the most recently seen named ancestor
    // at each shallower indent level.
    let mut ancestor_stack: Vec<(u32, String)> = Vec::new();

    for raw_line in text.lines() {
        let depth = leading_indent_depth(raw_line);
        let line = raw_line.trim_start();
        if line.is_empty() {
            continue;
        }

        let Some((uid, role, name)) = parse_snapshot_line(line) else {
            continue;
        };

        // Drop ancestors at the same depth or deeper — they're siblings or
        // cousins, not parents of this node.
        while let Some((d, _)) = ancestor_stack.last() {
            if *d >= depth {
                ancestor_stack.pop();
            } else {
                break;
            }
        }

        let parent_name = ancestor_stack.last().map(|(_, n)| n.clone());

        out.push(AxSnapshotEntry {
            uid,
            role,
            name: name.clone(),
            depth,
            parent_name,
        });

        if let Some(n) = name
            && !n.is_empty()
        {
            ancestor_stack.push((depth, n));
        }
    }
    out
}

fn leading_indent_depth(line: &str) -> u32 {
    let spaces = line.chars().take_while(|c| *c == ' ').count();
    // AX snapshot uses two spaces per depth level (see
    // `native-devtools-mcp/src/tools/ax_snapshot.rs::format_snapshot`).
    (spaces / 2) as u32
}

/// Extract `(uid, role, name)` from a single trimmed snapshot line. The MCP
/// formatter emits `uid=<uid> <role> ["<name>"] [key="val"] [focused] ...`
/// where the quoted name (if present) appears *immediately* after the role.
/// We walk the line once: find `uid=`, consume the uid and role as
/// whitespace-delimited tokens, then check whether the next token starts
/// with a bare `"` (name) or a `key="..."` attribute (no name). Returns
/// `None` if `uid=` is absent or its value is empty — treated as a non-entry
/// (blank lines, free-form text).
fn parse_snapshot_line(line: &str) -> Option<(String, String, Option<String>)> {
    let start = line.find("uid=")?;
    let after_uid = &line[start + 4..];
    let (uid, after) = split_first_token(after_uid);
    if uid.is_empty() {
        return None;
    }
    let (role, after_role) = match after {
        Some(s) => split_first_token(s.trim_start()),
        None => ("", None),
    };
    // The name is only present when the token immediately following the
    // role starts with an unprefixed quote. Attributes like `value="..."`
    // and `bbox=(...)` must not be mistaken for a name.
    let name = after_role
        .map(str::trim_start)
        .filter(|s| s.starts_with('"'))
        .and_then(parse_first_quoted);
    Some((uid.to_string(), role.to_string(), name))
}

/// Split `s` on the first whitespace. Returns the leading non-whitespace
/// token and the remainder after that whitespace (or `None` when the whole
/// string is a single token).
fn split_first_token(s: &str) -> (&str, Option<&str>) {
    match s.find(|c: char| c.is_whitespace()) {
        Some(end) => (&s[..end], Some(&s[end + 1..])),
        None => (s, None),
    }
}

/// First quoted substring in a line — caller has already verified that the
/// line begins (after any whitespace) with a bare `"`. Returns the contents
/// between the first two *unescaped* double quotes, with `\"` sequences
/// decoded to `"` and `\\` decoded to `\` so descriptors captured out-of-band
/// (e.g. from `element_at_point`) round-trip through any future snapshot
/// formatter that emits escaped quotes. The installed MCP server does not
/// escape quotes today — for a name that contains `"`, both producer and
/// consumer therefore see the same truncated string and still match — but
/// handling the escape is forward-compatible and the only way a name with
/// internal quotes can survive the snapshot wire format.
fn parse_first_quoted(line: &str) -> Option<String> {
    let start = line.find('"')?;
    let mut out = String::new();
    let mut chars = line[start + 1..].chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                // Treat `\"` as a literal `"` and `\\` as a literal `\`;
                // anything else keeps the backslash (conservative — the
                // formatter doesn't document other escapes, so preserving
                // the raw sequence loses no information).
                match chars.next() {
                    Some('"') => out.push('"'),
                    Some('\\') => out.push('\\'),
                    Some(other) => {
                        out.push('\\');
                        out.push(other);
                    }
                    None => {
                        out.push('\\');
                        return None;
                    }
                }
            }
            '"' => return Some(out),
            other => out.push(other),
        }
    }
    None
}

/// Normalize a macOS AX role (`AXButton`, `AXTextField`) to the CDP-style
/// role the server's `take_ax_snapshot` formatter emits (`button`,
/// `textbox`). Must stay in sync with
/// `native-devtools-mcp::tools::ax_snapshot::map_ax_role`. Values already in
/// lowercase / CDP form pass through unchanged (the fallback mirrors the
/// server: strip `AX` prefix and lowercase), so the function is idempotent
/// and safe to call on either producer's role string.
pub(crate) fn normalize_ax_role(role: &str) -> String {
    match role {
        "AXButton" => "button".to_string(),
        "AXStaticText" => "text".to_string(),
        "AXTextField" | "AXTextArea" => "textbox".to_string(),
        "AXCheckBox" => "checkbox".to_string(),
        "AXWebArea" => "RootWebArea".to_string(),
        "AXGroup" => "generic".to_string(),
        "AXLink" => "link".to_string(),
        "AXImage" => "img".to_string(),
        "AXList" => "list".to_string(),
        "AXHeading" => "heading".to_string(),
        "AXMenuItem" => "menuitem".to_string(),
        "AXTable" => "table".to_string(),
        "AXRow" => "row".to_string(),
        "AXCell" => "cell".to_string(),
        "AXTabGroup" => "tablist".to_string(),
        "AXComboBox" | "AXPopUpButton" => "combobox".to_string(),
        "AXScrollArea" => "scrollbar".to_string(),
        "AXToolbar" => "toolbar".to_string(),
        "AXRadioButton" => "radio".to_string(),
        "AXSlider" => "slider".to_string(),
        "AXProgressIndicator" => "progressbar".to_string(),
        other => other
            .strip_prefix("AX")
            .map(str::to_lowercase)
            .unwrap_or_else(|| other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_ax_snapshot_extracts_uid_role_name() {
        let text = "uid=a1g3 AXButton \"Submit\"\n";
        let entries = parse_ax_snapshot(text);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].uid, "a1g3");
        assert_eq!(entries[0].role, "AXButton");
        assert_eq!(entries[0].name.as_deref(), Some("Submit"));
        assert_eq!(entries[0].depth, 0);
        assert_eq!(entries[0].parent_name, None);
    }

    #[test]
    fn parse_ax_snapshot_tracks_parent_name_via_indent() {
        let text = "\
uid=a1g1 AXWindow \"Settings\"
  uid=a2g1 AXGroup
    uid=a3g1 AXButton \"OK\"
";
        let entries = parse_ax_snapshot(text);
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].parent_name, None);
        assert_eq!(entries[1].parent_name.as_deref(), Some("Settings"));
        // Intermediate AXGroup had no name → walk up to the nearest named
        // ancestor.
        assert_eq!(entries[2].parent_name.as_deref(), Some("Settings"));
    }

    #[test]
    fn parse_ax_snapshot_ignores_bbox_and_value_attrs() {
        let text = "uid=a7g2 AXTextField \"Search\" value=\"hello\" focused bbox=(10,20,30,40)\n";
        let entries = parse_ax_snapshot(text);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name.as_deref(), Some("Search"));
        assert_eq!(entries[0].role, "AXTextField");
    }

    #[test]
    fn parse_ax_snapshot_skips_blank_lines() {
        let text = "\n\nuid=a1g1 AXButton \"X\"\n\n";
        let entries = parse_ax_snapshot(text);
        assert_eq!(entries.len(), 1);
    }

    #[test]
    fn parse_ax_snapshot_handles_server_wire_format_with_lowercase_role() {
        // The live MCP server emits CDP-style lowercase roles (`button`,
        // `textbox`, `row`) from `map_ax_role`, not raw `AXButton`. Make sure
        // we parse that form correctly.
        let text = "uid=a1g3 button \"Submit\"\nuid=a2g3 row \"First\"\n";
        let entries = parse_ax_snapshot(text);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].role, "button");
        assert_eq!(entries[0].name.as_deref(), Some("Submit"));
        assert_eq!(entries[1].role, "row");
    }

    #[test]
    fn parse_ax_snapshot_unlabeled_field_does_not_lift_value_as_name() {
        // An unlabeled text field with a current value would be serialized
        // by the server as `uid=... textbox value="hello" focused` — no
        // quoted name. Treating the value as a name would make the
        // descriptor change whenever the field contents change, breaking
        // replay. Correct behavior: `name == None`.
        let text = "uid=a3g1 textbox value=\"hello\" focused\n";
        let entries = parse_ax_snapshot(text);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].role, "textbox");
        assert_eq!(entries[0].name, None);
    }

    #[test]
    fn parse_first_quoted_decodes_escaped_internal_quote() {
        // A label like `Save "As"` should round-trip through an escaped
        // wire form without being truncated at the first internal quote.
        let decoded = parse_first_quoted(r#""Save \"As\"""#);
        assert_eq!(decoded.as_deref(), Some(r#"Save "As""#));
    }

    #[test]
    fn parse_first_quoted_decodes_escaped_backslash() {
        let decoded = parse_first_quoted(r#""a\\b""#);
        assert_eq!(decoded.as_deref(), Some(r#"a\b"#));
    }

    #[test]
    fn parse_first_quoted_preserves_unknown_escapes_verbatim() {
        // The formatter doesn't document `\n` etc. — preserve the raw
        // two-char sequence rather than invent a decoding.
        let decoded = parse_first_quoted(r#""a\nb""#);
        assert_eq!(decoded.as_deref(), Some(r#"a\nb"#));
    }

    #[test]
    fn parse_first_quoted_handles_utf8_names() {
        // Multibyte chars (emoji, accented letters) must round-trip
        // correctly — byte-indexing would panic on split boundaries.
        let decoded = parse_first_quoted("\"Café 🍰\"");
        assert_eq!(decoded.as_deref(), Some("Café 🍰"));
    }

    #[test]
    fn parse_first_quoted_returns_none_when_unterminated() {
        assert_eq!(parse_first_quoted(r#""no close"#), None);
    }

    #[test]
    fn parse_ax_snapshot_bare_role_with_no_name_and_no_attrs() {
        // Truly empty row: `uid=... generic` — used for unnamed AXGroup.
        // Also should not contribute to the ancestor stack.
        let text = "\
uid=a1g1 generic
  uid=a2g1 button \"Close\"
";
        let entries = parse_ax_snapshot(text);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].role, "generic");
        assert_eq!(entries[0].name, None);
        // Unnamed parent should not become a `parent_name` for its child.
        assert_eq!(entries[1].parent_name, None);
    }

    #[test]
    fn normalize_ax_role_maps_common_ax_roles_to_cdp_form() {
        assert_eq!(normalize_ax_role("AXButton"), "button");
        assert_eq!(normalize_ax_role("AXTextField"), "textbox");
        assert_eq!(normalize_ax_role("AXTextArea"), "textbox");
        assert_eq!(normalize_ax_role("AXRow"), "row");
        assert_eq!(normalize_ax_role("AXComboBox"), "combobox");
        assert_eq!(normalize_ax_role("AXPopUpButton"), "combobox");
        // Already-CDP form passes through unchanged (idempotent).
        assert_eq!(normalize_ax_role("button"), "button");
        assert_eq!(normalize_ax_role("textbox"), "textbox");
        // Unknown role: strip `AX` and lowercase — mirrors the server.
        assert_eq!(normalize_ax_role("AXSplitGroup"), "splitgroup");
    }

    #[test]
    fn parse_ax_error_extracts_code_and_fallback() {
        let body = r#"{"error":{"code":"snapshot_expired","message":"stale","fallback":null}}"#;
        let (code, message, fallback) = parse_ax_error(body);
        assert_eq!(code.as_deref(), Some("snapshot_expired"));
        assert_eq!(message.as_deref(), Some("stale"));
        assert_eq!(fallback, None);
    }

    #[test]
    fn parse_ax_error_extracts_fallback_xy() {
        let body =
            r#"{"error":{"code":"not_dispatchable","message":"x","fallback":{"x":12.5,"y":34.0}}}"#;
        let (_, _, fallback) = parse_ax_error(body);
        assert_eq!(fallback, Some((12.5, 34.0)));
    }

    #[test]
    fn parse_ax_error_returns_none_on_non_json() {
        let (code, _, _) = parse_ax_error("not json");
        assert_eq!(code, None);
    }

    #[test]
    fn build_dispatch_args_omits_value_when_none() {
        let args = build_dispatch_args("a1g1", None);
        assert_eq!(args["uid"], "a1g1");
        assert!(args.get("value").is_none());
    }

    #[test]
    fn build_dispatch_args_includes_value_when_present() {
        let args = build_dispatch_args("a2g2", Some("hello"));
        assert_eq!(args["uid"], "a2g2");
        assert_eq!(args["value"], "hello");
    }
}
