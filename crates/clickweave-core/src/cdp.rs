/// Build the CDP server name for a given app.
/// Each Electron/Chrome app gets its own named server: `"cdp:<app_name>"`.
pub fn cdp_server_name(app_name: &str) -> String {
    format!("cdp:{app_name}")
}

/// Parse a CDP snapshot and find interactive elements whose text contains the target.
/// Returns a vec of (uid, label) tuples.
///
/// Leaf text nodes (`StaticText`, `InlineTextBox`) are excluded since they
/// duplicate their parent's label and are not useful as click targets.
///
/// The snapshot from chrome-devtools `take_snapshot` is a text representation
/// of the accessibility tree where each element has a UID. Format:
/// ```text
/// uid=1_0 button "Submit"
/// uid=1_1 link "Friends"
/// ```
pub fn find_elements_in_snapshot(snapshot_text: &str, target: &str) -> Vec<(String, String)> {
    let target_lower = target.to_lowercase();
    let mut exact = Vec::new();
    let mut substring = Vec::new();
    for line in snapshot_text.lines() {
        let Some((uid, is_leaf)) = parse_line_uid(line) else {
            continue;
        };
        if is_leaf {
            continue;
        }
        let label = extract_label(line);
        let label_lower = label.to_lowercase();
        if label_lower == target_lower {
            exact.push((uid, label));
        } else if label_lower.contains(&target_lower) || line.to_lowercase().contains(&target_lower)
        {
            substring.push((uid, label));
        }
    }
    // Prefer exact label matches; fall back to substring matches.
    if exact.is_empty() { substring } else { exact }
}

/// Parse a snapshot line to extract its UID and whether it's a leaf text node.
///
/// Returns `(uid, is_leaf)` or `None` if the line has no UID.
/// Handles both `uid=1_11 treeitem ...` and `uid="e1" button ...` formats.
fn parse_line_uid(line: &str) -> Option<(String, bool)> {
    let uid_pos = line.find("uid=")?;
    let rest = &line[uid_pos + 4..];
    let (uid, after_uid) = if rest.starts_with('"') {
        let end = rest[1..].find('"')?;
        let uid = &rest[1..1 + end];
        (uid, &rest[1 + end + 1..])
    } else {
        let end = rest.find(' ').unwrap_or(rest.len());
        let uid = &rest[..end];
        if uid.is_empty() {
            return None;
        }
        (uid, &rest[end..])
    };
    let role = after_uid.trim_start();
    let is_leaf = role.starts_with("StaticText") || role.starts_with("InlineTextBox");
    Some((uid.to_string(), is_leaf))
}

/// Extract the visible label text from a snapshot line.
///
/// The label is the first standalone quoted string (not preceded by `=`).
/// For `uid=1_5 treeitem "Direct Messages" level="1"` → `Direct Messages`.
/// Falls back to the trimmed line if no standalone quoted string is found.
fn extract_label(line: &str) -> String {
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'"' {
            // Check if this quote is preceded by '=' (attribute value) — skip it.
            let is_attr_value = i > 0 && bytes[i - 1] == b'=';
            if let Some(end) = line[i + 1..].find('"') {
                if !is_attr_value {
                    let label = &line[i + 1..i + 1 + end];
                    if !label.is_empty() {
                        return label.to_string();
                    }
                }
                i = i + 1 + end + 1; // skip past closing quote
            } else {
                break;
            }
        } else {
            i += 1;
        }
    }
    line.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::{cdp_server_name, find_elements_in_snapshot, parse_line_uid};

    // Real chrome-devtools-mcp format (unquoted UIDs).
    const SNAPSHOT: &str = r##"
uid=1_0 RootWebArea "#avail | DevCrew" url="https://discord.com/"
  uid=1_1 button "Go back"
  uid=1_2 link "Friends"
  uid=1_3 heading "Settings"
  uid=1_4 button "Submit Form"
  uid=1_5 treeitem "Direct Messages" level="1" selectable
    uid=1_6 StaticText "Direct Messages"
  uid=1_7 button "Add Friends to DM"
"##;

    // Legacy format (quoted UIDs in brackets).
    const SNAPSHOT_QUOTED: &str = r##"
[uid="e1"] button "Submit"
[uid="e2"] link "Friends"
"##;

    #[test]
    fn server_name_format() {
        assert_eq!(cdp_server_name("Discord"), "cdp:Discord");
        assert_eq!(cdp_server_name("Google Chrome"), "cdp:Google Chrome");
    }

    #[test]
    fn parse_uid_unquoted() {
        let (uid, is_leaf) = parse_line_uid(r#"uid=1_5 treeitem "Direct Messages""#).unwrap();
        assert_eq!(uid, "1_5");
        assert!(!is_leaf);

        let (uid, _) = parse_line_uid("  uid=1_0 RootWebArea").unwrap();
        assert_eq!(uid, "1_0");
    }

    #[test]
    fn parse_uid_quoted() {
        let (uid, is_leaf) = parse_line_uid(r#"[uid="e1"] button "Submit""#).unwrap();
        assert_eq!(uid, "e1");
        assert!(!is_leaf);
    }

    #[test]
    fn parse_uid_none() {
        assert!(parse_line_uid("no uid here").is_none());
        assert!(parse_line_uid("## Latest page snapshot").is_none());
    }

    #[test]
    fn parse_uid_detects_leaf_text() {
        let (_, is_leaf) = parse_line_uid(r#"    uid=1_6 StaticText "Direct Messages""#).unwrap();
        assert!(is_leaf);

        let (_, is_leaf) = parse_line_uid(r#"uid=1_2 link "Friends""#).unwrap();
        assert!(!is_leaf);
    }

    #[test]
    fn single_match() {
        // "Direct Messages" appears on both a treeitem and a StaticText child.
        // StaticText is filtered out, so only the treeitem matches.
        let matches = find_elements_in_snapshot(SNAPSHOT, "Direct Messages");
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].0, "1_5");
        assert_eq!(matches[0].1, "Direct Messages");
    }

    #[test]
    fn single_match_quoted_format() {
        let matches = find_elements_in_snapshot(SNAPSHOT_QUOTED, "Friends");
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].0, "e2");
        assert_eq!(matches[0].1, "Friends");
    }

    #[test]
    fn multiple_matches() {
        // "Friends" appears in the link; searching for a broader term
        let matches = find_elements_in_snapshot(SNAPSHOT, "button");
        // "Go back" and "Submit Form" both have button in the line
        assert!(matches.len() >= 2);
    }

    #[test]
    fn case_insensitive() {
        let matches = find_elements_in_snapshot(SNAPSHOT, "settings");
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].0, "1_3");
        assert_eq!(matches[0].1, "Settings");
    }

    #[test]
    fn exact_label_preferred_over_substring() {
        // "Friends" matches both link "Friends" (exact) and button "Add Friends to DM" (substring).
        // Only the exact match should be returned.
        let matches = find_elements_in_snapshot(SNAPSHOT, "Friends");
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].0, "1_2");
        assert_eq!(matches[0].1, "Friends");
    }

    #[test]
    fn no_matches() {
        let matches = find_elements_in_snapshot(SNAPSHOT, "Nonexistent");
        assert!(matches.is_empty());
    }

    #[test]
    fn empty_snapshot() {
        let matches = find_elements_in_snapshot("", "Submit");
        assert!(matches.is_empty());
    }
}
