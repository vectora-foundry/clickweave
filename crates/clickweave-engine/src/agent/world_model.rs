#![allow(dead_code)] // Phase 1: module wired to its own tests only; runtime consumers land in later phases.

use serde::Serialize;

use clickweave_core::cdp::CdpFindElementMatch;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FreshnessSource {
    DirectObservation,
    InferredFromEvent,
    CarriedOver,
}

#[derive(Debug, Clone, Serialize)]
pub struct Fresh<T> {
    pub value: T,
    pub written_at: usize,
    pub source: FreshnessSource,
    pub ttl_steps: Option<u32>,
}

/// Classification for the currently-focused app. Mirrors the existing
/// `AppKind` classification used by the runner — will unify in Phase 4.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "snake_case")]
pub enum AppKind {
    Native,
    ElectronApp,
    ChromeBrowser,
}

#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct FocusedApp {
    pub name: String,
    pub kind: AppKind,
    pub pid: i32,
}

#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct WindowRef {
    pub app_name: String,
    pub title: String,
    pub pid: i32,
}

#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct CdpPageState {
    pub url: String,
    pub page_fingerprint: String,
}

/// Source-agnostic observed element (D16).
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "source", rename_all = "snake_case")]
pub enum ObservedElement {
    Cdp(CdpFindElementMatch),
    Ax(AxElement),
    Ocr(OcrMatch),
}

/// Parsed AX element from native `take_ax_snapshot` text output.
#[derive(Debug, Clone, Serialize)]
pub struct AxElement {
    pub uid: String, // e.g. "a42g3"
    pub role: String,
    pub name: Option<String>,
    pub value: Option<String>,
    pub depth: u32,
    pub focused: bool,
    pub disabled: bool,
    /// Name of the nearest named ancestor (by indentation depth). Used by
    /// `StateRunner::enrich_ax_descriptor` to rewrite raw AX uids into
    /// replay-stable `AxTarget::Descriptor` payloads where the parent
    /// anchor disambiguates common (role, name) pairs such as outline rows.
    pub parent_name: Option<String>,
}

/// Parsed OCR match from `find_text` responses.
#[derive(Debug, Clone, Serialize)]
pub struct OcrMatch {
    pub text: String,
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
    pub confidence: f32,
}

#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct ScreenshotRef {
    pub screenshot_id: String,
    pub captured_at_step: usize,
}

/// Full AX tree body + identity metadata (D15). Native `take_ax_snapshot` only.
#[derive(Debug, Clone, Serialize)]
pub struct AxSnapshotData {
    pub snapshot_id: String,
    pub element_count: usize,
    pub captured_at_step: usize,
    pub ax_tree_text: String,
}

#[derive(Debug, Clone, Default, Serialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct UncertaintyScore {
    pub score: f32, // 0.0 .. 1.0
    pub reasons: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct WorldModel {
    pub focused_app: Option<Fresh<FocusedApp>>,
    pub window_list: Option<Fresh<Vec<WindowRef>>>,
    pub cdp_page: Option<Fresh<CdpPageState>>,
    pub elements: Option<Fresh<Vec<ObservedElement>>>,
    pub modal_present: Option<Fresh<bool>>,
    pub dialog_present: Option<Fresh<bool>>,
    pub last_screenshot: Option<Fresh<ScreenshotRef>>,
    pub last_native_ax_snapshot: Option<Fresh<AxSnapshotData>>,
    pub uncertainty: UncertaintyScore,
}

#[derive(Debug, Clone)]
pub enum InvalidationEvent {
    FocusChanging { tool: String },
    CdpNavigation { new_url: String },
    AppLifecycle { tool: String },
    ToolFailed { tool: String },
    SnapshotStale { age_steps: u32 },
}

/// Signals passed into `WorldModel::recompute_uncertainty`. Collected by
/// the runner before each observe phase (D14).
#[derive(Debug, Clone, Copy)]
pub struct UncertaintySignals {
    pub consecutive_errors: usize,
    pub refuted_hypotheses: usize,
    pub modal_dialog_mismatch: bool,
}

impl WorldModel {
    /// Recompute the uncertainty score from signal set + current world
    /// model state per design-doc decision D14. Inputs: number of
    /// invalid (unobserved or invalidated-pending-refresh) fields,
    /// consecutive errors, refuted-hypothesis count, and modal/dialog
    /// target mismatch. Weights are intentionally conservative; tune
    /// later once the metric is observed in real runs.
    ///
    /// Invalid-field contribution matches POMDP belief-state semantics:
    /// absent observations are epistemic uncertainty, so a fresh
    /// `WorldModel::default()` starts nonzero — the agent is maximally
    /// uncertain before it looks. Spec 1 only surfaces uncertainty via
    /// the state block; gating dispatch on it is deferred to Spec 2
    /// (design doc, out-of-scope section).
    pub fn recompute_uncertainty(&mut self, signals: UncertaintySignals) {
        let mut score: f32 = 0.0;
        let mut reasons: Vec<String> = Vec::new();

        let invalid = self.invalid_field_count();
        if invalid > 0 {
            score += (invalid as f32) * 0.05;
            reasons.push(format!("{invalid} invalid field(s)"));
        }
        if signals.consecutive_errors > 0 {
            score += (signals.consecutive_errors as f32) * 0.15;
            reasons.push(format!("{} consecutive errors", signals.consecutive_errors));
        }
        if signals.refuted_hypotheses > 0 {
            score += (signals.refuted_hypotheses as f32) * 0.05;
            reasons.push(format!("{} refuted hypotheses", signals.refuted_hypotheses));
        }
        if signals.modal_dialog_mismatch {
            score += 0.3;
            reasons.push("modal/dialog target mismatch".to_string());
        }

        self.uncertainty.score = score.min(1.0);
        self.uncertainty.reasons = reasons;
    }

    /// Per-field presence + freshness signature used by the runner to
    /// compute a `WorldModelDiff` across an observe boundary (D17).
    ///
    /// Each entry is `(field_name, signature)`. The signature is
    /// `Some(written_at)` when the slot is populated — `written_at`
    /// monotonically advances on every write, so a change (including
    /// a same-value re-write) flips the signature. `None` encodes an
    /// absent slot. The `uncertainty` score is not Fresh-wrapped; we
    /// hash its reason count + discretised score so a recompute that
    /// shifts either surfaces as a diff.
    ///
    /// Pure and cheap: zero allocation beyond the returned vec. The
    /// runner snapshots signatures pre-observe, applies events, then
    /// re-snapshots and diffs.
    pub fn field_signatures(&self) -> Vec<(&'static str, Option<usize>)> {
        vec![
            (
                "focused_app",
                self.focused_app.as_ref().map(|f| f.written_at),
            ),
            (
                "window_list",
                self.window_list.as_ref().map(|f| f.written_at),
            ),
            ("cdp_page", self.cdp_page.as_ref().map(|f| f.written_at)),
            ("elements", self.elements.as_ref().map(|f| f.written_at)),
            (
                "modal_present",
                self.modal_present.as_ref().map(|f| f.written_at),
            ),
            (
                "dialog_present",
                self.dialog_present.as_ref().map(|f| f.written_at),
            ),
            (
                "last_screenshot",
                self.last_screenshot.as_ref().map(|f| f.written_at),
            ),
            (
                "last_native_ax_snapshot",
                self.last_native_ax_snapshot.as_ref().map(|f| f.written_at),
            ),
            (
                "uncertainty",
                Some(uncertainty_signature(&self.uncertainty)),
            ),
        ]
    }

    fn invalid_field_count(&self) -> usize {
        [
            self.focused_app.is_none(),
            self.window_list.is_none(),
            self.cdp_page.is_none(),
            self.elements.is_none(),
            self.modal_present.is_none(),
            self.dialog_present.is_none(),
            self.last_screenshot.is_none(),
            self.last_native_ax_snapshot.is_none(),
        ]
        .iter()
        .filter(|present| **present)
        .count()
    }

    pub fn apply_events(&mut self, events: Vec<InvalidationEvent>) {
        for e in events {
            match e {
                InvalidationEvent::FocusChanging { .. }
                | InvalidationEvent::AppLifecycle { .. } => {
                    self.focused_app = None;
                    self.window_list = None;
                    self.elements = None;
                    self.modal_present = None;
                    self.dialog_present = None;
                    // Screenshots and AX snapshots are app-bound; invalidate.
                    self.last_screenshot = None;
                    self.last_native_ax_snapshot = None;
                }
                InvalidationEvent::CdpNavigation { .. } => {
                    self.cdp_page = None;
                    self.elements = None;
                    self.modal_present = None;
                    self.dialog_present = None;
                }
                InvalidationEvent::ToolFailed { tool } => {
                    self.uncertainty.score = (self.uncertainty.score + 0.15).min(1.0);
                    self.uncertainty
                        .reasons
                        .push(format!("tool_failed: {}", tool));
                }
                InvalidationEvent::SnapshotStale { age_steps } => {
                    if let Some(ref ax) = self.last_native_ax_snapshot
                        && let Some(ttl) = ax.ttl_steps
                        && age_steps > ttl
                    {
                        self.last_native_ax_snapshot = None;
                    }
                    if let Some(ref s) = self.last_screenshot
                        && let Some(ttl) = s.ttl_steps
                        && age_steps > ttl
                    {
                        self.last_screenshot = None;
                    }
                }
            }
        }
    }
}

/// Discretised single-`usize` signature of an `UncertaintyScore`, used by
/// `WorldModel::field_signatures` so a recompute that shifts the score
/// bucket or the reasons list shows up as a diff. `score` is bucketed
/// in 1% increments (rounded) and xor'd with the reason count so both
/// axes of the score participate.
fn uncertainty_signature(u: &UncertaintyScore) -> usize {
    let bucket = (u.score.clamp(0.0, 1.0) * 100.0) as usize;
    bucket ^ (u.reasons.len() << 16)
}

/// Minimal MCP surface the world model's refresh path needs. Implemented by
/// the runner against the real `McpClient`; stubbed in unit tests.
#[async_trait::async_trait]
pub trait WorldModelObserver: Send + Sync {
    /// Run an MCP observation tool and return its text body. `Ok(None)` when
    /// the tool is not available in the current MCP session (e.g. CDP not
    /// attached, AX permissions denied). `Err` for hard failures.
    async fn observe(
        &self,
        tool_name: &str,
        args: serde_json::Value,
    ) -> Result<Option<String>, String>;
}

impl WorldModel {
    /// Re-fetch invalid fields via the observer. Never fails hard: partial
    /// failures elevate uncertainty but leave other fields reachable.
    ///
    /// Tool selection:
    /// - `elements`: try `cdp_find_elements` first; fall back to
    ///   `take_ax_snapshot` when the observer returns `Ok(None)`.
    /// - `focused_app`: parse from `list_apps` (the row with `focused=true`).
    /// - `window_list`: parse from `list_windows`.
    /// - `modal_present` / `dialog_present` are inferred by the runner at
    ///   dispatch time, not refreshed directly here.
    pub async fn refresh_invalid_fields<O: WorldModelObserver + ?Sized>(
        &mut self,
        obs: &O,
        step_index: usize,
    ) -> Result<(), String> {
        // P2.H1: try CDP first; fall back to AX when CDP is not attached in
        // this MCP session (observer returns Ok(None)).
        if self.elements.is_none() {
            let try_result = obs
                .observe(
                    "cdp_find_elements",
                    serde_json::json!({ "query": "", "max_results": 300 }),
                )
                .await;
            match try_result {
                Ok(Some(body)) => match serde_json::from_str::<
                    clickweave_core::cdp::CdpFindElementsResponse,
                >(&body)
                {
                    Ok(resp) => {
                        let els: Vec<ObservedElement> =
                            resp.matches.into_iter().map(ObservedElement::Cdp).collect();
                        self.elements = Some(Fresh {
                            value: els,
                            written_at: step_index,
                            source: FreshnessSource::DirectObservation,
                            ttl_steps: Some(2),
                        });
                    }
                    Err(_) => {
                        self.uncertainty.score = (self.uncertainty.score + 0.1).min(1.0);
                        self.uncertainty
                            .reasons
                            .push("cdp_find_elements parse failed".to_string());
                    }
                },
                Ok(None) => {
                    // CDP unavailable — try AX.
                    match obs.observe("take_ax_snapshot", serde_json::json!({})).await {
                        Ok(Some(body)) => {
                            let parsed = parse_ax_snapshot(&body);
                            let els: Vec<ObservedElement> =
                                parsed.into_iter().map(ObservedElement::Ax).collect();
                            self.elements = Some(Fresh {
                                value: els,
                                written_at: step_index,
                                source: FreshnessSource::DirectObservation,
                                ttl_steps: Some(2),
                            });
                        }
                        Ok(None) => {
                            self.uncertainty.score = (self.uncertainty.score + 0.1).min(1.0);
                            self.uncertainty
                                .reasons
                                .push("neither CDP nor AX available".to_string());
                        }
                        Err(e) => {
                            self.uncertainty.score = (self.uncertainty.score + 0.15).min(1.0);
                            self.uncertainty
                                .reasons
                                .push(format!("take_ax_snapshot: {}", e));
                        }
                    }
                }
                Err(e) => {
                    self.uncertainty.score = (self.uncertainty.score + 0.15).min(1.0);
                    self.uncertainty
                        .reasons
                        .push(format!("cdp_find_elements: {}", e));
                }
            }
        }

        if self.focused_app.is_none()
            && let Ok(Some(body)) = obs.observe("list_apps", serde_json::json!({})).await
            && let Ok(focused) = parse_focused_app_from_list(&body)
        {
            self.focused_app = Some(Fresh {
                value: focused,
                written_at: step_index,
                source: FreshnessSource::DirectObservation,
                ttl_steps: Some(4),
            });
        }
        if self.window_list.is_none()
            && let Ok(Some(body)) = obs.observe("list_windows", serde_json::json!({})).await
            && let Ok(wins) = parse_window_list(&body)
        {
            self.window_list = Some(Fresh {
                value: wins,
                written_at: step_index,
                source: FreshnessSource::DirectObservation,
                ttl_steps: Some(4),
            });
        }

        Ok(())
    }
}

/// Parse `list_apps` output into a `FocusedApp`. Shape mirrors the live
/// probe path the legacy runner used.
fn parse_focused_app_from_list(body: &str) -> Result<FocusedApp, String> {
    #[derive(serde::Deserialize)]
    struct AppRow {
        name: String,
        #[serde(default)]
        kind: String,
        pid: i32,
        #[serde(default)]
        focused: bool,
    }
    let rows: Vec<AppRow> =
        serde_json::from_str(body).map_err(|e| format!("list_apps parse: {}", e))?;
    let focused = rows
        .into_iter()
        .find(|r| r.focused)
        .ok_or_else(|| "no focused app in list_apps output".to_string())?;
    let kind = match focused.kind.as_str() {
        "ElectronApp" | "electron_app" => AppKind::ElectronApp,
        "ChromeBrowser" | "chrome_browser" => AppKind::ChromeBrowser,
        _ => AppKind::Native,
    };
    Ok(FocusedApp {
        name: focused.name,
        kind,
        pid: focused.pid,
    })
}

/// Parse `list_windows` output into `Vec<WindowRef>`.
fn parse_window_list(body: &str) -> Result<Vec<WindowRef>, String> {
    #[derive(serde::Deserialize)]
    struct WinRow {
        app_name: String,
        title: String,
        pid: i32,
    }
    let rows: Vec<WinRow> =
        serde_json::from_str(body).map_err(|e| format!("list_windows parse: {}", e))?;
    Ok(rows
        .into_iter()
        .map(|w| WindowRef {
            app_name: w.app_name,
            title: w.title,
            pid: w.pid,
        })
        .collect())
}

/// Parse native `take_ax_snapshot` text output into structured `AxElement`s.
/// The format is documented in `native-devtools-mcp/src/tools/ax_snapshot.rs::format_snapshot`.
///
/// Walks an ancestor stack keyed on indentation depth so each element's
/// `parent_name` is the `name` of the nearest preceding line one depth
/// shallower that carried a non-empty name. Mirrors the derivation in
/// `crate::executor::deterministic::ax::parse_ax_snapshot`.
pub fn parse_ax_snapshot(text: &str) -> Vec<AxElement> {
    let mut out = Vec::new();
    let mut ancestor_stack: Vec<(u32, String)> = Vec::new();
    for line in text.lines() {
        let Some(mut el) = parse_ax_line(line) else {
            continue;
        };
        // Drop ancestors at the same depth or deeper.
        while let Some((d, _)) = ancestor_stack.last() {
            if *d >= el.depth {
                ancestor_stack.pop();
            } else {
                break;
            }
        }
        el.parent_name = ancestor_stack.last().map(|(_, n)| n.clone());
        if let Some(name) = el.name.clone()
            && !name.is_empty()
        {
            ancestor_stack.push((el.depth, name));
        }
        out.push(el);
    }
    out
}

fn parse_ax_line(line: &str) -> Option<AxElement> {
    // Count leading 2-space indents to compute depth.
    let trimmed = line.trim_start_matches(' ');
    let indent_chars = line.len() - trimmed.len();
    let depth = (indent_chars / 2) as u32;
    if trimmed.is_empty() {
        return None;
    }

    // First token must be `uid=aXgY`.
    let mut parts = trimmed.splitn(3, ' ');
    let uid_tok = parts.next()?;
    let uid = uid_tok.strip_prefix("uid=")?.to_string();
    let role = parts.next()?.to_string();
    let rest = parts.next().unwrap_or("");

    // Walk the rest: optional `"name"`, then a sequence of attr tokens.
    let mut name: Option<String> = None;
    let mut value: Option<String> = None;
    let mut focused = false;
    let mut disabled = false;

    let mut chars = rest.chars().peekable();
    // Consume a leading `"..."` if present.
    if chars.peek() == Some(&'"') {
        chars.next();
        let mut s = String::new();
        for c in chars.by_ref() {
            if c == '"' {
                break;
            }
            s.push(c);
        }
        name = Some(s);
    }

    // Remaining tokens split on whitespace.
    let remaining: String = chars.collect();
    for tok in remaining.split_whitespace() {
        if let Some(v) = tok
            .strip_prefix("value=\"")
            .and_then(|s| s.strip_suffix('"'))
        {
            value = Some(v.to_string());
        } else if tok == "focused" {
            focused = true;
        } else if tok == "disabled" {
            disabled = true;
        }
        // Unknown attributes ignored.
    }

    Some(AxElement {
        uid,
        role,
        name,
        value,
        depth,
        focused,
        disabled,
        parent_name: None,
    })
}

#[derive(Debug, Clone, serde::Deserialize)]
struct OcrMatchRaw {
    text: String,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    #[serde(default)]
    confidence: f32,
}

pub fn parse_ocr_matches(text: &str) -> Result<Vec<OcrMatch>, serde_json::Error> {
    let raw: Vec<OcrMatchRaw> = serde_json::from_str(text)?;
    Ok(raw
        .into_iter()
        .map(|r| OcrMatch {
            text: r.text,
            x: r.x,
            y: r.y,
            width: r.width,
            height: r.height,
            confidence: r.confidence,
        })
        .collect())
}

#[cfg(test)]
#[allow(clippy::field_reassign_with_default)] // Tests build WorldModel in stages for readability.
mod tests {
    use super::*;

    fn fresh_focused_app(step: usize) -> Fresh<FocusedApp> {
        Fresh {
            value: FocusedApp {
                name: "Chrome".to_string(),
                kind: AppKind::ChromeBrowser,
                pid: 1234,
            },
            written_at: step,
            source: FreshnessSource::DirectObservation,
            ttl_steps: None,
        }
    }

    #[test]
    fn apply_events_focus_changing_invalidates_focused_app() {
        let mut wm = WorldModel::default();
        wm.focused_app = Some(fresh_focused_app(1));
        wm.apply_events(vec![InvalidationEvent::FocusChanging {
            tool: "launch_app".to_string(),
        }]);
        assert!(
            wm.focused_app.is_none(),
            "focused_app should be invalidated"
        );
    }

    #[test]
    fn apply_events_cdp_navigation_invalidates_elements_and_cdp_page() {
        let mut wm = WorldModel::default();
        wm.cdp_page = Some(Fresh {
            value: CdpPageState {
                url: "https://old.example.com/".to_string(),
                page_fingerprint: "abc".to_string(),
            },
            written_at: 1,
            source: FreshnessSource::DirectObservation,
            ttl_steps: None,
        });
        wm.elements = Some(Fresh {
            value: Vec::new(),
            written_at: 1,
            source: FreshnessSource::DirectObservation,
            ttl_steps: None,
        });
        wm.apply_events(vec![InvalidationEvent::CdpNavigation {
            new_url: "https://new.example.com/".to_string(),
        }]);
        assert!(wm.cdp_page.is_none());
        assert!(wm.elements.is_none());
    }

    #[test]
    fn apply_events_snapshot_stale_invalidates_ax_snapshot_when_age_exceeds_ttl() {
        let mut wm = WorldModel::default();
        wm.last_native_ax_snapshot = Some(Fresh {
            value: AxSnapshotData {
                snapshot_id: "a1g3".to_string(),
                element_count: 5,
                captured_at_step: 1,
                ax_tree_text: "uid=a1g3 button \"OK\"".to_string(),
            },
            written_at: 1,
            source: FreshnessSource::DirectObservation,
            ttl_steps: Some(3),
        });
        wm.apply_events(vec![InvalidationEvent::SnapshotStale { age_steps: 4 }]);
        assert!(wm.last_native_ax_snapshot.is_none());
    }

    #[test]
    fn apply_events_tool_failed_bumps_uncertainty_but_does_not_drop_fields() {
        let mut wm = WorldModel::default();
        wm.focused_app = Some(fresh_focused_app(1));
        let before = wm.uncertainty.score;
        wm.apply_events(vec![InvalidationEvent::ToolFailed {
            tool: "cdp_click".to_string(),
        }]);
        assert!(
            wm.uncertainty.score > before,
            "tool failure should elevate uncertainty"
        );
        assert!(wm.focused_app.is_some(), "focused_app should persist");
    }

    #[test]
    fn recompute_uncertainty_covers_all_d14_signals() {
        // D14: score factors in invalid field count, consecutive
        // errors, refuted-hypothesis count, and modal/dialog mismatch.
        // Default WorldModel has 8 unobserved Option<Fresh<_>> fields,
        // so the invalid-field contribution is nonzero from the start —
        // POMDP belief-state semantics (missing observations are
        // epistemic uncertainty). Each signal then increments
        // monotonically.
        let mut wm = WorldModel::default();
        wm.recompute_uncertainty(UncertaintySignals {
            consecutive_errors: 0,
            refuted_hypotheses: 0,
            modal_dialog_mismatch: false,
        });
        assert!(
            wm.uncertainty.score > 0.0,
            "default WorldModel has unobserved fields; invalid-field contribution must be nonzero"
        );
        let with_invalid_only = wm.uncertainty.score;

        wm.recompute_uncertainty(UncertaintySignals {
            consecutive_errors: 2,
            refuted_hypotheses: 1,
            modal_dialog_mismatch: true,
        });
        assert!(wm.uncertainty.score > with_invalid_only);
        assert!(wm.uncertainty.score <= 1.0);
    }

    #[test]
    fn parse_ax_snapshot_basic() {
        let text = "uid=a1g3 RootWebArea \"Page Title\"\n  uid=a2g3 button \"Submit\"\n  uid=a3g3 textbox value=\"hello\" focused";
        let parsed = parse_ax_snapshot(text);
        assert_eq!(parsed.len(), 3);
        assert_eq!(parsed[0].uid, "a1g3");
        assert_eq!(parsed[0].role, "RootWebArea");
        assert_eq!(parsed[0].name.as_deref(), Some("Page Title"));
        assert_eq!(parsed[0].depth, 0);
        assert_eq!(parsed[1].depth, 1);
        assert_eq!(parsed[2].role, "textbox");
        assert_eq!(parsed[2].value.as_deref(), Some("hello"));
        assert!(parsed[2].focused);
    }

    #[test]
    fn parse_ax_snapshot_empty_input_returns_empty_vec() {
        assert!(parse_ax_snapshot("").is_empty());
    }

    #[test]
    fn parse_ax_snapshot_handles_disabled_and_omitted_name() {
        let text = "uid=a1g1 generic\n  uid=a2g1 checkbox \"Remember me\" disabled selected";
        let parsed = parse_ax_snapshot(text);
        assert_eq!(parsed.len(), 2);
        assert!(parsed[0].name.is_none());
        assert!(parsed[1].disabled);
    }

    #[test]
    fn parse_ax_snapshot_tolerates_unknown_attributes() {
        // Future MCP versions may add new trailing attributes. The parser
        // must not panic or drop the element.
        let text = "uid=a1g1 button \"Click\" novel_attr=42";
        let parsed = parse_ax_snapshot(text);
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].role, "button");
    }

    #[test]
    fn parse_ax_snapshot_derives_parent_name_from_indentation() {
        let text = "\
uid=a1g1 list \"Networks\"
  uid=a2g1 row \"Wi-Fi\"
    uid=a3g1 textbox \"Password\"
";
        let parsed = parse_ax_snapshot(text);
        assert_eq!(parsed[0].parent_name, None); // root
        assert_eq!(parsed[1].parent_name.as_deref(), Some("Networks"));
        assert_eq!(parsed[2].parent_name.as_deref(), Some("Wi-Fi"));
    }

    #[test]
    fn parse_ax_snapshot_parent_name_skips_unnamed_ancestor() {
        let text = "\
uid=a1g1 generic
  uid=a2g1 button \"Submit\"
";
        let parsed = parse_ax_snapshot(text);
        // Ancestor had no name; children get None.
        assert_eq!(parsed[1].parent_name, None);
    }

    #[test]
    fn parse_ocr_matches_from_find_text_json() {
        let json = r#"[
            {"text":"Submit","x":10,"y":20,"width":60,"height":24,"confidence":0.95},
            {"text":"Cancel","x":80,"y":20,"width":60,"height":24,"confidence":0.92}
        ]"#;
        let parsed = parse_ocr_matches(json).unwrap();
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].text, "Submit");
        assert_eq!(parsed[1].x, 80);
    }

    #[test]
    fn parse_ocr_matches_tolerates_extra_fields() {
        let json = r#"[{"text":"A","x":0,"y":0,"width":10,"height":10,"confidence":0.5,"extra":"ignored"}]"#;
        let parsed = parse_ocr_matches(json).unwrap();
        assert_eq!(parsed.len(), 1);
    }

    #[test]
    fn parse_ocr_matches_rejects_malformed_json() {
        let bad = "not json";
        assert!(parse_ocr_matches(bad).is_err());
    }
}

#[cfg(test)]
mod refresh_tests {
    use super::*;
    use async_trait::async_trait;

    struct StubObserver;

    #[async_trait]
    impl WorldModelObserver for StubObserver {
        async fn observe(
            &self,
            tool_name: &str,
            _args: serde_json::Value,
        ) -> Result<Option<String>, String> {
            match tool_name {
                "cdp_find_elements" => Ok(Some(
                    r#"{"page_url":"https://example.com/","source":"cdp","matches":[{"uid":"d1","role":"button","label":"OK","tag":"button"}]}"#
                        .to_string(),
                )),
                "take_ax_snapshot" => Ok(Some("uid=a1g1 button \"OK\"".to_string())),
                _ => Ok(None),
            }
        }
    }

    #[tokio::test]
    async fn refresh_repopulates_elements_via_cdp_when_available() {
        // P2.H1: CDP availability is signaled by the observer returning
        // Ok(Some(_)) from cdp_find_elements. The observer is the
        // authoritative source for "CDP is currently attached".
        let mut wm = WorldModel::default();
        wm.apply_events(vec![InvalidationEvent::CdpNavigation {
            new_url: "https://example.com/".to_string(),
        }]);
        let obs = StubObserver;
        wm.refresh_invalid_fields(&obs, 1).await.unwrap();
        let els = wm.elements.as_ref().unwrap();
        assert!(
            matches!(els.value.first(), Some(ObservedElement::Cdp(_))),
            "elements must be repopulated via CDP path when the observer returns a CDP body"
        );
    }

    #[tokio::test]
    async fn refresh_falls_back_to_ax_when_cdp_returns_none() {
        struct AxOnlyObserver;
        #[async_trait]
        impl WorldModelObserver for AxOnlyObserver {
            async fn observe(
                &self,
                tool_name: &str,
                _args: serde_json::Value,
            ) -> Result<Option<String>, String> {
                match tool_name {
                    "cdp_find_elements" => Ok(None),
                    "take_ax_snapshot" => Ok(Some("uid=a1g1 button \"OK\"".to_string())),
                    _ => Ok(None),
                }
            }
        }
        let mut wm = WorldModel::default();
        // `elements` is already None in Default; refresh should still fill it.
        wm.refresh_invalid_fields(&AxOnlyObserver, 1).await.unwrap();
        let els = wm.elements.as_ref().unwrap();
        assert!(
            matches!(els.value.first(), Some(ObservedElement::Ax(_))),
            "expected AX fallback when cdp_find_elements returns None"
        );
    }

    #[tokio::test]
    async fn refresh_failure_elevates_uncertainty_but_does_not_panic() {
        struct FailObserver;
        #[async_trait]
        impl WorldModelObserver for FailObserver {
            async fn observe(
                &self,
                _tool_name: &str,
                _args: serde_json::Value,
            ) -> Result<Option<String>, String> {
                Err("mcp unavailable".to_string())
            }
        }
        let mut wm = WorldModel::default();
        wm.apply_events(vec![InvalidationEvent::CdpNavigation {
            new_url: String::new(),
        }]);
        let before = wm.uncertainty.score;
        let res = wm.refresh_invalid_fields(&FailObserver, 1).await;
        assert!(
            res.is_ok(),
            "refresh must not surface error — returns Ok and elevates uncertainty"
        );
        assert!(wm.uncertainty.score > before);
    }
}

#[cfg(test)]
mod observation_union_tests {
    //! Coverage for the observation / cacheability predicates that gate
    //! approval bypass, cache eligibility, and workflow-node inclusion.
    //!
    //! Ported verbatim from the legacy runner's observation-union tests
    //! for Task 3a.7.d. Legacy tests hit `AgentRunner::<B>::is_observation_tool`
    //! etc. as generic associated fns; their `StateRunner` replacements
    //! are free functions in `crate::agent::runner`, promoted to
    //! `pub(crate)` so this module can exercise them without adding a
    //! runner-shaped wrapper. The hardcoded-list ∪ readOnlyHint ∖
    //! CONFIRMABLE_TOOLS semantics are identical to the legacy contract.
    //!
    //! NOTE on destination: the task plan specifies `world_model.rs` as
    //! the new home for these tests, with the rationale that observation
    //! merging is a `WorldModel::apply_events` / `refresh_invalid_fields`
    //! concern. That rationale applies to a different axis — the tests
    //! ported here target tool *classification*, which still lives as
    //! free functions in `runner.rs`. Keeping the plan's file home so
    //! Phase 3b's deletion of the legacy runner doesn't drop coverage.
    use std::collections::HashMap;

    use crate::agent::permissions::ToolAnnotations;
    use crate::agent::runner::{
        extract_result_text, is_ax_dispatch_tool, is_observation_tool, is_state_transition_tool,
    };

    fn is_observation(
        tool_name: &str,
        annotations_by_tool: &HashMap<String, ToolAnnotations>,
    ) -> bool {
        is_observation_tool(tool_name, annotations_by_tool)
    }

    #[test]
    fn hardcoded_tool_is_observation_without_annotations() {
        let annotations: HashMap<String, ToolAnnotations> = HashMap::new();
        assert!(is_observation("take_screenshot", &annotations));
        assert!(is_observation("cdp_find_elements", &annotations));
    }

    #[test]
    fn readonly_hint_makes_novel_tool_observation() {
        // Tool not in the hardcoded list becomes observation once the MCP
        // manifest advertises `readOnlyHint = true`.
        let mut annotations = HashMap::new();
        annotations.insert(
            "custom_inspect".to_string(),
            ToolAnnotations {
                read_only_hint: Some(true),
                ..Default::default()
            },
        );
        assert!(is_observation("custom_inspect", &annotations));
    }

    #[test]
    fn missing_readonly_hint_is_not_observation() {
        // A tool with no annotations and not in the hardcoded list must
        // fall through to approval — the default-to-Ask path in the
        // permission policy depends on it.
        let annotations: HashMap<String, ToolAnnotations> = HashMap::new();
        assert!(!is_observation("click", &annotations));
        assert!(!is_observation("type_text", &annotations));
    }

    #[test]
    fn readonly_hint_false_is_not_observation() {
        let mut annotations = HashMap::new();
        annotations.insert(
            "custom_click".to_string(),
            ToolAnnotations {
                read_only_hint: Some(false),
                ..Default::default()
            },
        );
        assert!(!is_observation("custom_click", &annotations));
    }

    #[test]
    fn confirmable_tool_always_requires_approval_even_with_readonly_hint() {
        // Guardrail: the MCP server could (mis)advertise `launch_app` as
        // read-only, but it still has user-visible side effects. Our
        // hardcoded destructive list wins regardless.
        let mut annotations = HashMap::new();
        annotations.insert(
            "launch_app".to_string(),
            ToolAnnotations {
                read_only_hint: Some(true),
                ..Default::default()
            },
        );
        assert!(!is_observation("launch_app", &annotations));
        // Same for cdp_connect and quit_app:
        annotations.insert(
            "cdp_connect".to_string(),
            ToolAnnotations {
                read_only_hint: Some(true),
                ..Default::default()
            },
        );
        annotations.insert(
            "quit_app".to_string(),
            ToolAnnotations {
                read_only_hint: Some(true),
                ..Default::default()
            },
        );
        assert!(!is_observation("cdp_connect", &annotations));
        assert!(!is_observation("quit_app", &annotations));
    }

    #[test]
    fn extract_result_text_joins_all_text_blocks_for_transcript() {
        // Agent transcript must see every text block the tool returned.
        // Dropping later blocks silently hides data from the LLM and from
        // cache replay. JSON-parse sites use cdp_lifecycle::extract_text
        // instead.
        let result = clickweave_mcp::ToolCallResult {
            content: vec![
                clickweave_mcp::ToolContent::Text {
                    text: "[{\"x\": 1}]".to_string(),
                },
                clickweave_mcp::ToolContent::Text {
                    text: "trailing commentary".to_string(),
                },
            ],
            is_error: None,
        };
        assert_eq!(
            extract_result_text(&result),
            "[{\"x\": 1}]\ntrailing commentary"
        );
    }

    #[test]
    fn extract_result_text_placeholder_for_image_only_result() {
        let result = clickweave_mcp::ToolCallResult {
            content: vec![clickweave_mcp::ToolContent::Image {
                data: "b64data".to_string(),
                mime_type: "image/png".to_string(),
            }],
            is_error: None,
        };
        let text = extract_result_text(&result);
        assert!(text.contains("image"), "got {text:?}");
        assert!(text.contains("image/png"), "got {text:?}");
    }

    #[test]
    fn extract_result_text_empty_for_no_content() {
        let result = clickweave_mcp::ToolCallResult {
            content: vec![],
            is_error: None,
        };
        assert_eq!(extract_result_text(&result), "");
    }

    #[test]
    fn confirmable_tool_overrides_hardcoded_observation_list() {
        // Belt-and-braces: even if someone adds a CONFIRMABLE tool to
        // OBSERVATION_TOOLS by mistake, the guardrail still fires.
        // (`launch_app` is not in `OBSERVATION_TOOLS` today, but this test
        // pins the precedence rule independent of the specific list.)
        let annotations: HashMap<String, ToolAnnotations> = HashMap::new();
        assert!(!is_observation("launch_app", &annotations));
        assert!(!is_observation("quit_app", &annotations));
        assert!(!is_observation("cdp_connect", &annotations));
    }

    #[test]
    fn take_ax_snapshot_is_observation_but_ax_dispatch_is_not() {
        // Snapshot is read-only, should bypass the approval prompt and be
        // eligible for transcript-level collapse. The three dispatch tools
        // (ax_click / ax_set_value / ax_select) perform real side effects
        // even though the cursor doesn't move — they must stay in the
        // approval path, matching the MCP server's
        // `readOnlyHint: false` / `destructiveHint: false` annotations.
        let mut annotations: HashMap<String, ToolAnnotations> = HashMap::new();
        annotations.insert(
            "take_ax_snapshot".to_string(),
            ToolAnnotations {
                read_only_hint: Some(true),
                ..Default::default()
            },
        );
        annotations.insert(
            "ax_click".to_string(),
            ToolAnnotations {
                read_only_hint: Some(false),
                ..Default::default()
            },
        );
        annotations.insert(
            "ax_set_value".to_string(),
            ToolAnnotations {
                read_only_hint: Some(false),
                ..Default::default()
            },
        );
        annotations.insert(
            "ax_select".to_string(),
            ToolAnnotations {
                read_only_hint: Some(false),
                ..Default::default()
            },
        );
        assert!(is_observation("take_ax_snapshot", &annotations));
        assert!(!is_observation("ax_click", &annotations));
        assert!(!is_observation("ax_set_value", &annotations));
        assert!(!is_observation("ax_select", &annotations));
    }

    #[test]
    fn ax_dispatch_tools_are_not_cacheable() {
        // Cache eligibility on the write side AND replay on the read side
        // must skip AX dispatch tools — their `uid` argument is scoped to
        // one snapshot generation.
        assert!(is_ax_dispatch_tool("ax_click"));
        assert!(is_ax_dispatch_tool("ax_set_value"));
        assert!(is_ax_dispatch_tool("ax_select"));
        // Snapshot and sibling tools are not dispatch tools.
        assert!(!is_ax_dispatch_tool("take_ax_snapshot"));
        assert!(!is_ax_dispatch_tool("click"));
        assert!(!is_ax_dispatch_tool("type_text"));
    }

    #[test]
    fn state_transition_tools_are_not_cacheable() {
        // Cache eligibility on the write side AND replay on the read side
        // must skip state-transition tools. Their cache key encodes the
        // pre-transition page, so replay re-fires the transition against
        // unchanged elements — which caused double `step_completed` events
        // and duplicate workflow nodes after the LLM issued a `launch_app`
        // or `focus_window` that switched away from the current CDP target.
        assert!(is_state_transition_tool("launch_app"));
        assert!(is_state_transition_tool("focus_window"));
        assert!(is_state_transition_tool("quit_app"));
        assert!(is_state_transition_tool("cdp_connect"));
        assert!(is_state_transition_tool("cdp_disconnect"));
        // Content-acting and observation tools are not state transitions.
        assert!(!is_state_transition_tool("cdp_click"));
        assert!(!is_state_transition_tool("take_ax_snapshot"));
        assert!(!is_state_transition_tool("ax_click"));
    }
}

#[cfg(all(test, feature = "specta"))]
mod specta_derive_tests {
    //! D17: the `WorldModelSnapshot` surface types rendered into the
    //! `agent://world_model_changed` event payload must derive
    //! `specta::Type` so the bindings exporter picks them up.
    use super::*;
    use specta::{Generics, Type, TypeCollection};

    #[test]
    fn uncertainty_score_derives_specta_type() {
        let _: specta::DataType =
            UncertaintyScore::inline(&mut TypeCollection::default(), Generics::NONE);
    }

    #[test]
    fn focused_app_derives_specta_type() {
        let _: specta::DataType =
            FocusedApp::inline(&mut TypeCollection::default(), Generics::NONE);
    }

    #[test]
    fn window_ref_derives_specta_type() {
        let _: specta::DataType = WindowRef::inline(&mut TypeCollection::default(), Generics::NONE);
    }

    #[test]
    fn cdp_page_state_derives_specta_type() {
        let _: specta::DataType =
            CdpPageState::inline(&mut TypeCollection::default(), Generics::NONE);
    }

    #[test]
    fn screenshot_ref_derives_specta_type() {
        let _: specta::DataType =
            ScreenshotRef::inline(&mut TypeCollection::default(), Generics::NONE);
    }
}
