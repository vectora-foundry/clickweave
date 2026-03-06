/// MCP server name for the Chrome DevTools Protocol server.
pub const CDP_SERVER: &str = "chrome-devtools";

/// Parse a CDP snapshot and find elements whose text contains the target.
/// Returns a vec of (uid, matched_line) tuples.
///
/// The snapshot from chrome-devtools `take_snapshot` is a text representation
/// of the accessibility tree where each element has a UID. Format:
/// ```text
/// [uid="e1"] button "Submit"
/// [uid="e2"] link "Friends"
/// ```
pub fn find_elements_in_snapshot(snapshot_text: &str, target: &str) -> Vec<(String, String)> {
    let target_lower = target.to_lowercase();
    let mut matches = Vec::new();
    for line in snapshot_text.lines() {
        if let Some(uid_start) = line.find("uid=\"") {
            let uid_rest = &line[uid_start + 5..];
            if let Some(uid_end) = uid_rest.find('"') {
                let uid = &uid_rest[..uid_end];
                if line.to_lowercase().contains(&target_lower) {
                    let label = extract_label(line);
                    matches.push((uid.to_string(), label));
                }
            }
        }
    }
    matches
}

/// Extract the visible label text from a snapshot line.
///
/// Lines have the format `[uid="e1"] button "Submit"` — we extract
/// the last quoted string as the human-readable label. Falls back to
/// the trimmed line if no quoted string is found.
fn extract_label(line: &str) -> String {
    // Find the last quoted string in the line.
    if let Some(last_quote_end) = line.rfind('"') {
        let before = &line[..last_quote_end];
        if let Some(last_quote_start) = before.rfind('"') {
            let label = &line[last_quote_start + 1..last_quote_end];
            if !label.is_empty() {
                return label.to_string();
            }
        }
    }
    line.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::find_elements_in_snapshot;

    const SNAPSHOT: &str = r#"
[uid="e1"] button "Submit"
[uid="e2"] link "Friends"
[uid="e3"] heading "Settings"
[uid="e4"] button "Submit Form"
"#;

    #[test]
    fn single_match() {
        let matches = find_elements_in_snapshot(SNAPSHOT, "Friends");
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].0, "e2");
        assert_eq!(matches[0].1, "Friends");
    }

    #[test]
    fn multiple_matches() {
        let matches = find_elements_in_snapshot(SNAPSHOT, "Submit");
        assert_eq!(matches.len(), 2);
        assert_eq!(matches[0].0, "e1");
        assert_eq!(matches[0].1, "Submit");
        assert_eq!(matches[1].0, "e4");
        assert_eq!(matches[1].1, "Submit Form");
    }

    #[test]
    fn case_insensitive() {
        let matches = find_elements_in_snapshot(SNAPSHOT, "settings");
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].0, "e3");
        assert_eq!(matches[0].1, "Settings");
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
