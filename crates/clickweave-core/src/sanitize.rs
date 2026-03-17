/// Shared string sanitization utilities for path and identifier generation.

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

/// Sanitize a string for use as a variable/node name prefix.
///
/// Lowercases, replaces non-alphanumeric chars (except `_`) with underscores.
/// Does NOT collapse consecutive underscores (preserving current behavior).
///
/// Examples: `"Find Text"` -> `"find_text"`, `"Click (Login Button)"` -> `"click__login_button_"`
pub fn sanitize_for_node_name(name: &str) -> String {
    name.to_lowercase()
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- sanitize_for_path ---

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
    fn path_empty_string() {
        assert_eq!(sanitize_for_path(""), "unnamed");
    }

    #[test]
    fn path_only_dashes() {
        assert_eq!(sanitize_for_path("---"), "unnamed");
    }

    #[test]
    fn path_only_whitespace() {
        assert_eq!(sanitize_for_path("   "), "unnamed");
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

    // --- sanitize_for_node_name ---

    #[test]
    fn node_name_basic_spaces() {
        assert_eq!(sanitize_for_node_name("Find Text"), "find_text");
    }

    #[test]
    fn node_name_special_chars() {
        assert_eq!(
            sanitize_for_node_name("Click (Login Button)"),
            "click__login_button_"
        );
    }

    #[test]
    fn node_name_preserves_underscores() {
        assert_eq!(sanitize_for_node_name("my_node_1"), "my_node_1");
    }

    #[test]
    fn node_name_empty_string() {
        assert_eq!(sanitize_for_node_name(""), "");
    }

    #[test]
    fn node_name_does_not_collapse_underscores() {
        assert_eq!(sanitize_for_node_name("a  b"), "a__b");
    }

    #[test]
    fn node_name_uppercase() {
        assert_eq!(sanitize_for_node_name("HELLO World"), "hello_world");
    }

    #[test]
    fn node_name_mixed_special_chars() {
        assert_eq!(sanitize_for_node_name("a@b!c"), "a_b_c");
    }
}
