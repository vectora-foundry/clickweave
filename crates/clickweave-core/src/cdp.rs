/// Pick a random port in the ephemeral range (49152-65535).
pub fn rand_ephemeral_port() -> u16 {
    use std::time::SystemTime;
    let seed = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos();
    let raw = seed.wrapping_mul(1664525).wrapping_add(1013904223);
    let range = 65535 - 49152;
    49152 + (raw % range) as u16
}

/// A match found in a CDP snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotMatch {
    pub uid: String,
    pub label: String,
    pub role: String,
    pub url: Option<String>,
    pub parent_role: Option<String>,
    pub parent_name: Option<String>,
}

/// Parse a CDP snapshot and find interactive elements whose text contains the target.
/// Returns a vec of `SnapshotMatch` with uid, label, role, and url.
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
pub fn find_elements_in_snapshot(snapshot_text: &str, target: &str) -> Vec<SnapshotMatch> {
    let (exact, substring) = find_matches_split(snapshot_text, target);
    // Prefer exact label matches; fall back to substring matches.
    if exact.is_empty() { substring } else { exact }
}

/// Core parsing: scan a CDP snapshot for elements matching `target`,
/// returning exact-label matches and substring matches separately.
///
/// Callers that need both sets (e.g. `search_interactive_elements`) use
/// this directly instead of `find_elements_in_snapshot` which collapses
/// them with an exact-preference rule.
fn find_matches_split(
    snapshot_text: &str,
    target: &str,
) -> (Vec<SnapshotMatch>, Vec<SnapshotMatch>) {
    let target_lower = target.to_lowercase();
    let mut exact = Vec::new();
    let mut substring = Vec::new();

    // Track parent context via an indentation-based stack.
    // Each entry: (indent_level, role, label).
    let mut parent_stack: Vec<(usize, String, Option<String>)> = Vec::new();

    for line in snapshot_text.lines() {
        let Some((uid, role, is_leaf)) = parse_line_uid(line) else {
            continue;
        };

        let indent = line.len() - line.trim_start().len();

        while let Some(top) = parent_stack.last() {
            if top.0 >= indent {
                parent_stack.pop();
            } else {
                break;
            }
        }

        // Leaf text nodes (StaticText, InlineTextBox) are terminal — they
        // can never be parents, so skip label extraction and stack push.
        if is_leaf {
            continue;
        }

        let label = extract_label(line);

        let label_for_stack = if label == line.trim() {
            None
        } else {
            Some(label.clone())
        };
        parent_stack.push((indent, role.to_string(), label_for_stack));

        let label_lower = label.to_lowercase();
        let is_match = if label_lower == target_lower {
            Some(true)
        } else if label_lower.contains(&target_lower) || line.to_lowercase().contains(&target_lower)
        {
            Some(false)
        } else {
            None
        };
        if let Some(is_exact) = is_match {
            let (parent_role, parent_name) = parent_stack
                .iter()
                .rev()
                .nth(1)
                .map(|(_, r, n)| (Some(r.clone()), n.clone()))
                .unwrap_or((None, None));

            let m = SnapshotMatch {
                uid,
                label: label.clone(),
                role: role.to_string(),
                url: extract_url(line),
                parent_role,
                parent_name,
            };
            if is_exact {
                exact.push(m);
            } else {
                substring.push(m);
            }
        }
    }
    (exact, substring)
}

/// Parse a snapshot line to extract its UID, role, and whether it's a leaf text node.
///
/// Returns `(uid, role, is_leaf)` or `None` if the line has no UID.
/// Handles both `uid=1_11 treeitem ...` and `uid="e1" button ...` formats.
fn parse_line_uid(line: &str) -> Option<(String, &str, bool)> {
    let uid_pos = line.find("uid=")?;
    let rest = &line[uid_pos + 4..];
    let (uid, after_uid) = if let Some(quoted) = rest.strip_prefix('"') {
        let end = quoted.find('"')?;
        let uid = &quoted[..end];
        (uid, &quoted[end + 1..])
    } else {
        let end = rest.find(' ').unwrap_or(rest.len());
        let uid = &rest[..end];
        if uid.is_empty() {
            return None;
        }
        (uid, &rest[end..])
    };
    let after_uid = after_uid.trim_start().trim_start_matches(']').trim_start();
    let role = after_uid.split_whitespace().next().unwrap_or("");
    let is_leaf = role.starts_with("StaticText") || role.starts_with("InlineTextBox");
    Some((uid.to_string(), role, is_leaf))
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

/// Retain only matches satisfying `predicate`, but keep the original set
/// if no match satisfies it (avoids eliminating all candidates).
fn narrow_if_any(matches: &mut Vec<SnapshotMatch>, predicate: impl Fn(&SnapshotMatch) -> bool) {
    if matches.iter().any(&predicate) {
        matches.retain(predicate);
    }
}

/// Narrow matches by element role and/or URL.
/// Keeps the original set if filtering would eliminate all candidates.
pub fn narrow_matches(
    matches: &mut Vec<SnapshotMatch>,
    expected_role: Option<&str>,
    expected_href: Option<&str>,
) {
    if let Some(role) = expected_role {
        let role_lower = role.to_lowercase();
        narrow_if_any(matches, |m| m.role.to_lowercase() == role_lower);
    }
    if let Some(href) = expected_href {
        narrow_if_any(matches, |m| m.url.as_deref() == Some(href));
    }
}

/// Narrow matches by parent role and/or parent name.
/// Keeps the original set if filtering would eliminate all candidates.
pub fn narrow_by_parent(
    matches: &mut Vec<SnapshotMatch>,
    expected_parent_role: Option<&str>,
    expected_parent_name: Option<&str>,
) {
    if let Some(role) = expected_parent_role {
        let role_lower = role.to_lowercase();
        narrow_if_any(matches, |m| {
            m.parent_role
                .as_ref()
                .is_some_and(|r| r.to_lowercase() == role_lower)
        });
    }
    if let Some(name) = expected_parent_name {
        let name_lower = name.to_lowercase();
        narrow_if_any(matches, |m| {
            m.parent_name
                .as_ref()
                .is_some_and(|n| n.to_lowercase().contains(&name_lower))
        });
    }
}

/// Result of searching interactive elements in a CDP snapshot.
#[derive(Debug, Clone)]
pub struct InteractiveSearchResult {
    /// Interactive element matches.
    pub matches: Vec<SnapshotMatch>,
    /// Best-effort count of non-interactive matches omitted from results.
    /// Note: `find_elements_in_snapshot()` excludes leaf text nodes
    /// (StaticText, InlineTextBox) before matching, and returns only exact
    /// matches when they exist, so this count may understate the true total
    /// of non-interactive elements containing the query text.
    pub omitted_count: usize,
}

/// Interactive ARIA roles returned by `search_interactive_elements`.
/// Non-interactive roles (heading, StaticText, generic, etc.) are counted but omitted.
const INTERACTIVE_ROLES: &[&str] = &[
    "button",
    "checkbox",
    "combobox",
    "link",
    "menuitem",
    "menuitemcheckbox",
    "menuitemradio",
    "option",
    "radio",
    "searchbox",
    "slider",
    "spinbutton",
    "switch",
    "tab",
    "textbox",
    "treeitem",
];

/// Search a CDP snapshot for interactive elements matching a query.
///
/// Unlike `find_elements_in_snapshot()`, this function:
/// - Only returns elements with interactive ARIA roles
/// - Supports an optional role filter
/// - Caps results at `max_results`
/// - Reports how many non-interactive matches were omitted
///
/// Uses `find_matches_split` directly so the interactive-role filter
/// is applied before the exact/substring preference. This avoids a
/// false-negative when an exact-match non-interactive element (e.g. a
/// heading "Settings") would shadow a substring-match interactive one
/// (e.g. a button "Open Settings").
pub fn search_interactive_elements(
    snapshot_text: &str,
    query: &str,
    role_filter: Option<&str>,
    max_results: usize,
) -> InteractiveSearchResult {
    let (exact, substring) = find_matches_split(snapshot_text, query);

    let is_interactive = |m: &SnapshotMatch| {
        INTERACTIVE_ROLES
            .iter()
            .any(|r| r.eq_ignore_ascii_case(&m.role))
    };

    // Partition exact matches into interactive and non-interactive.
    let mut omitted_count = 0;
    let mut matches: Vec<SnapshotMatch> = Vec::new();
    for m in exact {
        if is_interactive(&m) {
            matches.push(m);
        } else {
            omitted_count += 1;
        }
    }

    // If exact matches produced no interactive results, try substring matches.
    if matches.is_empty() {
        for m in substring {
            if is_interactive(&m) {
                matches.push(m);
            } else {
                omitted_count += 1;
            }
        }
    }

    if let Some(role) = role_filter {
        matches.retain(|m| m.role.eq_ignore_ascii_case(role));
    }

    matches.truncate(max_results);

    InteractiveSearchResult {
        matches,
        omitted_count,
    }
}

/// Extract `url=` attribute value from a snapshot line.
///
/// For `uid=1_0 link "Home" url="https://example.com"` → `Some("https://example.com")`.
fn extract_url(line: &str) -> Option<String> {
    let marker = "url=\"";
    let start = line.find(marker)? + marker.len();
    let end = line[start..].find('"')? + start;
    Some(line[start..end].to_string())
}

#[cfg(test)]
mod tests {
    use super::{
        extract_label, extract_url, find_elements_in_snapshot, narrow_by_parent, narrow_matches,
        parse_line_uid, search_interactive_elements,
    };

    const SNAPSHOT_MIXED_ROLES: &str = r##"
uid=1_0 RootWebArea "App" url="https://app.example.com/"
  uid=1_1 navigation "Sidebar"
    uid=1_2 button "Settings"
    uid=1_3 link "Home"
    uid=1_4 heading "Menu"
      uid=1_5 StaticText "Menu"
  uid=1_6 main "Content"
    uid=1_7 textbox "Search"
    uid=1_8 button "Submit"
    uid=1_9 generic "container"
    uid=1_10 checkbox "Remember me"
    uid=1_11 heading "Welcome"
      uid=1_12 StaticText "Welcome"
"##;

    #[test]
    fn search_interactive_returns_only_interactive_roles() {
        let result = search_interactive_elements(SNAPSHOT_MIXED_ROLES, "Menu", None, 10);
        // "Menu" matches heading uid=1_4 (non-interactive) — should be omitted.
        assert!(
            result.matches.is_empty(),
            "heading should not be in interactive results"
        );
        assert_eq!(result.omitted_count, 1);
    }

    #[test]
    fn search_interactive_returns_buttons_and_links() {
        let result = search_interactive_elements(SNAPSHOT_MIXED_ROLES, "Settings", None, 10);
        assert_eq!(result.matches.len(), 1);
        assert_eq!(result.matches[0].uid, "1_2");
        assert_eq!(result.matches[0].role, "button");
        assert_eq!(result.omitted_count, 0);
    }

    #[test]
    fn search_interactive_role_filter() {
        let result = search_interactive_elements(SNAPSHOT_MIXED_ROLES, "button", None, 10);
        let all_uids: Vec<&str> = result.matches.iter().map(|m| m.uid.as_str()).collect();
        assert!(all_uids.contains(&"1_2"), "Settings button should match");
        assert!(all_uids.contains(&"1_8"), "Submit button should match");

        let filtered =
            search_interactive_elements(SNAPSHOT_MIXED_ROLES, "button", Some("button"), 10);
        assert!(filtered.matches.iter().all(|m| m.role == "button"));
    }

    #[test]
    fn search_interactive_max_results() {
        let result = search_interactive_elements(SNAPSHOT_MIXED_ROLES, "e", None, 2);
        assert!(result.matches.len() <= 2, "should cap at max_results");
    }

    #[test]
    fn search_interactive_empty_snapshot() {
        let result = search_interactive_elements("", "anything", None, 10);
        assert!(result.matches.is_empty());
        assert_eq!(result.omitted_count, 0);
    }

    #[test]
    fn search_interactive_no_matches() {
        let result = search_interactive_elements(SNAPSHOT_MIXED_ROLES, "Nonexistent", None, 10);
        assert!(result.matches.is_empty());
        assert_eq!(result.omitted_count, 0);
    }

    #[test]
    fn search_interactive_falls_back_to_substring_when_exact_is_non_interactive() {
        // "Settings" exactly matches heading uid=1_4 (non-interactive) and
        // substring-matches button uid=1_2 "Settings" via the line containing
        // "button" + "Settings". But the button label is also "Settings" (exact),
        // so both match exactly. Let's use a snapshot where the exact match is
        // only non-interactive but a substring match is interactive.
        let snapshot = r##"
uid=1_0 RootWebArea "App"
  uid=1_1 heading "Settings"
  uid=1_2 button "Open Settings"
"##;
        let result = search_interactive_elements(snapshot, "Settings", None, 10);
        // Without the fallback, only heading "Settings" (exact) would be returned
        // and filtered out, leaving no results. With the fallback, button "Open
        // Settings" (substring) should be found.
        assert_eq!(result.matches.len(), 1);
        assert_eq!(result.matches[0].uid, "1_2");
        assert_eq!(result.matches[0].role, "button");
        assert_eq!(result.omitted_count, 1); // the heading
    }

    #[test]
    fn search_interactive_includes_checkbox_and_textbox() {
        let result = search_interactive_elements(SNAPSHOT_MIXED_ROLES, "Remember", None, 10);
        assert_eq!(result.matches.len(), 1);
        assert_eq!(result.matches[0].role, "checkbox");

        let result = search_interactive_elements(SNAPSHOT_MIXED_ROLES, "Search", None, 10);
        assert_eq!(result.matches.len(), 1);
        assert_eq!(result.matches[0].role, "textbox");
    }

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
    fn parse_uid_unquoted() {
        let (uid, role, is_leaf) = parse_line_uid(r#"uid=1_5 treeitem "Direct Messages""#).unwrap();
        assert_eq!(uid, "1_5");
        assert_eq!(role, "treeitem");
        assert!(!is_leaf);

        let (uid, role, _) = parse_line_uid("  uid=1_0 RootWebArea").unwrap();
        assert_eq!(uid, "1_0");
        assert_eq!(role, "RootWebArea");
    }

    #[test]
    fn parse_uid_quoted() {
        let (uid, role, is_leaf) = parse_line_uid(r#"[uid="e1"] button "Submit""#).unwrap();
        assert_eq!(uid, "e1");
        assert_eq!(role, "button");
        assert!(!is_leaf);
    }

    #[test]
    fn parse_uid_none() {
        assert!(parse_line_uid("no uid here").is_none());
        assert!(parse_line_uid("## Latest page snapshot").is_none());
    }

    #[test]
    fn parse_uid_detects_leaf_text() {
        let (_, _, is_leaf) =
            parse_line_uid(r#"    uid=1_6 StaticText "Direct Messages""#).unwrap();
        assert!(is_leaf);

        let (_, _, is_leaf) = parse_line_uid(r#"uid=1_2 link "Friends""#).unwrap();
        assert!(!is_leaf);
    }

    #[test]
    fn single_match() {
        // "Direct Messages" appears on both a treeitem and a StaticText child.
        // StaticText is filtered out, so only the treeitem matches.
        let matches = find_elements_in_snapshot(SNAPSHOT, "Direct Messages");
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].uid, "1_5");
        assert_eq!(matches[0].label, "Direct Messages");
    }

    #[test]
    fn single_match_quoted_format() {
        let matches = find_elements_in_snapshot(SNAPSHOT_QUOTED, "Friends");
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].uid, "e2");
        assert_eq!(matches[0].label, "Friends");
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
        assert_eq!(matches[0].uid, "1_3");
        assert_eq!(matches[0].label, "Settings");
    }

    #[test]
    fn exact_label_preferred_over_substring() {
        // "Friends" matches both link "Friends" (exact) and button "Add Friends to DM" (substring).
        // Only the exact match should be returned.
        let matches = find_elements_in_snapshot(SNAPSHOT, "Friends");
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].uid, "1_2");
        assert_eq!(matches[0].label, "Friends");
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

    // --- extract_label ---

    #[test]
    fn extract_label_standalone_quoted() {
        let label = extract_label(r#"uid=1_5 treeitem "Direct Messages" level="1""#);
        assert_eq!(label, "Direct Messages");
    }

    #[test]
    fn extract_label_only_attr_values_falls_back() {
        // All quoted strings preceded by '=' — should fall back to trimmed line.
        let line = r#"uid=1_0 RootWebArea url="https://example.com""#;
        let label = extract_label(line);
        assert_eq!(label, line);
    }

    #[test]
    fn extract_label_first_standalone_wins() {
        let label = extract_label(r#"uid=1 button "First" "Second""#);
        assert_eq!(label, "First");
    }

    #[test]
    fn extract_label_skips_empty_quoted() {
        // Empty standalone "" should be skipped, next standalone wins.
        let label = extract_label(r#"uid=1 button "" "Real Label""#);
        assert_eq!(label, "Real Label");
    }

    #[test]
    fn extract_label_no_quotes_falls_back() {
        let line = "uid=1_0 RootWebArea";
        let label = extract_label(line);
        assert_eq!(label, line);
    }

    // --- parse_line_uid edge cases ---

    #[test]
    fn parse_uid_empty_value() {
        // "uid= button ..." has empty unquoted UID — should return None.
        assert!(parse_line_uid(r#"uid= button "Foo""#).is_none());
    }

    #[test]
    fn parse_uid_detects_inline_text_box() {
        let (_, _, is_leaf) = parse_line_uid(r#"uid=1_8 InlineTextBox "Hello""#).unwrap();
        assert!(is_leaf);
    }

    // --- find_elements_in_snapshot: line-level substring ---

    #[test]
    fn line_substring_match_when_target_in_role() {
        // "button" doesn't appear in the label "Go back", but does in the line.
        let matches = find_elements_in_snapshot(SNAPSHOT, "button");
        let uids: Vec<&str> = matches.iter().map(|m| m.uid.as_str()).collect();
        assert!(
            uids.contains(&"1_1"),
            "should match 'Go back' button via line"
        );
        assert!(
            uids.contains(&"1_4"),
            "should match 'Submit Form' button via line"
        );
        assert!(
            uids.contains(&"1_7"),
            "should match 'Add Friends to DM' button via line"
        );
    }

    // --- extract_url ---

    #[test]
    fn extract_url_present() {
        let url = extract_url(r##"uid=1_0 RootWebArea "#avail" url="https://discord.com/""##);
        assert_eq!(url.as_deref(), Some("https://discord.com/"));
    }

    #[test]
    fn extract_url_absent() {
        assert!(extract_url(r#"uid=1_1 button "Go back""#).is_none());
    }

    #[test]
    fn extract_url_multiple_links() {
        let snapshot = r#"uid=1_0 link "Home" url="https://example.com/home"
uid=1_1 link "Home" url="https://example.com/other""#;
        let matches = find_elements_in_snapshot(snapshot, "Home");
        assert_eq!(matches.len(), 2);
        assert_eq!(matches[0].url.as_deref(), Some("https://example.com/home"));
        assert_eq!(matches[1].url.as_deref(), Some("https://example.com/other"));
    }

    // --- SnapshotMatch role field ---

    #[test]
    fn find_elements_returns_role() {
        let matches = find_elements_in_snapshot(SNAPSHOT, "Friends");
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].role, "link");
    }

    // --- Parent context ---

    #[test]
    fn find_elements_includes_parent_context() {
        // "Direct Messages" has parent RootWebArea in SNAPSHOT.
        let matches = find_elements_in_snapshot(SNAPSHOT, "Direct Messages");
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].parent_role.as_deref(), Some("RootWebArea"));
        assert_eq!(matches[0].parent_name.as_deref(), Some("#avail | DevCrew"));
    }

    const SNAPSHOT_MULTI_DM: &str = r##"
uid=1_0 RootWebArea "Discord" url="https://discord.com/"
  uid=1_1 navigation "Servers sidebar"
    uid=1_2 treeitem "Direct Messages"
      uid=1_3 StaticText "Direct Messages"
  uid=1_4 complementary "Channel sidebar"
    uid=1_5 heading "Direct Messages" level="1"
      uid=1_6 StaticText "Direct Messages"
"##;

    #[test]
    fn find_elements_disambiguates_by_parent() {
        let matches = find_elements_in_snapshot(SNAPSHOT_MULTI_DM, "Direct Messages");
        assert_eq!(matches.len(), 2);
        // treeitem under navigation
        assert_eq!(matches[0].uid, "1_2");
        assert_eq!(matches[0].parent_role.as_deref(), Some("navigation"));
        assert_eq!(matches[0].parent_name.as_deref(), Some("Servers sidebar"));
        // heading under complementary
        assert_eq!(matches[1].uid, "1_5");
        assert_eq!(matches[1].parent_role.as_deref(), Some("complementary"));
        assert_eq!(matches[1].parent_name.as_deref(), Some("Channel sidebar"));
    }

    // --- narrow_matches ---

    const SNAPSHOT_MIXED_HOME: &str = r#"
uid=1_0 link "Home" url="https://example.com/home"
uid=1_1 button "Home"
"#;

    #[test]
    fn narrow_matches_by_role() {
        let mut matches = find_elements_in_snapshot(SNAPSHOT_MIXED_HOME, "Home");
        assert_eq!(matches.len(), 2);
        narrow_matches(&mut matches, Some("button"), None);
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].uid, "1_1");
    }

    #[test]
    fn narrow_matches_by_href() {
        let mut matches = find_elements_in_snapshot(SNAPSHOT_MIXED_HOME, "Home");
        assert_eq!(matches.len(), 2);
        narrow_matches(&mut matches, None, Some("https://example.com/home"));
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].uid, "1_0");
    }

    #[test]
    fn narrow_matches_preserves_all_when_no_match() {
        let mut matches = find_elements_in_snapshot(SNAPSHOT_MIXED_HOME, "Home");
        assert_eq!(matches.len(), 2);
        narrow_matches(&mut matches, Some("checkbox"), None);
        assert_eq!(matches.len(), 2, "should keep all if no candidate matches");
    }

    // --- narrow_by_parent ---

    #[test]
    fn narrow_by_parent_role() {
        let mut matches = find_elements_in_snapshot(SNAPSHOT_MULTI_DM, "Direct Messages");
        assert_eq!(matches.len(), 2);
        narrow_by_parent(&mut matches, Some("navigation"), None);
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].uid, "1_2");
    }

    #[test]
    fn narrow_by_parent_name() {
        let mut matches = find_elements_in_snapshot(SNAPSHOT_MULTI_DM, "Direct Messages");
        assert_eq!(matches.len(), 2);
        narrow_by_parent(&mut matches, None, Some("Channel sidebar"));
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].uid, "1_5");
    }

    #[test]
    fn narrow_by_parent_preserves_all_when_no_match() {
        let mut matches = find_elements_in_snapshot(SNAPSHOT_MULTI_DM, "Direct Messages");
        assert_eq!(matches.len(), 2);
        narrow_by_parent(&mut matches, Some("nonexistent"), None);
        assert_eq!(matches.len(), 2, "should keep all if no candidate matches");
    }
}
