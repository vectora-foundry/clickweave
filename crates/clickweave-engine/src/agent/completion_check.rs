//! Post-`agent_done` VLM completion verification.
//!
//! When the agent emits `agent_done`, the loop takes a screenshot and asks
//! the VLM whether the goal was actually achieved. A YES verdict confirms
//! the run completed normally; a NO verdict halts the run and emits a
//! disagreement event so the user can decide what to do.
//!
//! This module contains the *pure* pieces (prompt construction, YES/NO
//! parsing, screenshot-scope selection) so they can be unit tested with
//! synthetic inputs. The orchestration that calls into MCP and the VLM
//! lives in `loop_runner`.

use crate::executor::screenshot::ScreenshotScope;

/// The VLM verdict derived from a completion-check reply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum VlmVerdict {
    /// Screenshot confirms the goal was achieved — run completes normally.
    Yes,
    /// Screenshot does NOT confirm the goal — halt and surface to the user.
    No,
}

/// Pick the screenshot scope for completion verification from the CDP
/// lifecycle state.
///
/// When a CDP session is bound to a named app, capture that app's window —
/// this matches the shape the executor's action/supervision verifiers use
/// and avoids the `mode=window` / missing `app_name` MCP error that the
/// generic "focused window" default hits when nothing is tracked. When no
/// CDP session is active (native-only runs or pre-connect), fall back to a
/// full-screen capture: noisier for the VLM, but a valid request the MCP
/// server will always accept.
pub(crate) fn pick_completion_screenshot_scope(
    connected_app: Option<&(String, i32)>,
) -> ScreenshotScope {
    match connected_app {
        Some((name, _pid)) => ScreenshotScope::Window(name.clone()),
        None => ScreenshotScope::Screen,
    }
}

/// Build the user-facing prompt text sent to the VLM alongside the screenshot.
pub(crate) fn build_completion_prompt(goal: &str, summary: &str) -> String {
    format!(
        "The goal was: \"{}\".\n\
         The agent believes it is complete: \"{}\".\n\
         Does this screenshot confirm the goal was achieved? \
         Reply with YES or NO and a one-sentence explanation.",
        goal, summary,
    )
}

/// Parse a VLM reply into a YES/NO verdict.
///
/// Matching requires the reply's first token to be exactly `YES` or `NO`
/// (after trimming whitespace, markdown fences, and surrounding
/// punctuation). Anything else (`"yeahh"`, `"not sure"`, `"YESN'T"`,
/// empty body) falls through to `No` — fail-closed so the operator sees
/// the disagreement when the VLM didn't emit an explicit verdict.
pub(crate) fn parse_yes_no(reply: &str) -> VlmVerdict {
    let first_token = reply
        .trim_start()
        .split(|c: char| c.is_whitespace() || c == '.' || c == ',' || c == ':' || c == ';')
        .find(|t| !t.is_empty())
        .unwrap_or("");
    let stripped = first_token.trim_matches(|c: char| !c.is_alphanumeric());
    let upper = stripped.to_ascii_uppercase();
    match upper.as_str() {
        "YES" => VlmVerdict::Yes,
        _ => VlmVerdict::No,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn yes_plain_maps_to_yes() {
        assert_eq!(
            parse_yes_no("YES, the login dialog is visible."),
            VlmVerdict::Yes
        );
    }

    #[test]
    fn yes_lowercase_maps_to_yes() {
        assert_eq!(parse_yes_no("yes - looks good"), VlmVerdict::Yes);
    }

    #[test]
    fn yes_mixed_case_maps_to_yes() {
        assert_eq!(parse_yes_no("Yes. Confirmed."), VlmVerdict::Yes);
    }

    #[test]
    fn no_plain_maps_to_no() {
        assert_eq!(
            parse_yes_no("NO, the page still shows an error."),
            VlmVerdict::No
        );
    }

    #[test]
    fn no_lowercase_maps_to_no() {
        assert_eq!(parse_yes_no("no, not yet"), VlmVerdict::No);
    }

    #[test]
    fn garbage_defaults_to_no() {
        assert_eq!(parse_yes_no("I am not sure"), VlmVerdict::No);
    }

    #[test]
    fn empty_defaults_to_no() {
        assert_eq!(parse_yes_no(""), VlmVerdict::No);
    }

    #[test]
    fn whitespace_defaults_to_no() {
        assert_eq!(parse_yes_no("   \n\t "), VlmVerdict::No);
    }

    #[test]
    fn yes_wrapped_in_markdown_parses() {
        assert_eq!(parse_yes_no("**YES** — confirmed"), VlmVerdict::Yes);
    }

    #[test]
    fn yes_wrapped_in_quotes_parses() {
        assert_eq!(
            parse_yes_no("\"YES\" the screenshot shows the result"),
            VlmVerdict::Yes
        );
    }

    #[test]
    fn no_leading_dash_parses() {
        assert_eq!(parse_yes_no("- no, it failed"), VlmVerdict::No);
    }

    #[test]
    fn prompt_includes_goal_and_summary() {
        let p = build_completion_prompt("Open the settings page", "I clicked gear icon");
        assert!(p.contains("Open the settings page"));
        assert!(p.contains("I clicked gear icon"));
        assert!(p.contains("YES or NO"));
    }

    #[test]
    fn nope_does_not_count_as_no_prefix() {
        // Strict match — any token other than exactly YES/NO falls through
        // to No (fail-closed). "nope" ends up in the No branch, but via
        // the default path rather than a "starts with NO" bypass.
        assert_eq!(parse_yes_no("nope"), VlmVerdict::No);
    }

    #[test]
    fn yesnt_rejected_as_non_yes() {
        // A prefix like "YESN'T" must not map to Yes under strict parsing.
        assert_eq!(parse_yes_no("YESN'T — not confirmed"), VlmVerdict::No);
    }

    #[test]
    fn yes_with_trailing_text_parses() {
        // The reply "YES, but the modal is still open" still starts with
        // YES; the strict parser accepts the first-token match.
        assert_eq!(
            parse_yes_no("YES, but the modal is still open"),
            VlmVerdict::Yes
        );
    }

    #[test]
    fn scope_uses_connected_app_window() {
        let connected = ("Signal".to_string(), 16024);
        let scope = pick_completion_screenshot_scope(Some(&connected));
        match scope {
            ScreenshotScope::Window(name) => assert_eq!(name, "Signal"),
            other => panic!("expected Window(Signal), got {:?}", other),
        }
    }

    #[test]
    fn scope_falls_back_to_screen_without_connection() {
        let scope = pick_completion_screenshot_scope(None);
        assert!(matches!(scope, ScreenshotScope::Screen));
    }
}
