use uuid::Uuid;

use super::types::{
    ActionConfidence, TargetCandidate, WalkthroughAction, WalkthroughActionKind, WalkthroughEvent,
    WalkthroughEventKind,
};

// ---------------------------------------------------------------------------
// CDP JavaScript constants
// ---------------------------------------------------------------------------

/// JavaScript click listener injected into CDP-enabled apps.
/// Captures the semantic target element on each click (capture phase,
/// fires before navigation/DOM mutation).
///
/// All state is stored on `document` (not `window`) because
/// chrome-devtools-mcp evaluates scripts in Puppeteer's utility world,
/// which has a separate `window` from the main world.  `document` is
/// shared across all JS execution contexts, so the listener, handler,
/// and click queue remain accessible regardless of which world runs the
/// injection or retrieval.
pub const CDP_CLICK_LISTENER_JS: &str = r#"() => {
  const d = document;
  d.__cw_clicks = [];
  const TAG_ROLES = {BUTTON:'button',A:'link',INPUT:'textbox',SELECT:'combobox',TEXTAREA:'textbox'};
  const INTERACTIVE = '[role="button"],[role="link"],[role="menuitem"],[role="menuitemcheckbox"],[role="menuitemradio"],[role="tab"],[role="treeitem"],[role="option"],[role="checkbox"],[role="radio"],[role="switch"],[role="textbox"],[role="combobox"],[role="searchbox"],[role="slider"],[role="spinbutton"],a,button,select,textarea,input,[tabindex]:not([tabindex="-1"])';
  function accessibleText(node) {
    const a = node.ariaLabel || node.getAttribute('aria-label');
    if (a) return a;
    const lb = node.getAttribute('aria-labelledby');
    if (lb) {
      const t = lb.split(/\s+/).map(id => document.getElementById(id)?.textContent?.trim() || '').filter(Boolean).join(' ');
      if (t) return t.substring(0, 200);
    }
    if (node.title) return node.title;
    if (node.alt) return node.alt;
    if (node.placeholder) return node.placeholder;
    if ((node.tagName === 'svg' || node.tagName === 'SVG') || (node.namespaceURI === 'http://www.w3.org/2000/svg' && node.tagName === 'svg')) {
      const st = node.querySelector('title');
      if (st?.textContent) return st.textContent.trim().substring(0, 200);
    }
    let t = '';
    for (const ch of node.childNodes) {
      if (ch.nodeType === 3) { t += ch.textContent; continue; }
      if (ch.nodeType === 1 && ch.getAttribute('aria-hidden') !== 'true') {
        const sub = accessibleText(ch);
        if (sub && t) t += ' ';
        t += sub;
      }
    }
    return t.trim().substring(0, 200);
  }
  d.__cw_handler = (e) => {
    const el = e.target.closest(INTERACTIVE) || e.target.closest('[aria-label]') || e.target;
    let text = accessibleText(el);
    if (!text) {
      let p = el.parentElement;
      while (p && p !== d.documentElement) {
        const la = p.ariaLabel || p.getAttribute('aria-label');
        if (la) { text = la; break; }
        const lb = p.getAttribute('aria-labelledby');
        if (lb) {
          const r = lb.split(/\s+/).map(id => document.getElementById(id)?.textContent?.trim() || '').filter(Boolean).join(' ');
          if (r) { text = r; break; }
        }
        if (p.title) { text = p.title; break; }
        p = p.parentElement;
      }
    }
    let parentRole = null;
    let parentName = null;
    {
      let p = el.parentElement;
      while (p && p !== d.documentElement) {
        const r = p.getAttribute('role');
        const a = p.ariaLabel || p.getAttribute('aria-label');
        if (r || a) {
          parentRole = r || null;
          parentName = a || accessibleText(p).substring(0, 200) || null;
          break;
        }
        p = p.parentElement;
      }
    }
    d.__cw_clicks.push({
      ts: Date.now(),
      tagName: el.tagName,
      role: el.getAttribute('role') || TAG_ROLES[el.tagName] || null,
      ariaLabel: el.ariaLabel || el.getAttribute('aria-label') || null,
      textContent: text || null,
      title: el.title || el.closest('[title]')?.title || null,
      value: el.value || null,
      href: el.closest('a')?.href || null,
      id: el.id || null,
      className: el.className || null,
      parentRole: parentRole,
      parentName: parentName,
    });
  };
  if (d.__cw_listener) {
    d.removeEventListener('click', d.__cw_listener, true);
  }
  d.__cw_listener = (e) => d.__cw_handler(e);
  d.addEventListener('click', d.__cw_listener, true);
}"#;

/// JavaScript to retrieve and remove the oldest click from the queue.
pub const CDP_RETRIEVE_CLICK_JS: &str = r#"() => {
  if (!Array.isArray(document.__cw_clicks)) return null;
  return document.__cw_clicks.shift() || null;
}"#;

/// JavaScript to check if the click listener is still alive; re-inject if lost.
/// Returns `"reinjected"` if it was re-injected, `"alive"` otherwise.
pub const CDP_CHECK_AND_REINJECT_JS: &str = r#"() => {
  const d = document;
  if (d.__cw_listener) return 'alive';
  d.__cw_clicks = [];
  const TAG_ROLES = {BUTTON:'button',A:'link',INPUT:'textbox',SELECT:'combobox',TEXTAREA:'textbox'};
  const INTERACTIVE = '[role="button"],[role="link"],[role="menuitem"],[role="menuitemcheckbox"],[role="menuitemradio"],[role="tab"],[role="treeitem"],[role="option"],[role="checkbox"],[role="radio"],[role="switch"],[role="textbox"],[role="combobox"],[role="searchbox"],[role="slider"],[role="spinbutton"],a,button,select,textarea,input,[tabindex]:not([tabindex="-1"])';
  function accessibleText(node) {
    const a = node.ariaLabel || node.getAttribute('aria-label');
    if (a) return a;
    const lb = node.getAttribute('aria-labelledby');
    if (lb) {
      const t = lb.split(/\s+/).map(id => document.getElementById(id)?.textContent?.trim() || '').filter(Boolean).join(' ');
      if (t) return t.substring(0, 200);
    }
    if (node.title) return node.title;
    if (node.alt) return node.alt;
    if (node.placeholder) return node.placeholder;
    if ((node.tagName === 'svg' || node.tagName === 'SVG') || (node.namespaceURI === 'http://www.w3.org/2000/svg' && node.tagName === 'svg')) {
      const st = node.querySelector('title');
      if (st?.textContent) return st.textContent.trim().substring(0, 200);
    }
    let t = '';
    for (const ch of node.childNodes) {
      if (ch.nodeType === 3) { t += ch.textContent; continue; }
      if (ch.nodeType === 1 && ch.getAttribute('aria-hidden') !== 'true') {
        const sub = accessibleText(ch);
        if (sub && t) t += ' ';
        t += sub;
      }
    }
    return t.trim().substring(0, 200);
  }
  d.__cw_handler = (e) => {
    const el = e.target.closest(INTERACTIVE) || e.target.closest('[aria-label]') || e.target;
    let text = accessibleText(el);
    if (!text) {
      let p = el.parentElement;
      while (p && p !== d.documentElement) {
        const la = p.ariaLabel || p.getAttribute('aria-label');
        if (la) { text = la; break; }
        const lb = p.getAttribute('aria-labelledby');
        if (lb) {
          const r = lb.split(/\s+/).map(id => document.getElementById(id)?.textContent?.trim() || '').filter(Boolean).join(' ');
          if (r) { text = r; break; }
        }
        if (p.title) { text = p.title; break; }
        p = p.parentElement;
      }
    }
    let parentRole = null;
    let parentName = null;
    {
      let p = el.parentElement;
      while (p && p !== d.documentElement) {
        const r = p.getAttribute('role');
        const a = p.ariaLabel || p.getAttribute('aria-label');
        if (r || a) {
          parentRole = r || null;
          parentName = a || accessibleText(p).substring(0, 200) || null;
          break;
        }
        p = p.parentElement;
      }
    }
    d.__cw_clicks.push({
      ts: Date.now(),
      tagName: el.tagName,
      role: el.getAttribute('role') || TAG_ROLES[el.tagName] || null,
      ariaLabel: el.ariaLabel || el.getAttribute('aria-label') || null,
      textContent: text || null,
      title: el.title || el.closest('[title]')?.title || null,
      value: el.value || null,
      href: el.closest('a')?.href || null,
      id: el.id || null,
      className: el.className || null,
      parentRole: parentRole,
      parentName: parentName,
    });
  };
  d.__cw_listener = (e) => d.__cw_handler(e);
  d.addEventListener('click', d.__cw_listener, true);
  return 'reinjected';
}"#;

/// JavaScript hover listener injected into CDP-enabled apps.
/// Tracks which interactive element the cursor is over using a polling
/// approach: `mousemove` updates the last-known cursor position, and a
/// 100ms `setInterval` calls `elementFromPoint` to detect element
/// transitions with dwell timing.
///
/// Uses the same `accessibleText()`, `INTERACTIVE` selector, and parent
/// traversal logic as the click listener so hover and click results are
/// directly comparable.  Pushes to `document.__cw_hovers` only when the
/// resolved element changes, recording the dwell time on the previous
/// element.
///
/// Contains a `__CW_MIN_DWELL__` placeholder that must be replaced with
/// the actual minimum dwell threshold (in milliseconds) before injection.
pub const CDP_HOVER_LISTENER_JS: &str = r#"() => {
  const d = document;
  d.__cw_hovers = [];
  d.__cw_hover_cx = 0;
  d.__cw_hover_cy = 0;
  d.__cw_hover_enter_sx = 0;
  d.__cw_hover_enter_sy = 0;
  const TAG_ROLES = {BUTTON:'button',A:'link',INPUT:'textbox',SELECT:'combobox',TEXTAREA:'textbox'};
  const INTERACTIVE = '[role="button"],[role="link"],[role="menuitem"],[role="menuitemcheckbox"],[role="menuitemradio"],[role="tab"],[role="treeitem"],[role="option"],[role="checkbox"],[role="radio"],[role="switch"],[role="textbox"],[role="combobox"],[role="searchbox"],[role="slider"],[role="spinbutton"],a,button,select,textarea,input,[tabindex]:not([tabindex="-1"])';
  function accessibleText(node) {
    const a = node.ariaLabel || node.getAttribute('aria-label');
    if (a) return a;
    const lb = node.getAttribute('aria-labelledby');
    if (lb) {
      const t = lb.split(/\s+/).map(id => document.getElementById(id)?.textContent?.trim() || '').filter(Boolean).join(' ');
      if (t) return t.substring(0, 200);
    }
    if (node.title) return node.title;
    if (node.alt) return node.alt;
    if (node.placeholder) return node.placeholder;
    if ((node.tagName === 'svg' || node.tagName === 'SVG') || (node.namespaceURI === 'http://www.w3.org/2000/svg' && node.tagName === 'svg')) {
      const st = node.querySelector('title');
      if (st?.textContent) return st.textContent.trim().substring(0, 200);
    }
    let t = '';
    for (const ch of node.childNodes) {
      if (ch.nodeType === 3) { t += ch.textContent; continue; }
      if (ch.nodeType === 1 && ch.getAttribute('aria-hidden') !== 'true') {
        const sub = accessibleText(ch);
        if (sub && t) t += ' ';
        t += sub;
      }
    }
    return t.trim().substring(0, 200);
  }
  d.__cw_hover_lastEl = null;
  d.__cw_hover_enterTime = 0;
  const MIN_DWELL = __CW_MIN_DWELL__;
  if (d.__cw_hover_mousemove) {
    d.removeEventListener('mousemove', d.__cw_hover_mousemove, true);
  }
  d.__cw_hover_mousemove = (e) => {
    d.__cw_hover_cx = e.clientX;
    d.__cw_hover_cy = e.clientY;
  };
  d.addEventListener('mousemove', d.__cw_hover_mousemove, true);
  d.__cw_hover_flush = () => {
    const el = d.__cw_hover_lastEl;
    const enter = d.__cw_hover_enterTime;
    if (!el || !enter) return;
    const now = Date.now();
    if ((now - enter) < MIN_DWELL) return;
    let text = accessibleText(el);
    if (!text) {
      let p = el.parentElement;
      while (p && p !== d.documentElement) {
        const la = p.ariaLabel || p.getAttribute('aria-label');
        if (la) { text = la; break; }
        const lb = p.getAttribute('aria-labelledby');
        if (lb) {
          const r = lb.split(/\s+/).map(id => document.getElementById(id)?.textContent?.trim() || '').filter(Boolean).join(' ');
          if (r) { text = r; break; }
        }
        if (p.title) { text = p.title; break; }
        p = p.parentElement;
      }
    }
    let parentRole = null;
    let parentName = null;
    {
      let p = el.parentElement;
      while (p && p !== d.documentElement) {
        const r = p.getAttribute('role');
        const a = p.ariaLabel || p.getAttribute('aria-label');
        if (r || a) {
          parentRole = r || null;
          parentName = a || accessibleText(p).substring(0, 200) || null;
          break;
        }
        p = p.parentElement;
      }
    }
    d.__cw_hovers.push({
      ts: enter,
      dwellMs: now - enter,
      x: d.__cw_hover_enter_sx,
      y: d.__cw_hover_enter_sy,
      tagName: el.tagName,
      role: el.getAttribute('role') || TAG_ROLES[el.tagName] || null,
      ariaLabel: el.ariaLabel || el.getAttribute('aria-label') || null,
      textContent: text || null,
      href: el.closest('a')?.href || null,
      parentRole: parentRole,
      parentName: parentName,
    });
    d.__cw_hover_lastEl = null;
    d.__cw_hover_enterTime = 0;
  };
  if (d.__cw_hover_interval) clearInterval(d.__cw_hover_interval);
  d.__cw_hover_interval = setInterval(() => {
    const raw = d.elementFromPoint(d.__cw_hover_cx, d.__cw_hover_cy);
    if (!raw) { d.__cw_hover_lastEl = null; d.__cw_hover_enterTime = 0; return; }
    const el = raw.closest(INTERACTIVE) || raw.closest('[aria-label]') || raw;
    if (el === d.__cw_hover_lastEl) return;
    d.__cw_hover_flush();
    d.__cw_hover_lastEl = el;
    d.__cw_hover_enterTime = Date.now();
    d.__cw_hover_enter_sx = d.__cw_hover_cx + window.screenX;
    d.__cw_hover_enter_sy = d.__cw_hover_cy + window.screenY;
  }, 100);
}"#;

/// JavaScript to retrieve and clear all collected hover data from the
/// injected hover listener.  Returns the full array and resets it.
pub const CDP_RETRIEVE_HOVERS_JS: &str = r#"() => {
  if (!Array.isArray(document.__cw_hovers)) return [];
  const h = document.__cw_hovers;
  document.__cw_hovers = [];
  return h;
}"#;

/// JavaScript to stop the hover listener's polling interval and remove
/// the mousemove handler, flushing any pending dwell that exceeds the
/// minimum threshold.
pub const CDP_STOP_HOVER_JS: &str = r#"() => {
  const d = document;
  if (d.__cw_hover_interval) { clearInterval(d.__cw_hover_interval); d.__cw_hover_interval = null; }
  if (d.__cw_hover_flush) { d.__cw_hover_flush(); d.__cw_hover_flush = null; }
  if (d.__cw_hover_mousemove) { d.removeEventListener('mousemove', d.__cw_hover_mousemove, true); d.__cw_hover_mousemove = null; }
}"#;

// ---------------------------------------------------------------------------
// Pure session helpers
// ---------------------------------------------------------------------------

/// Cached info about a running app, populated from MCP's `list_apps` response.
pub struct CachedApp {
    pub name: String,
    pub bundle_id: Option<String>,
}

/// Strip the last click event if it lands inside the recording bar window.
///
/// When the user clicks Stop, the event tap captures that click before shutting
/// down. This function removes that click and any events sharing its timestamp
/// (enrichment data for the stop-button click), preserving all other events
/// (e.g. VLM results for earlier clicks that were appended later).
pub fn strip_recording_bar_click(
    events: &mut Vec<WalkthroughEvent>,
    bar_rect: (f64, f64, f64, f64),
) {
    let (bar_x, bar_y, bar_w, bar_h) = bar_rect;

    let last_click_pos = events
        .iter()
        .rposition(|e| matches!(&e.kind, WalkthroughEventKind::MouseClicked { .. }));

    if let Some(idx) = last_click_pos
        && let WalkthroughEventKind::MouseClicked { x, y, .. } = &events[idx].kind
        && *x >= bar_x
        && *x <= bar_x + bar_w
        && *y >= bar_y
        && *y <= bar_y + bar_h
    {
        let click_ts = events[idx].timestamp;
        events.retain(|e| e.timestamp != click_ts);
    }
}

/// Maximum time window (ms) after a hover to look for a subsuming click.
const HOVER_CLICK_WINDOW_MS: u64 = 2000;

/// Retrieve hover candidates from HoverDetected events captured during recording.
///
/// Filters by dwell threshold and removes hovers immediately followed by a click
/// on the same location (the click subsumes the hover).
pub fn retrieve_hover_candidates(
    events: &[WalkthroughEvent],
    hover_threshold_ms: u64,
) -> Vec<WalkthroughAction> {
    let mut candidates = Vec::new();

    // Pre-collect AppFocused events sorted by timestamp so we can resolve
    // both previous and next focus for any hover, regardless of file
    // append order (hover events are written after recording stops, so
    // they appear at the end of events.jsonl, not at their chronological
    // position).
    let mut focus_events: Vec<(u64, String, Option<String>)> = events
        .iter()
        .filter_map(|e| match &e.kind {
            WalkthroughEventKind::AppFocused {
                app_name,
                window_title,
                ..
            } => Some((e.timestamp, app_name.clone(), window_title.clone())),
            _ => None,
        })
        .collect();
    focus_events.sort_by_key(|(ts, _, _)| *ts);

    // Build paused intervals so we can discard hovers that occurred while
    // recording was paused (MCP hover tracking keeps running during pause,
    // so dwell time during paused intervals produces false candidates).
    let mut paused_intervals: Vec<(u64, u64)> = Vec::new();
    let mut pause_start: Option<u64> = None;
    for e in events {
        match &e.kind {
            WalkthroughEventKind::Paused => pause_start = Some(e.timestamp),
            WalkthroughEventKind::Resumed => {
                if let Some(start) = pause_start.take() {
                    paused_intervals.push((start, e.timestamp));
                }
            }
            _ => {}
        }
    }
    // If still paused at end of events (no matching Resume), extend to u64::MAX
    if let Some(start) = pause_start {
        paused_intervals.push((start, u64::MAX));
    }

    for event in events {
        let WalkthroughEventKind::HoverDetected {
            x,
            y,
            element_name,
            element_role,
            dwell_ms,
            app_name,
        } = &event.kind
        else {
            continue;
        };

        // Subtract any paused time that overlaps with the hover span so that
        // hovers spanning or occurring during a pause don't get inflated dwell.
        let hover_end = event.timestamp + dwell_ms;
        let paused_overlap: u64 = paused_intervals
            .iter()
            .map(|(ps, pe)| {
                let os = (*ps).max(event.timestamp);
                let oe = (*pe).min(hover_end);
                oe.saturating_sub(os)
            })
            .sum();
        let effective_dwell = dwell_ms.saturating_sub(paused_overlap);

        // Filter by dwell threshold using pause-adjusted dwell.
        if effective_dwell < hover_threshold_ms {
            continue;
        }

        // Skip window-level hovers — these capture the window title (e.g.
        // "#general | DevCrew - Discord") rather than the specific element
        // the user is hovering on.  Common with Electron/Chrome apps where
        // macOS accessibility can't resolve finer-grained elements.
        if element_role.as_deref() == Some("AXWindow") {
            continue;
        }

        // Skip if any click near the same coordinates occurred shortly after
        // this hover (the click subsumes the hover intent).  Scans all events
        // because hover entries may be appended after clicks in the file.
        let click_follows = events.iter().any(|e| {
            matches!(
                &e.kind,
                WalkthroughEventKind::MouseClicked { x: cx, y: cy, .. }
                if (cx - x).abs() < 20.0 && (cy - y).abs() < 20.0
                    && e.timestamp > event.timestamp
                    && e.timestamp.saturating_sub(event.timestamp) < HOVER_CLICK_WINDOW_MS
            )
        });
        if click_follows {
            continue;
        }

        // For CDP hovers, coordinate matching doesn't work (clientX/clientY vs
        // screen coords).  Match on name+role against CdpClickResolved instead.
        // Use CdpHoverResolved presence (not app_name) to detect CDP provenance,
        // since native hovers can also carry app_name.
        let is_cdp_hover = events.iter().any(|e| {
            matches!(
                &e.kind,
                WalkthroughEventKind::CdpHoverResolved { hover_event_id, .. }
                if *hover_event_id == event.id
            )
        });
        if is_cdp_hover {
            let matches_click = events.iter().any(|e| {
                if let WalkthroughEventKind::CdpClickResolved {
                    name,
                    role: click_role,
                    ..
                } = &e.kind
                {
                    e.timestamp > event.timestamp
                        && e.timestamp.saturating_sub(event.timestamp) < HOVER_CLICK_WINDOW_MS
                        && name == element_name
                        && match (element_role, click_role) {
                            (Some(hr), Some(cr)) => hr == cr,
                            _ => true, // if either role is missing, name match is sufficient
                        }
                } else {
                    false
                }
            });
            if matches_click {
                continue;
            }
        }

        // Use explicit app_name from CDP if present; fall back to timestamp resolution.
        let (hover_app, hover_window) = if let Some(explicit_app) = app_name {
            let title = focus_events
                .iter()
                .rev()
                .find(|(_, a, _)| a == explicit_app)
                .and_then(|(_, _, t)| t.clone());
            (Some(explicit_app.clone()), title)
        } else {
            resolve_hover_app(event.timestamp, &focus_events)
        };

        let mut target_candidates = vec![];

        // Check for CDP DOM resolution for this hover event.
        let cdp_resolved = events.iter().find_map(|e| {
            if let WalkthroughEventKind::CdpHoverResolved {
                hover_event_id,
                name,
                role,
                href,
                parent_role,
                parent_name,
            } = &e.kind
                && *hover_event_id == event.id
            {
                return Some((
                    name.clone(),
                    role.clone(),
                    href.clone(),
                    parent_role.clone(),
                    parent_name.clone(),
                ));
            }
            None
        });

        if let Some((name, role, href, parent_role, parent_name)) = cdp_resolved {
            target_candidates.push(TargetCandidate::CdpElement {
                name,
                role,
                href,
                parent_role,
                parent_name,
            });
        }

        if !element_name.is_empty() {
            target_candidates.push(TargetCandidate::AccessibilityLabel {
                label: element_name.clone(),
                role: element_role.clone(),
            });
        }

        candidates.push(WalkthroughAction {
            id: Uuid::new_v4(),
            kind: WalkthroughActionKind::Hover {
                x: *x,
                y: *y,
                dwell_ms: effective_dwell,
            },
            app_name: hover_app,
            window_title: hover_window,
            target_candidates,
            artifact_paths: vec![],
            source_event_ids: vec![event.id],
            confidence: ActionConfidence::Medium,
            warnings: vec![],
            screenshot_meta: None,
            candidate: true,
        });
    }

    candidates
}

/// Determine which app a hover event belongs to.
///
/// Default: use the chronologically preceding `AppFocused` event (the app
/// that was focused when the hover occurred).  Override with the *next*
/// focus only when the hover falls within a short transition window — the
/// brief period where the cursor has entered the new app's window but the
/// PID-based focus detection hasn't fired yet.
///
/// Both lookups use the pre-collected, timestamp-sorted focus list rather
/// than depending on file append order, because hover events are written
/// to `events.jsonl` after recording stops (not at their chronological
/// position).
pub fn resolve_hover_app(
    hover_ts: u64,
    focus_events: &[(u64, String, Option<String>)],
) -> (Option<String>, Option<String>) {
    /// Maximum gap (ms) between a hover and the *next* AppFocused event for
    /// the hover to be considered a transition hover belonging to the
    /// incoming app.  Kept short so legitimate hovers near a focus change
    /// aren't misattributed.
    const TRANSITION_WINDOW_MS: u64 = 500;

    let prev = focus_events.iter().rev().find(|(ts, _, _)| *ts <= hover_ts);
    let next = focus_events.iter().find(|(ts, _, _)| *ts > hover_ts);

    match (prev, next) {
        (Some((_, papp, ptitle)), Some((nts, napp, ntitle))) => {
            let dist_next = nts - hover_ts;
            if dist_next <= TRANSITION_WINDOW_MS {
                (Some(napp.clone()), ntitle.clone())
            } else {
                (Some(papp.clone()), ptitle.clone())
            }
        }
        (Some((_, app, title)), None) => (Some(app.clone()), title.clone()),
        (None, Some((_, app, title))) => (Some(app.clone()), title.clone()),
        (None, None) => (None, None),
    }
}

/// Find the chronological insertion position for a hover candidate action
/// based on its source event timestamp, relative to existing actions.
///
/// Uses "insert after the last action at or before the hover's timestamp"
/// rather than "insert before the first action after." Hover transition
/// events fire right before the click that triggers AppFocused, so placing
/// them *after* nearby actions keeps hovers behind the Launch/Focus setup
/// they logically belong to.
pub fn find_chronological_insert_position(
    actions: &[WalkthroughAction],
    candidate: &WalkthroughAction,
    events: &[WalkthroughEvent],
) -> usize {
    let candidate_ts = candidate
        .source_event_ids
        .first()
        .and_then(|id| events.iter().find(|e| e.id == *id))
        .map(|e| e.timestamp)
        .unwrap_or(u64::MAX);

    // Find the last action whose source event timestamp is at or before the
    // candidate's, then insert after it.
    let mut insert_after: Option<usize> = None;
    for (i, action) in actions.iter().enumerate() {
        let action_ts = action
            .source_event_ids
            .first()
            .and_then(|id| events.iter().find(|e| e.id == *id))
            .map(|e| e.timestamp)
            .unwrap_or(0);
        if action_ts <= candidate_ts {
            insert_after = Some(i);
        }
    }
    insert_after.map_or(0, |i| i + 1)
}

/// Parse the `list_apps` MCP response into a map of PID to app name and bundle ID.
///
/// Input is the text content from a `list_apps` tool call. Returns a Vec of
/// `(pid, name, bundle_id)` tuples.
pub fn parse_app_list(text: &str) -> Vec<(i32, String, Option<String>)> {
    let mut results = Vec::new();
    if let Ok(apps) = serde_json::from_str::<serde_json::Value>(text)
        && let Some(arr) = apps.as_array()
    {
        for app in arr {
            if let (Some(name), Some(pid)) = (app["name"].as_str(), app["pid"].as_i64()) {
                results.push((
                    pid as i32,
                    name.to_string(),
                    app["bundle_id"].as_str().map(|s| s.to_string()),
                ));
            }
        }
    }
    results
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::AppKind;
    use crate::MouseButton;
    use uuid::Uuid;

    fn focus_event(ts: u64, app: &str) -> WalkthroughEvent {
        WalkthroughEvent {
            id: Uuid::new_v4(),
            timestamp: ts,
            kind: WalkthroughEventKind::AppFocused {
                app_name: app.to_string(),
                pid: 1,
                window_title: Some(format!("{app} Window")),
                app_kind: AppKind::Native,
            },
        }
    }

    fn hover_event(ts: u64, dwell_ms: u64) -> WalkthroughEvent {
        hover_event_with_app(ts, dwell_ms, None)
    }

    fn hover_event_with_app(ts: u64, dwell_ms: u64, app: Option<&str>) -> WalkthroughEvent {
        WalkthroughEvent {
            id: Uuid::new_v4(),
            timestamp: ts,
            kind: WalkthroughEventKind::HoverDetected {
                x: 100.0,
                y: 200.0,
                element_name: "Button".to_string(),
                element_role: Some("AXButton".to_string()),
                dwell_ms,
                app_name: app.map(|s| s.to_string()),
            },
        }
    }

    fn click_event(timestamp: u64, x: f64, y: f64) -> WalkthroughEvent {
        WalkthroughEvent {
            id: Uuid::new_v4(),
            timestamp,
            kind: WalkthroughEventKind::MouseClicked {
                x,
                y,
                button: MouseButton::Left,
                click_count: 1,
                modifiers: vec![],
            },
        }
    }

    fn stopped_event(timestamp: u64) -> WalkthroughEvent {
        WalkthroughEvent {
            id: Uuid::new_v4(),
            timestamp,
            kind: WalkthroughEventKind::Stopped,
        }
    }

    // --- strip_recording_bar_click tests ---

    #[test]
    fn strip_removes_click_inside_bar() {
        let bar = (100.0, 200.0, 300.0, 50.0);
        let mut events = vec![
            click_event(1, 50.0, 50.0),   // outside bar — keep
            click_event(2, 150.0, 220.0), // inside bar (last click)
        ];
        strip_recording_bar_click(&mut events, bar);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].timestamp, 1);
    }

    #[test]
    fn strip_keeps_click_outside_bar() {
        let bar = (100.0, 200.0, 300.0, 50.0);
        let mut events = vec![
            click_event(1, 50.0, 50.0),
            click_event(2, 50.0, 100.0), // outside bar
        ];
        strip_recording_bar_click(&mut events, bar);
        assert_eq!(events.len(), 2);
    }

    #[test]
    fn strip_noop_when_no_clicks() {
        let bar = (100.0, 200.0, 300.0, 50.0);
        let mut events = vec![stopped_event(1)];
        strip_recording_bar_click(&mut events, bar);
        assert_eq!(events.len(), 1);
    }

    #[test]
    fn strip_removes_all_events_with_same_timestamp() {
        let bar = (100.0, 200.0, 300.0, 50.0);
        let mut events = vec![
            click_event(1, 50.0, 50.0),   // different ts — keep
            click_event(2, 150.0, 220.0), // inside bar, ts=2
            stopped_event(2),             // same ts as bar click — also removed
        ];
        strip_recording_bar_click(&mut events, bar);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].timestamp, 1);
    }

    // --- retrieve_hover_candidates tests ---

    #[test]
    fn hover_during_paused_interval_is_filtered() {
        let events = vec![
            focus_event(1000, "Discord"),
            hover_event(500, 1200),
            WalkthroughEvent {
                id: Uuid::new_v4(),
                timestamp: 2000,
                kind: WalkthroughEventKind::Paused,
            },
            hover_event(3000, 2000),
            WalkthroughEvent {
                id: Uuid::new_v4(),
                timestamp: 5000,
                kind: WalkthroughEventKind::Resumed,
            },
            hover_event(6000, 2000),
        ];
        let candidates = retrieve_hover_candidates(&events, 1000);
        assert_eq!(candidates.len(), 2, "hover during pause should be filtered");
    }

    #[test]
    fn hover_spanning_pause_has_dwell_adjusted() {
        let events = vec![
            focus_event(1000, "Discord"),
            hover_event(1800, 5000),
            WalkthroughEvent {
                id: Uuid::new_v4(),
                timestamp: 2000,
                kind: WalkthroughEventKind::Paused,
            },
            WalkthroughEvent {
                id: Uuid::new_v4(),
                timestamp: 5000,
                kind: WalkthroughEventKind::Resumed,
            },
        ];
        let candidates = retrieve_hover_candidates(&events, 1000);
        assert_eq!(
            candidates.len(),
            1,
            "hover spanning pause should pass low threshold"
        );
        if let WalkthroughActionKind::Hover { dwell_ms, .. } = &candidates[0].kind {
            assert_eq!(
                *dwell_ms, 2000,
                "dwell should be adjusted for pause overlap"
            );
        } else {
            panic!("expected Hover action");
        }

        let candidates = retrieve_hover_candidates(&events, 3000);
        assert_eq!(
            candidates.len(),
            0,
            "hover spanning pause should fail high threshold"
        );
    }

    #[test]
    fn hover_after_trailing_pause_without_resume_is_filtered() {
        let events = vec![
            focus_event(1000, "Discord"),
            hover_event(1500, 2000),
            WalkthroughEvent {
                id: Uuid::new_v4(),
                timestamp: 3000,
                kind: WalkthroughEventKind::Paused,
            },
            hover_event(4000, 2000),
        ];
        let candidates = retrieve_hover_candidates(&events, 1000);
        assert_eq!(
            candidates.len(),
            1,
            "hover after trailing pause should be filtered"
        );
    }

    #[test]
    fn hover_within_transition_window_gets_next_app() {
        let events = vec![
            focus_event(1000, "Discord"),
            hover_event(1700, 1500),
            focus_event(2000, "Signal"),
        ];
        let candidates = retrieve_hover_candidates(&events, 1000);
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].app_name.as_deref(), Some("Signal"));
        assert_eq!(candidates[0].window_title.as_deref(), Some("Signal Window"));
    }

    #[test]
    fn hover_outside_transition_window_keeps_previous_app() {
        let events = vec![
            focus_event(1000, "Discord"),
            hover_event(1100, 1500),
            focus_event(2000, "Signal"),
        ];
        let candidates = retrieve_hover_candidates(&events, 1000);
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].app_name.as_deref(), Some("Discord"));
    }

    #[test]
    fn hover_with_no_next_focus_uses_previous() {
        let events = vec![focus_event(1000, "Discord"), hover_event(5000, 1500)];
        let candidates = retrieve_hover_candidates(&events, 1000);
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].app_name.as_deref(), Some("Discord"));
    }

    #[test]
    fn hover_before_any_focus_uses_next_focus() {
        let events = vec![hover_event(500, 1500), focus_event(1000, "Signal")];
        let candidates = retrieve_hover_candidates(&events, 1000);
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].app_name.as_deref(), Some("Signal"));
    }

    #[test]
    fn hover_appended_after_all_focus_events_uses_preceding_app() {
        let events = vec![
            focus_event(1000, "Discord"),
            focus_event(5000, "Signal"),
            hover_event(4800, 1500),
            hover_event(1200, 1500),
        ];
        let candidates = retrieve_hover_candidates(&events, 1000);
        assert_eq!(candidates.len(), 2);
        assert_eq!(candidates[0].app_name.as_deref(), Some("Signal"));
        assert_eq!(candidates[1].app_name.as_deref(), Some("Discord"));
    }

    #[test]
    fn hover_with_app_name_uses_it_directly() {
        let events = vec![
            focus_event(1000, "Signal"),
            hover_event_with_app(1100, 1500, Some("Discord")),
            focus_event(2000, "Discord"),
        ];
        let candidates = retrieve_hover_candidates(&events, 1000);
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].app_name.as_deref(), Some("Discord"));
    }

    #[test]
    fn native_hover_with_app_name_not_subsumed_by_cdp_click() {
        let events = vec![
            focus_event(1000, "Finder"),
            hover_event_with_app(2000, 1500, Some("Finder")),
            WalkthroughEvent {
                id: Uuid::new_v4(),
                timestamp: 3000,
                kind: WalkthroughEventKind::CdpClickResolved {
                    name: "Button".to_string(),
                    role: Some("AXButton".to_string()),
                    href: None,
                    parent_role: None,
                    parent_name: None,
                    click_event_id: Uuid::new_v4(),
                },
            },
        ];
        let candidates = retrieve_hover_candidates(&events, 1000);
        assert_eq!(
            candidates.len(),
            1,
            "native hover should not be subsumed by CDP click"
        );
    }

    #[test]
    fn hover_text_matching_next_click_filtered_out() {
        let hover_id = Uuid::new_v4();
        let events = vec![
            focus_event(1000, "App"),
            WalkthroughEvent {
                id: hover_id,
                timestamp: 2000,
                kind: WalkthroughEventKind::HoverDetected {
                    x: 0.0,
                    y: 0.0,
                    element_name: "Submit".to_string(),
                    element_role: Some("button".to_string()),
                    dwell_ms: 1200,
                    app_name: Some("App".to_string()),
                },
            },
            WalkthroughEvent {
                id: Uuid::new_v4(),
                timestamp: 2000,
                kind: WalkthroughEventKind::CdpHoverResolved {
                    hover_event_id: hover_id,
                    name: "Submit".to_string(),
                    role: Some("button".to_string()),
                    href: None,
                    parent_role: None,
                    parent_name: None,
                },
            },
            WalkthroughEvent {
                id: Uuid::new_v4(),
                timestamp: 3000,
                kind: WalkthroughEventKind::MouseClicked {
                    x: 100.0,
                    y: 200.0,
                    button: MouseButton::Left,
                    click_count: 1,
                    modifiers: vec![],
                },
            },
            WalkthroughEvent {
                id: Uuid::new_v4(),
                timestamp: 3000,
                kind: WalkthroughEventKind::CdpClickResolved {
                    name: "Submit".to_string(),
                    role: Some("button".to_string()),
                    href: None,
                    parent_role: None,
                    parent_name: None,
                    click_event_id: Uuid::new_v4(),
                },
            },
        ];
        let candidates = retrieve_hover_candidates(&events, 1000);
        assert_eq!(
            candidates.len(),
            0,
            "hover matching next click target should be filtered"
        );
    }

    // --- parse_app_list tests ---

    #[test]
    fn parse_app_list_valid() {
        let text = r#"[{"name":"Discord","pid":123,"bundle_id":"com.hnc.Discord"},{"name":"Chrome","pid":456}]"#;
        let result = parse_app_list(text);
        assert_eq!(result.len(), 2);
        assert_eq!(
            result[0],
            (
                123,
                "Discord".to_string(),
                Some("com.hnc.Discord".to_string())
            )
        );
        assert_eq!(result[1], (456, "Chrome".to_string(), None));
    }

    #[test]
    fn parse_app_list_empty() {
        assert!(parse_app_list("[]").is_empty());
    }

    #[test]
    fn parse_app_list_invalid_json() {
        assert!(parse_app_list("not json").is_empty());
    }
}
