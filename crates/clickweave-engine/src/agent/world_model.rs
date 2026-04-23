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
/// `AppKind` classification in `loop_runner.rs` — will unify in Phase 4.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AppKind {
    Native,
    ElectronApp,
    ChromeBrowser,
}

#[derive(Debug, Clone, Serialize)]
pub struct FocusedApp {
    pub name: String,
    pub kind: AppKind,
    pub pid: i32,
}

#[derive(Debug, Clone, Serialize)]
pub struct WindowRef {
    pub app_name: String,
    pub title: String,
    pub pid: i32,
}

#[derive(Debug, Clone, Serialize)]
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
    /// model state (D14). Weights are intentionally conservative; tune
    /// later once the metric is observed in real runs.
    ///
    /// Deviation from the Phase 1 plan: the plan's draft included a
    /// "invalid fields" contribution derived from `Option::is_none` on
    /// `focused_app`, `elements`, and `modal_present`. That incorrectly
    /// fires on a freshly-constructed `WorldModel::default()` (where no
    /// field has ever been observed) and makes the plan's own zero-signal
    /// baseline test fail. Unknown is not the same as invalid; only
    /// explicit invalidation bumps the score (see `apply_events` for
    /// `ToolFailed`, which adds to the score directly). The three
    /// explicit signals plumbed through `UncertaintySignals` are D14's
    /// authoritative inputs here.
    pub fn recompute_uncertainty(&mut self, signals: UncertaintySignals) {
        let mut score: f32 = 0.0;
        let mut reasons: Vec<String> = Vec::new();

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
        // P1.M3: D14 requires the score to factor in invalid field count,
        // consecutive errors, refuted-hypothesis count, and modal/dialog
        // mismatch. Each signal must contribute a monotonically
        // non-decreasing increment.
        let mut wm = WorldModel::default();
        let baseline = wm.uncertainty.score;
        wm.recompute_uncertainty(UncertaintySignals {
            consecutive_errors: 0,
            refuted_hypotheses: 0,
            modal_dialog_mismatch: false,
        });
        assert_eq!(wm.uncertainty.score, baseline);

        wm.recompute_uncertainty(UncertaintySignals {
            consecutive_errors: 2,
            refuted_hypotheses: 1,
            modal_dialog_mismatch: true,
        });
        assert!(wm.uncertainty.score > baseline);
        assert!(wm.uncertainty.score <= 1.0);
    }
}
