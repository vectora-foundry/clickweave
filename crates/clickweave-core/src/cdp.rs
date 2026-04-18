/// Parsed response from upstream `cdp_find_elements`.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct CdpFindElementsResponse {
    #[serde(default)]
    pub page_url: String,
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub matches: Vec<CdpFindElementMatch>,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct CdpFindElementMatch {
    pub uid: String,
    #[serde(default)]
    pub role: String,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub tag: String,
    #[serde(default)]
    pub disabled: bool,
    #[serde(default)]
    pub parent_role: Option<String>,
    #[serde(default)]
    pub parent_name: Option<String>,
}

/// Pick a random port in the ephemeral range (49152-65535).
///
/// Backed by the thread-local CSPRNG in `rand`, so two calls on the same
/// sub-second boundary do not collide the way a nanosecond-seeded LCG
/// would. The range is inclusive on both ends.
pub fn rand_ephemeral_port() -> u16 {
    use rand::Rng;
    rand::rng().random_range(49152..=65535)
}

/// A single page entry parsed from `cdp_list_pages` output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CdpPageEntry {
    pub index: usize,
    pub url: String,
    pub selected: bool,
}

/// Parse the text body of a `cdp_list_pages` MCP tool response into page entries.
///
/// Expected format (from `native-devtools-mcp`):
/// ```text
/// Pages (N total):
///   [0] https://example.com/
///   [1]* https://other.example.com/
/// ```
/// The ` *` suffix marks the currently-selected page. Lines that don't match
/// the `[index] url` shape are ignored, so the parser tolerates header lines
/// and future additions.
pub fn parse_cdp_page_list(text: &str) -> Vec<CdpPageEntry> {
    let mut out = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim_start();
        if !trimmed.starts_with('[') {
            continue;
        }
        let Some(end) = trimmed.find(']') else {
            continue;
        };
        let Ok(index) = trimmed[1..end].parse::<usize>() else {
            continue;
        };
        let rest = trimmed[end + 1..].trim_start();
        let (selected, rest) = match rest.strip_prefix('*') {
            Some(r) => (true, r.trim_start()),
            None => (false, rest),
        };
        if rest.is_empty() {
            continue;
        }
        out.push(CdpPageEntry {
            index,
            url: rest.to_string(),
            selected,
        });
    }
    out
}

/// Return the URL we should remember as the "currently-selected tab" for a
/// future reconnect. Prefers the page marked `selected = true`; falls back
/// to the first entry so that a restored session still has an anchor URL
/// when the list lacks an explicit selection marker. Empty URLs are
/// discarded.
pub fn current_selected_page_url(pages: &[CdpPageEntry]) -> Option<String> {
    pages
        .iter()
        .find(|p| p.selected)
        .or_else(|| pages.first())
        .map(|p| p.url.clone())
        .filter(|u| !u.is_empty())
}

/// Pick the best page index to restore, given a list of currently-available
/// pages and a URL we remembered from the prior session with this app.
///
/// Match strategy (most-specific to least):
/// 1. Exact URL match.
/// 2. Origin + path match (drops query + fragment) — survives "?foo=bar" noise.
/// 3. Origin match (scheme + host + port) — survives path-only navigations
///    inside an SPA or site.
///
/// Returns `None` if `remembered_url` is empty or no page matches. Callers
/// should fall back to whatever `cdp_connect` auto-selected (the first
/// non-extension page) when this returns `None`.
///
/// The function is pure — no MCP calls, no network, no filesystem. It exists
/// specifically so the selection logic can be unit-tested with synthetic
/// inputs.
pub fn pick_page_index_for_url(pages: &[CdpPageEntry], remembered_url: &str) -> Option<usize> {
    let remembered = remembered_url.trim();
    if remembered.is_empty() || pages.is_empty() {
        return None;
    }

    // 1. Exact URL match.
    if let Some(hit) = pages.iter().find(|p| p.url == remembered) {
        return Some(hit.index);
    }

    // 2. Origin + path match.
    let remembered_op = origin_and_path(remembered);
    if !remembered_op.is_empty()
        && let Some(hit) = pages
            .iter()
            .find(|p| origin_and_path(&p.url) == remembered_op)
    {
        return Some(hit.index);
    }

    // 3. Origin-only match.
    let remembered_origin = origin_of(remembered);
    if !remembered_origin.is_empty()
        && let Some(hit) = pages
            .iter()
            .find(|p| origin_of(&p.url) == remembered_origin)
    {
        return Some(hit.index);
    }

    None
}

/// Extract the origin part of a URL: everything up to (but not including) the
/// path. For `https://example.com:8080/foo?bar#baz` returns
/// `https://example.com:8080`.
///
/// Returns an empty string for URLs without a recognised scheme (`scheme://`).
fn origin_of(url: &str) -> String {
    let Some(scheme_end) = url.find("://") else {
        return String::new();
    };
    let after_scheme = scheme_end + 3;
    let host_end = url[after_scheme..]
        .find(['/', '?', '#'])
        .map(|p| after_scheme + p)
        .unwrap_or(url.len());
    url[..host_end].to_string()
}

/// Extract origin + path — drops query string and fragment.
fn origin_and_path(url: &str) -> String {
    let Some(scheme_end) = url.find("://") else {
        return String::new();
    };
    let after_scheme = scheme_end + 3;
    // The path starts at the first '/' after the host (if any).
    let path_start = url[after_scheme..]
        .find('/')
        .map(|p| after_scheme + p)
        .unwrap_or(url.len());
    // Cut off at the first '?' or '#'.
    let tail_cut = url[path_start..]
        .find(['?', '#'])
        .map(|p| path_start + p)
        .unwrap_or(url.len());
    url[..tail_cut].to_string()
}

#[cfg(test)]
mod page_selection_tests {
    use super::*;

    fn page(index: usize, url: &str) -> CdpPageEntry {
        CdpPageEntry {
            index,
            url: url.to_string(),
            selected: false,
        }
    }

    #[test]
    fn parse_cdp_page_list_reads_standard_output() {
        let text =
            "Pages (2 total):\n  [0] https://a.example.com/\n  [1]* https://b.example.com/foo\n";
        let pages = parse_cdp_page_list(text);
        assert_eq!(pages.len(), 2);
        assert_eq!(pages[0].index, 0);
        assert_eq!(pages[0].url, "https://a.example.com/");
        assert!(!pages[0].selected);
        assert_eq!(pages[1].index, 1);
        assert_eq!(pages[1].url, "https://b.example.com/foo");
        assert!(pages[1].selected);
    }

    #[test]
    fn parse_cdp_page_list_ignores_non_matching_lines() {
        let text = "Pages (1 total):\n  [0] https://example.com/\nsome trailing noise\n";
        let pages = parse_cdp_page_list(text);
        assert_eq!(pages.len(), 1);
        assert_eq!(pages[0].url, "https://example.com/");
    }

    #[test]
    fn parse_cdp_page_list_handles_empty_output() {
        let pages = parse_cdp_page_list("");
        assert!(pages.is_empty());
    }

    #[test]
    fn parse_cdp_page_list_accepts_no_space_after_marker() {
        // `[1]*https://...` (no space) should still parse selected=true.
        let pages = parse_cdp_page_list("  [1]*https://example.com/");
        assert_eq!(pages.len(), 1);
        assert!(pages[0].selected);
        assert_eq!(pages[0].url, "https://example.com/");
    }

    #[test]
    fn pick_page_returns_none_for_empty_remembered() {
        let pages = vec![page(0, "https://a.com/")];
        assert_eq!(pick_page_index_for_url(&pages, ""), None);
        assert_eq!(pick_page_index_for_url(&pages, "   "), None);
    }

    #[test]
    fn pick_page_returns_none_for_empty_pages() {
        assert_eq!(pick_page_index_for_url(&[], "https://a.com/"), None);
    }

    #[test]
    fn pick_page_prefers_exact_match() {
        let pages = vec![
            page(0, "https://a.example.com/"),
            page(1, "https://b.example.com/foo"),
        ];
        assert_eq!(
            pick_page_index_for_url(&pages, "https://b.example.com/foo"),
            Some(1)
        );
    }

    #[test]
    fn pick_page_matches_origin_and_path_ignoring_query_and_fragment() {
        // Remembered URL has a query string; current page has a different one.
        let pages = vec![
            page(0, "https://a.example.com/?other=1"),
            page(1, "https://b.example.com/foo?session=xyz#frag"),
        ];
        assert_eq!(
            pick_page_index_for_url(&pages, "https://b.example.com/foo?session=abc"),
            Some(1)
        );
    }

    #[test]
    fn pick_page_falls_back_to_origin_only_when_path_differs() {
        // Remembered a specific path; SPA navigated to a different path same origin.
        let pages = vec![
            page(0, "https://a.example.com/"),
            page(1, "https://app.example.com/dashboard/new"),
        ];
        assert_eq!(
            pick_page_index_for_url(&pages, "https://app.example.com/settings"),
            Some(1)
        );
    }

    #[test]
    fn pick_page_returns_none_when_no_match() {
        let pages = vec![
            page(0, "https://a.example.com/"),
            page(1, "https://b.example.com/"),
        ];
        assert_eq!(
            pick_page_index_for_url(&pages, "https://c.example.com/"),
            None
        );
    }

    #[test]
    fn pick_page_handles_urls_without_scheme_safely() {
        // Unusual but not crash-worthy: "about:blank" has no `scheme://`,
        // so fallback-layer matches collapse to empty-string checks that
        // are skipped. Only exact match applies.
        let pages = vec![page(0, "about:blank"), page(1, "https://example.com/")];
        assert_eq!(pick_page_index_for_url(&pages, "about:blank"), Some(0));
        // A bare path won't match any scheme'd URL via origin.
        assert_eq!(pick_page_index_for_url(&pages, "/foo"), None);
    }

    #[test]
    fn pick_page_respects_entry_index_not_list_position() {
        // Entries may arrive out of order (parsed from map). The returned
        // index must come from the entry itself.
        let pages = vec![page(5, "https://a.com/"), page(2, "https://b.com/")];
        assert_eq!(pick_page_index_for_url(&pages, "https://b.com/"), Some(2));
    }

    #[test]
    fn current_selected_url_prefers_selected_marker() {
        let pages = vec![
            CdpPageEntry {
                index: 0,
                url: "https://a.com/".to_string(),
                selected: false,
            },
            CdpPageEntry {
                index: 1,
                url: "https://b.com/".to_string(),
                selected: true,
            },
        ];
        assert_eq!(
            current_selected_page_url(&pages),
            Some("https://b.com/".to_string())
        );
    }

    #[test]
    fn current_selected_url_falls_back_to_first_when_none_marked() {
        let pages = vec![
            CdpPageEntry {
                index: 0,
                url: "https://a.com/".to_string(),
                selected: false,
            },
            CdpPageEntry {
                index: 1,
                url: "https://b.com/".to_string(),
                selected: false,
            },
        ];
        assert_eq!(
            current_selected_page_url(&pages),
            Some("https://a.com/".to_string())
        );
    }

    #[test]
    fn current_selected_url_returns_none_for_empty_list() {
        assert_eq!(current_selected_page_url(&[]), None);
    }

    #[test]
    fn current_selected_url_skips_empty_url() {
        let pages = vec![CdpPageEntry {
            index: 0,
            url: String::new(),
            selected: true,
        }];
        assert_eq!(current_selected_page_url(&pages), None);
    }

    #[test]
    fn origin_of_strips_path_query_fragment() {
        assert_eq!(origin_of("https://a.com/foo?x=1#z"), "https://a.com");
        assert_eq!(origin_of("https://a.com:8080/foo"), "https://a.com:8080");
        assert_eq!(origin_of("no-scheme.example"), "");
    }

    #[test]
    fn rand_ephemeral_port_is_in_range() {
        for _ in 0..256 {
            let port = rand_ephemeral_port();
            assert!(
                (49152..=65535).contains(&port),
                "port {port} outside ephemeral range"
            );
        }
    }

    #[test]
    fn origin_and_path_strips_query_and_fragment() {
        assert_eq!(
            origin_and_path("https://a.com/foo/bar?x=1#z"),
            "https://a.com/foo/bar"
        );
        assert_eq!(origin_and_path("https://a.com"), "https://a.com");
        assert_eq!(origin_and_path("no-scheme.example"), "");
    }
}
