//! Permission policy for agent tool calls.
//!
//! This module is pure (no I/O, no async) so policy decisions can be
//! exhaustively unit-tested against synthetic inputs. The agent loop
//! evaluates the policy before prompting the user for approval:
//!
//! - `Allow` → skip approval entirely (behaves like an observation tool)
//! - `Ask`   → fall through to the existing approval prompt
//! - `Deny`  → hard policy reject; the step fails without prompting
//!
//! Rule evaluation combines user-defined per-tool rules and pattern
//! rules with the MCP server's own `ToolAnnotations` hints. See
//! `evaluate` for the full precedence documentation.
//!
//! This module does not depend on MCP or any runtime state — callers
//! extract `ToolAnnotations` from whatever source they have (JSON tool
//! schema, typed Rust struct, etc.) and hand it to `evaluate`.
use serde::{Deserialize, Serialize};

/// Outcome of evaluating the permission policy against a proposed
/// tool invocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PermissionAction {
    /// Execute the tool without asking the user.
    Allow,
    /// Prompt the user for approval before executing.
    Ask,
    /// Refuse to execute the tool; no user prompt.
    Deny,
}

/// A single permission rule. `tool_pattern` is a glob matched against the
/// tool name. `args_pattern` is an optional substring (not a glob) matched
/// against the JSON-serialized arguments — rules without an args pattern
/// apply to every invocation of the matching tool.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermissionRule {
    /// Glob pattern matched against the tool name. `*` matches any
    /// sequence of characters (including empty); `?` matches a single
    /// character. All other characters match literally.
    pub tool_pattern: String,
    /// Optional substring matched against the JSON representation of the
    /// arguments. When `Some`, the rule only fires if the substring is
    /// present in the serialized arguments.
    pub args_pattern: Option<String>,
    /// The action to take when this rule matches.
    pub action: PermissionAction,
}

/// The permission policy the agent evaluates for every non-observation
/// tool call. The policy is built from user settings on the UI side and
/// threaded through the agent runner.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PermissionPolicy {
    /// Ordered list of rules. Later rules don't override earlier ones —
    /// the final action is `max(Deny, Ask, Allow)` over all matching
    /// rules. See `evaluate` for precedence details.
    pub rules: Vec<PermissionRule>,
    /// Global "allow all" override from the existing UI toggle. When
    /// true, short-circuits rule evaluation to `Allow`, except when
    /// `require_confirm_destructive` is set and the tool is destructive.
    pub allow_all: bool,
    /// When true, destructive tools (`destructive_hint == Some(true)`)
    /// always require confirmation — their resolved action is upgraded
    /// from `Allow` to `Ask`. `Deny` and pre-existing `Ask` are not
    /// affected. This guardrail applies even when `allow_all` is set.
    pub require_confirm_destructive: bool,
}

/// MCP tool annotations. All fields are optional because the MCP spec
/// permits them to be missing; treat `None` as "no hint given".
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ToolAnnotations {
    pub destructive_hint: Option<bool>,
    pub read_only_hint: Option<bool>,
    pub idempotent_hint: Option<bool>,
    pub open_world_hint: Option<bool>,
}

impl ToolAnnotations {
    /// Parse MCP annotations out of a raw tool JSON blob. The expected
    /// shapes are:
    ///
    /// - Top-level `annotations` object on the tool: `{ "annotations":
    ///   { "destructiveHint": bool, ... } }`
    /// - Nested under `function` (OpenAI-function wrapped): `{
    ///   "function": { "annotations": { ... } } }`
    ///
    /// Missing fields become `None`, non-boolean values become `None`.
    /// Returns an empty `ToolAnnotations` when no annotations block is
    /// present — the caller should treat this as "no hints".
    pub fn from_tool_json(tool: &serde_json::Value) -> Self {
        let annotations = tool
            .get("annotations")
            .or_else(|| tool.get("function").and_then(|f| f.get("annotations")));
        let Some(ann) = annotations else {
            return Self::default();
        };
        Self {
            destructive_hint: ann.get("destructiveHint").and_then(|v| v.as_bool()),
            read_only_hint: ann.get("readOnlyHint").and_then(|v| v.as_bool()),
            idempotent_hint: ann.get("idempotentHint").and_then(|v| v.as_bool()),
            open_world_hint: ann.get("openWorldHint").and_then(|v| v.as_bool()),
        }
    }
}

/// Glob match for tool-name patterns. Supports `*` (any sequence
/// including empty) and `?` (any single character). Returns false when
/// the pattern is malformed or does not match.
fn glob_match(pattern: &str, text: &str) -> bool {
    // Classic recursive glob. Works for short tool names; the pattern
    // space is tiny (rule count × tool count × a handful of metachars).
    let pattern_bytes = pattern.as_bytes();
    let text_bytes = text.as_bytes();
    glob_match_inner(pattern_bytes, 0, text_bytes, 0)
}

fn glob_match_inner(pattern: &[u8], pi: usize, text: &[u8], ti: usize) -> bool {
    if pi == pattern.len() {
        return ti == text.len();
    }
    match pattern[pi] {
        b'*' => {
            // Greedy star: try matching zero, one, two... characters.
            if glob_match_inner(pattern, pi + 1, text, ti) {
                return true;
            }
            if ti < text.len() {
                return glob_match_inner(pattern, pi, text, ti + 1);
            }
            false
        }
        b'?' => {
            if ti >= text.len() {
                return false;
            }
            glob_match_inner(pattern, pi + 1, text, ti + 1)
        }
        c => {
            if ti >= text.len() || text[ti] != c {
                return false;
            }
            glob_match_inner(pattern, pi + 1, text, ti + 1)
        }
    }
}

/// Check whether a single rule applies to the given invocation.
fn rule_matches(rule: &PermissionRule, tool_name: &str, arguments_json: &str) -> bool {
    if !glob_match(&rule.tool_pattern, tool_name) {
        return false;
    }
    if let Some(needle) = rule.args_pattern.as_deref()
        && !needle.is_empty()
        && !arguments_json.contains(needle)
    {
        return false;
    }
    true
}

/// Pick the most-restrictive action among a set of matching rules:
/// `Deny` beats `Ask` beats `Allow`. Used when multiple rules fire.
fn combine_actions(
    actions: impl IntoIterator<Item = PermissionAction>,
) -> Option<PermissionAction> {
    let mut out: Option<PermissionAction> = None;
    for action in actions {
        out = Some(match (out, action) {
            (None, a) => a,
            (Some(PermissionAction::Deny), _) | (_, PermissionAction::Deny) => {
                PermissionAction::Deny
            }
            (Some(PermissionAction::Ask), _) | (_, PermissionAction::Ask) => PermissionAction::Ask,
            _ => PermissionAction::Allow,
        });
    }
    out
}

/// Evaluate the policy for a proposed tool call.
///
/// # Precedence (most → least specific)
///
/// 1. If the policy has at least one matching rule, the resolved action is
///    `max(Deny, Ask, Allow)` over every matching rule. `Deny` always wins.
/// 2. If no rule matches and the annotations report `read_only_hint =
///    Some(true)`, default to `Allow` (read-only tools do not mutate state).
/// 3. Otherwise the default is `Ask`.
/// 4. Apply the destructive guardrail: if
///    `require_confirm_destructive && destructive_hint == Some(true)`,
///    upgrade an `Allow` action to `Ask`. `Deny` and `Ask` are untouched.
/// 5. Apply `allow_all`: if set, the action becomes `Allow` unless the
///    destructive guardrail already upgraded (or will upgrade) to `Ask`.
///    This means: the global override cannot be used to silently skip
///    destructive confirmations — operators must turn both toggles off.
///
/// The function is pure; it performs no I/O.
pub fn evaluate(
    policy: &PermissionPolicy,
    tool_name: &str,
    arguments: &serde_json::Value,
    annotations: &ToolAnnotations,
) -> PermissionAction {
    // Render arguments once for substring matching. `null` is a fine
    // serialized form; absent-args rules don't care.
    let arguments_json = serde_json::to_string(arguments).unwrap_or_else(|_| String::from("null"));

    let rule_action = combine_actions(
        policy
            .rules
            .iter()
            .filter(|r| rule_matches(r, tool_name, &arguments_json))
            .map(|r| r.action),
    );

    let base = match rule_action {
        Some(a) => a,
        None => {
            if annotations.read_only_hint == Some(true) {
                PermissionAction::Allow
            } else {
                PermissionAction::Ask
            }
        }
    };

    // Destructive guardrail. Only upgrades; never downgrades.
    let guardrail_triggers =
        policy.require_confirm_destructive && annotations.destructive_hint == Some(true);
    let after_guardrail = match (base, guardrail_triggers) {
        (PermissionAction::Allow, true) => PermissionAction::Ask,
        (other, _) => other,
    };

    // allow_all short-circuits Ask → Allow, but respects the destructive
    // guardrail (never downgrades an Ask to Allow when guardrail fires).
    if policy.allow_all {
        match after_guardrail {
            PermissionAction::Deny => PermissionAction::Deny,
            PermissionAction::Ask if guardrail_triggers => PermissionAction::Ask,
            _ => PermissionAction::Allow,
        }
    } else {
        after_guardrail
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn rule(pattern: &str, action: PermissionAction) -> PermissionRule {
        PermissionRule {
            tool_pattern: pattern.to_string(),
            args_pattern: None,
            action,
        }
    }

    fn rule_with_args(
        pattern: &str,
        args_substring: &str,
        action: PermissionAction,
    ) -> PermissionRule {
        PermissionRule {
            tool_pattern: pattern.to_string(),
            args_pattern: Some(args_substring.to_string()),
            action,
        }
    }

    // ── Glob matching ───────────────────────────────────────────────

    #[test]
    fn glob_star_matches_any_sequence() {
        assert!(glob_match("cdp_*", "cdp_click"));
        assert!(glob_match("cdp_*", "cdp_"));
        assert!(glob_match("*", "anything"));
        assert!(glob_match("*click*", "cdp_click_element"));
    }

    #[test]
    fn glob_question_mark_matches_single_char() {
        assert!(glob_match("cl?ck", "click"));
        assert!(!glob_match("cl?ck", "clck"));
        assert!(!glob_match("cl?ck", "clicks"));
    }

    #[test]
    fn glob_literal_requires_exact_match() {
        assert!(glob_match("click", "click"));
        assert!(!glob_match("click", "cdp_click"));
        assert!(!glob_match("cdp_click", "click"));
    }

    // ── No rules / annotation defaults ──────────────────────────────

    #[test]
    fn no_rules_and_no_hints_default_to_ask() {
        let policy = PermissionPolicy::default();
        let action = evaluate(
            &policy,
            "cdp_click",
            &json!({"x": 1}),
            &ToolAnnotations::default(),
        );
        assert_eq!(action, PermissionAction::Ask);
    }

    #[test]
    fn no_rules_read_only_defaults_to_allow() {
        let policy = PermissionPolicy::default();
        let annotations = ToolAnnotations {
            read_only_hint: Some(true),
            ..Default::default()
        };
        let action = evaluate(&policy, "cdp_find_elements", &json!({}), &annotations);
        assert_eq!(action, PermissionAction::Allow);
    }

    #[test]
    fn no_rules_destructive_without_guardrail_defaults_to_ask() {
        // require_confirm_destructive = false, no rules: still Ask (no
        // read-only hint → falls into Ask branch).
        let policy = PermissionPolicy::default();
        let annotations = ToolAnnotations {
            destructive_hint: Some(true),
            ..Default::default()
        };
        let action = evaluate(&policy, "delete_file", &json!({}), &annotations);
        assert_eq!(action, PermissionAction::Ask);
    }

    // ── Glob matching via rules ─────────────────────────────────────

    #[test]
    fn glob_rule_matches_family_of_tools() {
        let policy = PermissionPolicy {
            rules: vec![rule("cdp_*", PermissionAction::Allow)],
            ..Default::default()
        };
        assert_eq!(
            evaluate(
                &policy,
                "cdp_click",
                &json!({}),
                &ToolAnnotations::default()
            ),
            PermissionAction::Allow,
        );
        assert_eq!(
            evaluate(
                &policy,
                "cdp_type_text",
                &json!({}),
                &ToolAnnotations::default()
            ),
            PermissionAction::Allow,
        );
        // Non-matching tool falls through to the default (Ask).
        assert_eq!(
            evaluate(&policy, "click", &json!({}), &ToolAnnotations::default()),
            PermissionAction::Ask,
        );
    }

    #[test]
    fn star_rule_matches_every_tool() {
        let policy = PermissionPolicy {
            rules: vec![rule("*", PermissionAction::Deny)],
            ..Default::default()
        };
        assert_eq!(
            evaluate(&policy, "click", &json!({}), &ToolAnnotations::default()),
            PermissionAction::Deny,
        );
    }

    #[test]
    fn args_pattern_narrows_rule_scope() {
        // Deny any type_text that contains "password"; everything else is
        // default (Ask).
        let policy = PermissionPolicy {
            rules: vec![rule_with_args(
                "type_text",
                "password",
                PermissionAction::Deny,
            )],
            ..Default::default()
        };
        let matching = evaluate(
            &policy,
            "type_text",
            &json!({"text": "my password"}),
            &ToolAnnotations::default(),
        );
        let other = evaluate(
            &policy,
            "type_text",
            &json!({"text": "hello world"}),
            &ToolAnnotations::default(),
        );
        assert_eq!(matching, PermissionAction::Deny);
        assert_eq!(other, PermissionAction::Ask);
    }

    // ── Precedence: Deny > Ask > Allow ──────────────────────────────

    #[test]
    fn deny_beats_allow_when_both_match() {
        let policy = PermissionPolicy {
            rules: vec![
                rule("*", PermissionAction::Allow),
                rule("delete_*", PermissionAction::Deny),
            ],
            ..Default::default()
        };
        assert_eq!(
            evaluate(
                &policy,
                "delete_file",
                &json!({}),
                &ToolAnnotations::default()
            ),
            PermissionAction::Deny,
        );
    }

    #[test]
    fn ask_beats_allow_when_both_match() {
        let policy = PermissionPolicy {
            rules: vec![
                rule("*", PermissionAction::Allow),
                rule("send_*", PermissionAction::Ask),
            ],
            ..Default::default()
        };
        assert_eq!(
            evaluate(
                &policy,
                "send_mail",
                &json!({}),
                &ToolAnnotations::default()
            ),
            PermissionAction::Ask,
        );
    }

    #[test]
    fn deny_beats_ask_when_both_match() {
        let policy = PermissionPolicy {
            rules: vec![
                rule("*", PermissionAction::Ask),
                rule("rm_rf", PermissionAction::Deny),
            ],
            ..Default::default()
        };
        assert_eq!(
            evaluate(&policy, "rm_rf", &json!({}), &ToolAnnotations::default()),
            PermissionAction::Deny,
        );
    }

    // ── allow_all semantics ─────────────────────────────────────────

    #[test]
    fn allow_all_short_circuits_default_to_allow() {
        let policy = PermissionPolicy {
            allow_all: true,
            ..Default::default()
        };
        let action = evaluate(
            &policy,
            "cdp_click",
            &json!({}),
            &ToolAnnotations::default(),
        );
        assert_eq!(action, PermissionAction::Allow);
    }

    #[test]
    fn allow_all_still_honors_deny_rule() {
        // If the user explicitly denied a tool, allow_all shouldn't override.
        let policy = PermissionPolicy {
            allow_all: true,
            rules: vec![rule("shutdown", PermissionAction::Deny)],
            ..Default::default()
        };
        let action = evaluate(&policy, "shutdown", &json!({}), &ToolAnnotations::default());
        assert_eq!(action, PermissionAction::Deny);
    }

    // ── Destructive guardrail ───────────────────────────────────────

    #[test]
    fn destructive_guardrail_upgrades_allow_to_ask() {
        let policy = PermissionPolicy {
            rules: vec![rule("delete_file", PermissionAction::Allow)],
            require_confirm_destructive: true,
            ..Default::default()
        };
        let annotations = ToolAnnotations {
            destructive_hint: Some(true),
            ..Default::default()
        };
        let action = evaluate(&policy, "delete_file", &json!({}), &annotations);
        assert_eq!(action, PermissionAction::Ask);
    }

    #[test]
    fn destructive_guardrail_does_not_downgrade_deny() {
        let policy = PermissionPolicy {
            rules: vec![rule("delete_file", PermissionAction::Deny)],
            require_confirm_destructive: true,
            ..Default::default()
        };
        let annotations = ToolAnnotations {
            destructive_hint: Some(true),
            ..Default::default()
        };
        let action = evaluate(&policy, "delete_file", &json!({}), &annotations);
        assert_eq!(action, PermissionAction::Deny);
    }

    #[test]
    fn destructive_guardrail_off_leaves_allow_alone() {
        let policy = PermissionPolicy {
            rules: vec![rule("delete_file", PermissionAction::Allow)],
            require_confirm_destructive: false,
            ..Default::default()
        };
        let annotations = ToolAnnotations {
            destructive_hint: Some(true),
            ..Default::default()
        };
        let action = evaluate(&policy, "delete_file", &json!({}), &annotations);
        assert_eq!(action, PermissionAction::Allow);
    }

    #[test]
    fn destructive_guardrail_triggers_even_with_allow_all() {
        let policy = PermissionPolicy {
            allow_all: true,
            require_confirm_destructive: true,
            ..Default::default()
        };
        let annotations = ToolAnnotations {
            destructive_hint: Some(true),
            ..Default::default()
        };
        let action = evaluate(&policy, "delete_file", &json!({}), &annotations);
        assert_eq!(
            action,
            PermissionAction::Ask,
            "allow_all must not bypass destructive guardrail",
        );
    }

    #[test]
    fn destructive_guardrail_missing_hint_does_not_trigger() {
        // Missing destructive_hint (None) should not be treated as true.
        let policy = PermissionPolicy {
            allow_all: true,
            require_confirm_destructive: true,
            ..Default::default()
        };
        let action = evaluate(
            &policy,
            "mystery_tool",
            &json!({}),
            &ToolAnnotations::default(),
        );
        assert_eq!(action, PermissionAction::Allow);
    }

    // ── Annotation parsing ──────────────────────────────────────────

    #[test]
    fn from_tool_json_reads_top_level_annotations() {
        let tool = json!({
            "name": "delete_file",
            "annotations": {
                "destructiveHint": true,
                "readOnlyHint": false,
            }
        });
        let ann = ToolAnnotations::from_tool_json(&tool);
        assert_eq!(ann.destructive_hint, Some(true));
        assert_eq!(ann.read_only_hint, Some(false));
        assert_eq!(ann.idempotent_hint, None);
        assert_eq!(ann.open_world_hint, None);
    }

    #[test]
    fn from_tool_json_reads_function_wrapped_annotations() {
        let tool = json!({
            "type": "function",
            "function": {
                "name": "delete_file",
                "annotations": {
                    "readOnlyHint": true,
                    "idempotentHint": true,
                    "openWorldHint": false,
                }
            }
        });
        let ann = ToolAnnotations::from_tool_json(&tool);
        assert_eq!(ann.read_only_hint, Some(true));
        assert_eq!(ann.idempotent_hint, Some(true));
        assert_eq!(ann.open_world_hint, Some(false));
    }

    #[test]
    fn from_tool_json_missing_annotations_returns_empty() {
        let tool = json!({
            "type": "function",
            "function": {"name": "click"}
        });
        let ann = ToolAnnotations::from_tool_json(&tool);
        assert_eq!(ann, ToolAnnotations::default());
    }

    #[test]
    fn from_tool_json_rejects_non_bool_values() {
        let tool = json!({
            "annotations": {
                "destructiveHint": "yes",
                "readOnlyHint": 1,
            }
        });
        let ann = ToolAnnotations::from_tool_json(&tool);
        assert_eq!(ann.destructive_hint, None);
        assert_eq!(ann.read_only_hint, None);
    }
}
