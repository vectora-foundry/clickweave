//! Shared string sanitization utilities for path and identifier generation.

/// Sanitize a string for use as a filesystem path component.
///
/// Lowercases, replaces non-alphanumeric chars with `-`, collapses consecutive
/// dashes, trims leading/trailing dashes, and returns `"unnamed"` for empty input.
///
/// Examples: `"Open Calculator"` -> `"open-calculator"`, `"  My---Workflow  "` -> `"my-workflow"`
pub fn sanitize_for_path(name: &str) -> String {
    let mut result = String::with_capacity(name.len());
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() {
            result.push(ch.to_ascii_lowercase());
        } else {
            result.push('-');
        }
    }
    // Collapse consecutive dashes and trim leading/trailing dashes
    let collapsed: String = result
        .split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    if collapsed.is_empty() {
        return "unnamed".to_string();
    }
    collapsed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_basic_spaces() {
        assert_eq!(sanitize_for_path("Open Calculator"), "open-calculator");
    }

    #[test]
    fn path_special_chars() {
        assert_eq!(sanitize_for_path("Click #5"), "click-5");
    }

    #[test]
    fn path_collapses_consecutive_dashes() {
        assert_eq!(sanitize_for_path("  My---Workflow  "), "my-workflow");
    }

    #[test]
    fn path_uppercase() {
        assert_eq!(sanitize_for_path("UPPER case"), "upper-case");
    }

    #[test]
    fn path_slashes_and_backslashes() {
        assert_eq!(sanitize_for_path("a/b\\c"), "a-b-c");
    }

    #[test]
    fn path_empty_or_blank_falls_back_to_unnamed() {
        for input in ["", "---", "   "] {
            assert_eq!(sanitize_for_path(input), "unnamed", "input: {input:?}");
        }
    }

    #[test]
    fn path_single_word() {
        assert_eq!(sanitize_for_path("hello"), "hello");
    }

    #[test]
    fn path_mixed_special_chars() {
        assert_eq!(sanitize_for_path("a@b!c$d"), "a-b-c-d");
    }

    #[test]
    fn path_leading_trailing_special() {
        assert_eq!(sanitize_for_path("--hello--"), "hello");
    }

    #[test]
    fn path_unicode_replaced() {
        // U+0301 (combining accent) is not ascii-alphanumeric, becomes a trailing dash that gets trimmed
        assert_eq!(sanitize_for_path("cafe\u{0301}"), "cafe");
    }
}
