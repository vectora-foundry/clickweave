use clickweave_core::cdp::CdpFindElementMatch;
use std::collections::BTreeSet;

/// Generate a stable fingerprint for a single element.
///
/// The fingerprint includes the element's `uid`, `role`, `label`, `tag`, and
/// parent info so that elements with the same visual description but different
/// DOM identities produce different fingerprints.
pub fn element_fingerprint(el: &CdpFindElementMatch) -> String {
    let parent = match (&el.parent_role, &el.parent_name) {
        (Some(role), Some(name)) => format!("{}:{}", role, name),
        (Some(role), None) => role.clone(),
        _ => String::new(),
    };
    format!("{}|{}|{}|{}|{}", el.uid, el.role, el.label, el.tag, parent)
}

/// Detect whether a page transition occurred between two observations.
///
/// A transition is detected when the element sets differ significantly.
/// Returns `true` if more than 50% of elements changed between the two
/// observations, indicating a navigation or page load.
pub fn detect_transition(
    prev_elements: &[CdpFindElementMatch],
    curr_elements: &[CdpFindElementMatch],
) -> bool {
    if prev_elements.is_empty() && curr_elements.is_empty() {
        return false;
    }
    if prev_elements.is_empty() || curr_elements.is_empty() {
        return true;
    }

    let prev_fps: BTreeSet<String> = prev_elements.iter().map(element_fingerprint).collect();
    let curr_fps: BTreeSet<String> = curr_elements.iter().map(element_fingerprint).collect();

    let intersection_count = prev_fps.intersection(&curr_fps).count();
    let union_count = prev_fps.union(&curr_fps).count();

    if union_count == 0 {
        return false;
    }

    // Jaccard similarity < 0.5 means more than half of elements changed
    let similarity = intersection_count as f64 / union_count as f64;
    similarity < 0.5
}

/// Generate a combined fingerprint for an entire page of elements.
///
/// This is used as a cache key for decision caching — if the page
/// fingerprint matches, we can reuse a previous decision.
pub fn page_fingerprint(elements: &[CdpFindElementMatch]) -> String {
    let mut fps: Vec<String> = elements.iter().map(element_fingerprint).collect();
    fps.sort();
    fps.join(";")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_element(uid: &str, role: &str, label: &str, tag: &str) -> CdpFindElementMatch {
        CdpFindElementMatch {
            uid: uid.to_string(),
            role: role.to_string(),
            label: label.to_string(),
            tag: tag.to_string(),
            disabled: false,
            parent_role: None,
            parent_name: None,
        }
    }

    #[test]
    fn element_fingerprint_is_deterministic() {
        let el = make_element("1_0", "button", "Submit", "button");
        let fp1 = element_fingerprint(&el);
        let fp2 = element_fingerprint(&el);
        assert_eq!(fp1, fp2);
        assert!(fp1.contains("button"));
        assert!(fp1.contains("Submit"));
    }

    #[test]
    fn element_fingerprint_includes_parent() {
        let el = CdpFindElementMatch {
            uid: "1_0".to_string(),
            role: "button".to_string(),
            label: "Submit".to_string(),
            tag: "button".to_string(),
            disabled: false,
            parent_role: Some("form".to_string()),
            parent_name: Some("Login".to_string()),
        };
        let fp = element_fingerprint(&el);
        assert!(fp.contains("form:Login"));
    }

    #[test]
    fn detect_transition_same_elements() {
        let elements = vec![
            make_element("1_0", "button", "Submit", "button"),
            make_element("1_1", "textbox", "Email", "input"),
        ];
        assert!(!detect_transition(&elements, &elements));
    }

    #[test]
    fn detect_transition_completely_different() {
        let prev = vec![
            make_element("1_0", "button", "Submit", "button"),
            make_element("1_1", "textbox", "Email", "input"),
        ];
        let curr = vec![
            make_element("2_0", "heading", "Dashboard", "h1"),
            make_element("2_1", "link", "Settings", "a"),
        ];
        assert!(detect_transition(&prev, &curr));
    }

    #[test]
    fn detect_transition_empty_to_elements() {
        let elements = vec![make_element("1_0", "button", "Submit", "button")];
        assert!(detect_transition(&[], &elements));
    }

    #[test]
    fn detect_transition_both_empty() {
        assert!(!detect_transition(&[], &[]));
    }

    #[test]
    fn detect_transition_partial_overlap() {
        // 2 out of 3 elements are the same — should NOT be a transition (67% overlap)
        let prev = vec![
            make_element("1_0", "button", "Submit", "button"),
            make_element("1_1", "textbox", "Email", "input"),
            make_element("1_2", "link", "Forgot Password", "a"),
        ];
        let curr = vec![
            make_element("1_0", "button", "Submit", "button"),
            make_element("1_1", "textbox", "Email", "input"),
            make_element("1_3", "textbox", "Password", "input"),
        ];
        // 2 shared out of 4 unique = 50% similarity → not a transition (threshold is < 0.5)
        assert!(!detect_transition(&prev, &curr));
    }

    #[test]
    fn page_fingerprint_is_order_independent() {
        let elements_a = vec![
            make_element("1_0", "button", "Submit", "button"),
            make_element("1_1", "textbox", "Email", "input"),
        ];
        let elements_b = vec![
            make_element("1_1", "textbox", "Email", "input"),
            make_element("1_0", "button", "Submit", "button"),
        ];
        assert_eq!(page_fingerprint(&elements_a), page_fingerprint(&elements_b));
    }

    #[test]
    fn page_fingerprint_differs_for_different_pages() {
        let page_a = vec![make_element("1_0", "button", "Submit", "button")];
        let page_b = vec![make_element("2_0", "heading", "Dashboard", "h1")];
        assert_ne!(page_fingerprint(&page_a), page_fingerprint(&page_b));
    }

    #[test]
    fn element_fingerprint_differs_by_uid() {
        // Two elements with the same role, label, and tag but different uids
        // should produce different fingerprints.
        let el_a = make_element("1_0", "button", "Submit", "button");
        let el_b = make_element("2_5", "button", "Submit", "button");
        assert_ne!(
            element_fingerprint(&el_a),
            element_fingerprint(&el_b),
            "Elements with different uids should have different fingerprints"
        );
    }
}
