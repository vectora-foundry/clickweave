//! Post-`agent_done` VLM completion verification.
//!
//! When the agent emits `agent_done`, the loop takes a screenshot and asks
//! the VLM whether the goal was actually achieved. A YES verdict confirms
//! the run completed normally; a NO verdict halts the run and emits a
//! disagreement event so the user can decide what to do.
//!
//! This module contains the *pure* pieces (prompt construction, YES/NO
//! parsing) so they can be unit tested with synthetic inputs. The
//! orchestration that calls into MCP and the VLM lives in `loop_runner`.

/// The VLM verdict derived from a completion-check reply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum VlmVerdict {
    /// Screenshot confirms the goal was achieved — run completes normally.
    Yes,
    /// Screenshot does NOT confirm the goal — halt and surface to the user.
    No,
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
/// Matching is forgiving:
/// - case-insensitive on the first non-whitespace token
/// - anything starting with "YES" maps to `Yes`
/// - anything starting with "NO" maps to `No`
/// - any other reply defaults to `No` (fail-closed: if the VLM didn't
///   explicitly confirm, treat it as a disagreement so the user sees it)
pub(crate) fn parse_yes_no(reply: &str) -> VlmVerdict {
    // Grab the first non-whitespace token. We strip any leading punctuation
    // so models that wrap the verdict in quotes or markdown (`**YES**`,
    // `"NO"`, `- yes`) still parse cleanly.
    let first_token = reply
        .trim_start()
        .split(|c: char| c.is_whitespace() || c == '.' || c == ',' || c == ':' || c == ';')
        .find(|t| !t.is_empty())
        .unwrap_or("");
    let stripped = first_token.trim_matches(|c: char| !c.is_alphanumeric());
    let upper = stripped.to_ascii_uppercase();
    if upper.starts_with("YES") {
        VlmVerdict::Yes
    } else {
        // Both "starts with NO" and "neither" fail-close to No — if the VLM
        // didn't emit an explicit YES we treat it as a disagreement so the
        // user sees the reasoning.
        VlmVerdict::No
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
    fn nope_word_does_not_count_as_no_prefix_edge_case() {
        // "nope" starts with "no" so it currently maps to No — this documents
        // the intentional lenient behaviour. Any further-than-NO word that
        // starts with those two letters also defaults to No, which is the
        // fail-closed default anyway.
        assert_eq!(parse_yes_no("nope"), VlmVerdict::No);
    }
}
