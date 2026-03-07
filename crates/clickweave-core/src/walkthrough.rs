use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::MouseButton;
use crate::storage::{append_jsonl, format_timestamped_dirname, now_millis, write_json_pretty};

// --- Session ---

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct WalkthroughSession {
    pub id: Uuid,
    pub workflow_id: Uuid,
    pub started_at: u64,
    pub ended_at: Option<u64>,
    pub status: WalkthroughStatus,
    #[serde(skip)]
    pub events: Vec<WalkthroughEvent>,
    #[serde(skip)]
    pub actions: Vec<WalkthroughAction>,
    pub warnings: Vec<String>,
}

impl WalkthroughSession {
    pub fn new(workflow_id: Uuid) -> Self {
        Self {
            id: Uuid::new_v4(),
            workflow_id,
            started_at: now_millis(),
            ended_at: None,
            status: WalkthroughStatus::Recording,
            events: Vec::new(),
            actions: Vec::new(),
            warnings: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub enum WalkthroughStatus {
    #[default]
    Idle,
    Recording,
    Paused,
    Processing,
    Review,
    Applied,
    Cancelled,
}

// --- Raw capture events ---

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct OcrAnnotation {
    pub text: String,
    pub x: f64,
    pub y: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct WalkthroughEvent {
    pub id: Uuid,
    pub timestamp: u64,
    pub kind: WalkthroughEventKind,
}

/// Classification of an app's UI framework, used to decide whether
/// Chrome DevTools Protocol (CDP) tools can provide better automation.
///
/// - `Native`: standard native app — use accessibility-based automation
/// - `ChromeBrowser`: Chrome-family browser — CDP gives DOM access
/// - `ElectronApp`: Electron-based app — native AX is unreliable, CDP preferred
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub enum AppKind {
    #[default]
    Native,
    ChromeBrowser,
    ElectronApp,
}

impl AppKind {
    /// Parse from a string value (e.g. from JSON tool arguments).
    /// Returns `None` for unrecognized values.
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "Native" => Some(Self::Native),
            "ChromeBrowser" => Some(Self::ChromeBrowser),
            "ElectronApp" => Some(Self::ElectronApp),
            _ => None,
        }
    }

    /// Whether this app kind uses Chrome DevTools Protocol for automation.
    pub fn uses_cdp(self) -> bool {
        matches!(self, Self::ChromeBrowser | Self::ElectronApp)
    }
}

/// CDP element data captured during walkthrough recording.
/// Attached to MouseClicked events for clicks in CDP-enabled apps.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct CdpClickAnnotation {
    pub uid: String,
    pub label: String,
    pub role: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(tag = "type")]
pub enum WalkthroughEventKind {
    AppFocused {
        app_name: String,
        pid: i32,
        window_title: Option<String>,
        #[serde(default)]
        app_kind: AppKind,
    },
    MouseClicked {
        x: f64,
        y: f64,
        button: MouseButton,
        click_count: u32,
        modifiers: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cdp_element: Option<CdpClickAnnotation>,
    },
    KeyPressed {
        key: String,
        modifiers: Vec<String>,
    },
    TextCommitted {
        text: String,
    },
    Scrolled {
        delta_y: f64,
        x: Option<f64>,
        y: Option<f64>,
    },
    ScreenshotCaptured {
        path: String,
        kind: ScreenshotKind,
        /// Window origin and scale from the MCP screenshot metadata.
        /// Used to map screen coordinates to image pixel coordinates.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        meta: Option<ScreenshotMeta>,
        /// Base64-encoded JPEG of a click crop. Only set for `ClickCrop` kind.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        image_b64: Option<String>,
    },
    OcrCaptured {
        annotations: Vec<OcrAnnotation>,
        click_x: f64,
        click_y: f64,
    },
    AccessibilityElementCaptured {
        label: String,
        role: Option<String>,
    },
    VlmLabelResolved {
        label: String,
    },
    CdpSnapshotCaptured {
        snapshot_text: String,
        click_event_id: Uuid,
    },
    Paused,
    Resumed,
    Stopped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub enum ScreenshotKind {
    BeforeClick,
    AfterClick,
    ClickCrop,
}

/// Screenshot coordinate metadata for mapping screen coordinates to image pixels.
///
/// Given a screen coordinate `(sx, sy)`, the image pixel coordinate is:
/// `px = (sx - origin_x) * scale`, `py = (sy - origin_y) * scale`
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct ScreenshotMeta {
    pub origin_x: f64,
    pub origin_y: f64,
    pub scale: f64,
}

impl ScreenshotMeta {
    /// Convert screen coordinates to image pixel coordinates.
    pub fn screen_to_pixel(&self, sx: f64, sy: f64) -> (f64, f64) {
        (
            (sx - self.origin_x) * self.scale,
            (sy - self.origin_y) * self.scale,
        )
    }
}

// --- Normalized semantic actions ---

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct WalkthroughAction {
    pub id: Uuid,
    pub kind: WalkthroughActionKind,
    pub app_name: Option<String>,
    pub window_title: Option<String>,
    pub target_candidates: Vec<TargetCandidate>,
    pub artifact_paths: Vec<String>,
    pub source_event_ids: Vec<Uuid>,
    pub confidence: ActionConfidence,
    pub warnings: Vec<String>,
    /// Screenshot coordinate metadata for VLM click target resolution.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub screenshot_meta: Option<ScreenshotMeta>,
}

impl WalkthroughAction {
    /// Create a new action with default fields (high confidence, no candidates/artifacts/warnings).
    fn new(
        kind: WalkthroughActionKind,
        app_name: Option<String>,
        source_event_ids: Vec<Uuid>,
    ) -> Self {
        Self {
            id: Uuid::new_v4(),
            kind,
            app_name,
            window_title: None,
            target_candidates: vec![],
            artifact_paths: vec![],
            source_event_ids,
            confidence: ActionConfidence::High,
            warnings: vec![],
            screenshot_meta: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(tag = "type")]
pub enum WalkthroughActionKind {
    LaunchApp {
        app_name: String,
        app_kind: AppKind,
    },
    FocusWindow {
        app_name: String,
        window_title: Option<String>,
        app_kind: AppKind,
    },
    Click {
        x: f64,
        y: f64,
        button: MouseButton,
        click_count: u32,
    },
    TypeText {
        text: String,
    },
    PressKey {
        key: String,
        modifiers: Vec<String>,
    },
    Scroll {
        delta_y: f64,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(tag = "type")]
pub enum TargetCandidate {
    AccessibilityLabel {
        label: String,
        role: Option<String>,
    },
    /// Label identified by a vision language model from a screenshot crop.
    VlmLabel {
        label: String,
    },
    OcrText {
        text: String,
    },
    ImageCrop {
        path: String,
        image_b64: String,
    },
    Coordinates {
        x: f64,
        y: f64,
    },
    /// Element verified via Chrome DevTools Protocol snapshot.
    CdpElement {
        text: String,
        uid: String,
    },
}

/// Accessibility roles that represent specific, actionable UI elements.
/// Labels from these roles are reliable enough to use as click targets
/// without VLM fallback.
const ACTIONABLE_AX_ROLES: &[&str] = &[
    "AXButton",
    "AXCheckBox",
    "AXComboBox",
    "AXDisclosureTriangle",
    "AXIncrementor",
    "AXLink",
    "AXMenuButton",
    "AXMenuItem",
    "AXPopUpButton",
    "AXRadioButton",
    "AXSegmentedControl",
    "AXSlider",
    "AXStaticText",
    "AXTab",
    "AXTabButton",
    "AXTextField",
    "AXTextArea",
    "AXToggle",
    "AXToolbarButton",
];

impl TargetCandidate {
    /// Return the text label if this candidate is useful as a click target.
    /// Non-actionable AX labels (e.g. AXWindow, AXGroup) are skipped so that
    /// VLM or OCR labels take priority in `find_map` iteration.
    pub fn preferred_label(&self) -> Option<&str> {
        match self {
            Self::AccessibilityLabel { label, role } => {
                if is_actionable_ax_role(role.as_deref()) {
                    Some(label)
                } else {
                    None
                }
            }
            Self::VlmLabel { label } => Some(label),
            Self::OcrText { text } => Some(text),
            Self::CdpElement { text, .. } => Some(text),
            _ => None,
        }
    }

    /// Whether this is an accessibility label from a specific, actionable element
    /// (button, text field, menu item, etc.) as opposed to a container
    /// (window, group, application).
    pub fn is_actionable_ax_label(&self) -> bool {
        matches!(self, Self::AccessibilityLabel { role: Some(r), .. } if is_actionable_ax_role(Some(r.as_str())))
    }
}

/// Whether an accessibility role string represents an actionable UI element.
pub fn is_actionable_ax_role(role: Option<&str>) -> bool {
    role.is_some_and(|r| ACTIONABLE_AX_ROLES.contains(&r))
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub enum ActionConfidence {
    High,
    #[default]
    Medium,
    Low,
}

// --- Review annotations ---

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct WalkthroughAnnotations {
    pub deleted_node_ids: Vec<Uuid>,
    pub renamed_nodes: Vec<NodeRename>,
    pub target_overrides: Vec<TargetOverride>,
    pub variable_promotions: Vec<VariablePromotion>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct NodeRename {
    pub node_id: Uuid,
    pub new_name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct TargetOverride {
    pub node_id: Uuid,
    pub chosen_candidate_index: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct VariablePromotion {
    pub node_id: Uuid,
    pub variable_name: String,
}

/// Maps a walkthrough action to its corresponding workflow node in the draft.
/// For deterministic drafts this is 1:1. For LLM-enhanced drafts, some actions
/// may have no node (removed as redundant) and some nodes may have no action
/// (LLM-added verification nodes).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct ActionNodeEntry {
    pub action_id: Uuid,
    pub node_id: Uuid,
}

/// Build a 1:1 action→node mapping for a deterministic draft where
/// actions and nodes are in the same order.
pub fn build_action_node_map(
    actions: &[WalkthroughAction],
    workflow: &crate::Workflow,
) -> Vec<ActionNodeEntry> {
    actions
        .iter()
        .zip(workflow.nodes.iter())
        .map(|(a, n)| ActionNodeEntry {
            action_id: a.id,
            node_id: n.id,
        })
        .collect()
}

// --- Walkthrough storage ---

/// Manages on-disk storage for walkthrough session data and artifacts.
///
/// Directory layout:
/// ```text
/// walkthroughs/<timestamp>_<shortid>/
///   session.json
///   events.jsonl
///   actions.json
///   draft.json
///   artifacts/
/// ```
#[derive(Clone)]
pub struct WalkthroughStorage {
    base_path: std::path::PathBuf,
}

impl WalkthroughStorage {
    /// Create storage for a saved project.
    ///
    /// Path: `<project>/.clickweave/walkthroughs/`
    pub fn new(project_path: &std::path::Path) -> Self {
        Self {
            base_path: project_path.join(".clickweave").join("walkthroughs"),
        }
    }

    /// Create storage for an unsaved project (app data fallback).
    ///
    /// Path: `<app_data>/walkthroughs/`
    pub fn new_app_data(app_data_dir: &std::path::Path) -> Self {
        Self {
            base_path: app_data_dir.join("walkthroughs"),
        }
    }

    /// Create a directory for a new walkthrough session.
    /// Returns the full path to the session directory.
    pub fn create_session_dir(
        &self,
        session: &WalkthroughSession,
    ) -> anyhow::Result<std::path::PathBuf> {
        let dirname = format_timestamped_dirname(session.started_at, session.id);
        let session_dir = self.base_path.join(&dirname);
        std::fs::create_dir_all(session_dir.join("artifacts"))
            .map_err(|e| anyhow::anyhow!("Failed to create walkthrough session directory: {e}"))?;

        Ok(session_dir)
    }

    /// Save the session metadata to `session.json`.
    ///
    /// Note: `events` and `actions` are skipped during serialization —
    /// they live in `events.jsonl` and `actions.json` respectively.
    pub fn save_session(
        &self,
        session_dir: &std::path::Path,
        session: &WalkthroughSession,
    ) -> anyhow::Result<()> {
        write_json_pretty(&session_dir.join("session.json"), session)
    }

    /// Append a raw event to `events.jsonl`.
    pub fn append_event(
        &self,
        session_dir: &std::path::Path,
        event: &WalkthroughEvent,
    ) -> anyhow::Result<()> {
        append_jsonl(&session_dir.join("events.jsonl"), event)
    }

    /// Save the normalized actions to `actions.json`.
    pub fn save_actions(
        &self,
        session_dir: &std::path::Path,
        actions: &[WalkthroughAction],
    ) -> anyhow::Result<()> {
        write_json_pretty(&session_dir.join("actions.json"), actions)
    }

    /// Save a workflow draft to `draft.json`.
    pub fn save_draft(
        &self,
        session_dir: &std::path::Path,
        draft: &crate::Workflow,
    ) -> anyhow::Result<()> {
        write_json_pretty(&session_dir.join("draft.json"), draft)
    }

    /// Read all events from `events.jsonl` in a session directory.
    pub fn read_events(
        &self,
        session_dir: &std::path::Path,
    ) -> anyhow::Result<Vec<WalkthroughEvent>> {
        let path = session_dir.join("events.jsonl");
        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(anyhow::anyhow!("Failed to read events.jsonl: {e}")),
        };
        let mut events = Vec::new();
        for line in content.lines() {
            if line.trim().is_empty() {
                continue;
            }
            let event: WalkthroughEvent = serde_json::from_str(line)
                .map_err(|e| anyhow::anyhow!("Failed to parse event line: {e}"))?;
            events.push(event);
        }
        Ok(events)
    }

    pub fn base_path(&self) -> &std::path::Path {
        &self.base_path
    }
}

// --- Event normalization ---

/// Idle gap threshold for text coalescing (milliseconds).
const TEXT_IDLE_GAP_MS: u64 = 2000;

/// Maximum distance (pixels) for matching OCR text to a click point.
pub const OCR_PROXIMITY_PX: f64 = 50.0;

/// Maximum gap between scroll events to coalesce (milliseconds).
const SCROLL_COALESCE_GAP_MS: u64 = 300;

/// Flush accumulated text buffer into a single TypeText action.
fn flush_text(
    buf: &mut Vec<(Uuid, u64, String)>,
    actions: &mut Vec<WalkthroughAction>,
    current_app: &Option<String>,
) {
    if buf.is_empty() {
        return;
    }
    let text: String = buf.iter().map(|(_, _, t)| t.as_str()).collect();
    let source_ids: Vec<Uuid> = buf.iter().map(|(id, _, _)| *id).collect();
    actions.push(WalkthroughAction::new(
        WalkthroughActionKind::TypeText { text },
        current_app.clone(),
        source_ids,
    ));
    buf.clear();
}

/// Normalize raw walkthrough events into semantic actions.
///
/// Returns `(actions, warnings)`. Pure function — no I/O.
pub fn normalize_events(events: &[WalkthroughEvent]) -> (Vec<WalkthroughAction>, Vec<String>) {
    // Sort by timestamp so each click is followed by its enrichment events.
    // Background enrichment tasks append events out-of-order (after later
    // clicks), but reuse the original click's timestamp. Stable sort keeps
    // the click before its enrichment within the same timestamp.
    let mut sorted = events.to_vec();
    sorted.sort_by_key(|e| e.timestamp);
    let events = &sorted;

    let mut actions: Vec<WalkthroughAction> = Vec::new();
    let warnings: Vec<String> = Vec::new();
    let mut seen_apps: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut last_app: Option<String> = None;
    let mut text_buffer: Vec<(Uuid, u64, String)> = Vec::new();
    let mut last_scroll_ts: u64 = 0;
    let mut i = 0;

    while i < events.len() {
        let event = &events[i];
        i += 1;
        match &event.kind {
            WalkthroughEventKind::AppFocused {
                app_name,
                window_title,
                app_kind,
                ..
            } => {
                flush_text(&mut text_buffer, &mut actions, &last_app);

                // Collapse repeated focus on same app, but update app_kind
                // if it changed (e.g. reactive Electron reclassification).
                if last_app.as_ref() == Some(app_name) {
                    // Search backward for the most recent focus/launch action
                    // for this app — it may not be the very last action (clicks
                    // can appear between the original focus and the correction).
                    for prev in actions.iter_mut().rev() {
                        let (prev_name, prev_kind) = match &mut prev.kind {
                            WalkthroughActionKind::LaunchApp {
                                app_name: name,
                                app_kind: kind,
                            } => (name as &str, kind),
                            WalkthroughActionKind::FocusWindow {
                                app_name: name,
                                app_kind: kind,
                                ..
                            } => (name as &str, kind),
                            _ => continue,
                        };
                        if prev_name == app_name && *prev_kind != *app_kind {
                            *prev_kind = *app_kind;
                            break;
                        }
                    }
                    continue;
                }

                let is_new = seen_apps.insert(app_name.clone());
                let kind = if is_new {
                    WalkthroughActionKind::LaunchApp {
                        app_name: app_name.clone(),
                        app_kind: *app_kind,
                    }
                } else {
                    WalkthroughActionKind::FocusWindow {
                        app_name: app_name.clone(),
                        window_title: window_title.clone(),
                        app_kind: *app_kind,
                    }
                };

                let mut action =
                    WalkthroughAction::new(kind, Some(app_name.clone()), vec![event.id]);
                action.window_title = window_title.clone();
                actions.push(action);
                last_app = Some(app_name.clone());
            }

            WalkthroughEventKind::TextCommitted { text } => {
                // Check for idle gap — break text group.
                if let Some((_, last_ts, _)) = text_buffer.last()
                    && event.timestamp - last_ts > TEXT_IDLE_GAP_MS
                {
                    flush_text(&mut text_buffer, &mut actions, &last_app);
                }
                text_buffer.push((event.id, event.timestamp, text.clone()));
            }

            WalkthroughEventKind::MouseClicked {
                x,
                y,
                button,
                click_count,
                ..
            } => {
                flush_text(&mut text_buffer, &mut actions, &last_app);

                // Lookahead: collect enrichment events (screenshot, OCR, accessibility)
                // that follow this click before the next action event.
                let mut screenshot_path: Option<String> = None;
                let mut screenshot_meta: Option<ScreenshotMeta> = None;
                let mut ocr_annotations: Option<&Vec<OcrAnnotation>> = None;
                let mut ax_label: Option<(String, Option<String>)> = None;
                let mut vlm_label: Option<String> = None;
                let mut crop_candidate: Option<(String, String)> = None;
                let mut cdp_snapshot: Option<String> = None;
                let mut peek = i;
                while peek < events.len() {
                    match &events[peek].kind {
                        WalkthroughEventKind::ScreenshotCaptured {
                            path,
                            kind: ScreenshotKind::ClickCrop,
                            image_b64: Some(b64),
                            ..
                        } => {
                            crop_candidate = Some((path.clone(), b64.clone()));
                        }
                        WalkthroughEventKind::ScreenshotCaptured { path, meta, .. } => {
                            screenshot_path = Some(path.clone());
                            screenshot_meta = *meta;
                        }
                        WalkthroughEventKind::OcrCaptured { annotations, .. } => {
                            ocr_annotations = Some(annotations);
                        }
                        WalkthroughEventKind::AccessibilityElementCaptured { label, role } => {
                            ax_label = Some((label.clone(), role.clone()));
                        }
                        WalkthroughEventKind::VlmLabelResolved { label } => {
                            vlm_label = Some(label.clone());
                        }
                        WalkthroughEventKind::CdpSnapshotCaptured { snapshot_text, .. } => {
                            cdp_snapshot = Some(snapshot_text.clone());
                        }
                        // Stop at the next action event.
                        _ => break,
                    }
                    peek += 1;
                }
                // Advance past consumed enrichment events.
                i = peek;

                // Build target candidates: CDP > accessibility label > OCR text > coordinates.
                let mut candidates = Vec::new();

                // CDP element is the highest-priority target.
                // Try AX label first, then VLM label as hint for matching.
                if let Some(ref snapshot) = cdp_snapshot {
                    let hints = ax_label
                        .as_ref()
                        .map(|(label, _)| label.as_str())
                        .into_iter()
                        .chain(vlm_label.as_deref());
                    for hint in hints {
                        let matches = crate::cdp::find_elements_in_snapshot(snapshot, hint);
                        if matches.len() == 1 {
                            let (uid, label) = &matches[0];
                            candidates.push(TargetCandidate::CdpElement {
                                text: label.clone(),
                                uid: uid.clone(),
                            });
                            break;
                        }
                    }
                }

                // Accessibility label is the most reliable target.
                if let Some((label, role)) = ax_label {
                    candidates.push(TargetCandidate::AccessibilityLabel { label, role });
                }

                // VLM label as second-best target (after actionable AX labels).
                if let Some(label) = vlm_label {
                    candidates.push(TargetCandidate::VlmLabel { label });
                }

                // OCR text as fallback.
                if let Some(annotations) = ocr_annotations {
                    let mut nearest: Option<(&OcrAnnotation, f64)> = None;
                    for ann in annotations {
                        let dist = ((ann.x - x).powi(2) + (ann.y - y).powi(2)).sqrt();
                        if dist <= OCR_PROXIMITY_PX
                            && (nearest.is_none() || dist < nearest.unwrap().1)
                        {
                            nearest = Some((ann, dist));
                        }
                    }
                    if let Some((ann, _)) = nearest {
                        candidates.push(TargetCandidate::OcrText {
                            text: ann.text.clone(),
                        });
                    }
                }
                // Image crop as fallback (before coordinates).
                if let Some((crop_path, crop_b64)) = crop_candidate {
                    candidates.push(TargetCandidate::ImageCrop {
                        path: crop_path,
                        image_b64: crop_b64,
                    });
                }
                // Always add coordinates as fallback.
                candidates.push(TargetCandidate::Coordinates { x: *x, y: *y });

                let has_image_crop = candidates
                    .iter()
                    .any(|c| matches!(c, TargetCandidate::ImageCrop { .. }));
                let confidence = if candidates
                    .iter()
                    .any(|c| matches!(c, TargetCandidate::CdpElement { .. }))
                    || candidates.iter().any(|c| c.is_actionable_ax_label())
                {
                    ActionConfidence::High
                } else if candidates.iter().any(|c| {
                    matches!(
                        c,
                        TargetCandidate::VlmLabel { .. } | TargetCandidate::OcrText { .. }
                    )
                }) || has_image_crop
                {
                    ActionConfidence::Medium
                } else {
                    ActionConfidence::Low
                };

                let mut click_warnings = Vec::new();
                if confidence == ActionConfidence::Low {
                    click_warnings.push(format!(
                        "No text target found for click at ({x:.0}, {y:.0}) — using coordinates"
                    ));
                }

                let mut action = WalkthroughAction::new(
                    WalkthroughActionKind::Click {
                        x: *x,
                        y: *y,
                        button: *button,
                        click_count: *click_count,
                    },
                    last_app.clone(),
                    vec![event.id],
                );
                action.target_candidates = candidates;
                action.confidence = confidence;
                action.warnings = click_warnings;
                action.screenshot_meta = screenshot_meta;
                if let Some(path) = screenshot_path {
                    action.artifact_paths.push(path);
                }
                actions.push(action);
            }

            WalkthroughEventKind::KeyPressed { key, modifiers } => {
                flush_text(&mut text_buffer, &mut actions, &last_app);
                actions.push(WalkthroughAction::new(
                    WalkthroughActionKind::PressKey {
                        key: key.clone(),
                        modifiers: modifiers.clone(),
                    },
                    last_app.clone(),
                    vec![event.id],
                ));
            }

            WalkthroughEventKind::Scrolled { delta_y, .. } => {
                flush_text(&mut text_buffer, &mut actions, &last_app);

                // Coalesce with previous scroll if recent.
                let coalesced = if let Some(prev) = actions.last_mut() {
                    if let WalkthroughActionKind::Scroll {
                        delta_y: ref mut prev_dy,
                    } = prev.kind
                    {
                        if event.timestamp - last_scroll_ts <= SCROLL_COALESCE_GAP_MS {
                            *prev_dy += delta_y;
                            prev.source_event_ids.push(event.id);
                            true
                        } else {
                            false
                        }
                    } else {
                        false
                    }
                } else {
                    false
                };

                if !coalesced {
                    actions.push(WalkthroughAction::new(
                        WalkthroughActionKind::Scroll { delta_y: *delta_y },
                        last_app.clone(),
                        vec![event.id],
                    ));
                }
                last_scroll_ts = event.timestamp;
            }

            // Enrichment events are consumed by the MouseClicked lookahead above,
            // so standalone occurrences are skipped.
            WalkthroughEventKind::OcrCaptured { .. }
            | WalkthroughEventKind::ScreenshotCaptured { .. }
            | WalkthroughEventKind::AccessibilityElementCaptured { .. }
            | WalkthroughEventKind::VlmLabelResolved { .. } => {}

            // CDP snapshot events are consumed in the click peek loop above.
            WalkthroughEventKind::CdpSnapshotCaptured { .. } => {}

            // Skip non-action events.
            WalkthroughEventKind::Paused
            | WalkthroughEventKind::Resumed
            | WalkthroughEventKind::Stopped => {}
        }
    }

    // Flush remaining text buffer.
    flush_text(&mut text_buffer, &mut actions, &last_app);

    // Per-action warnings are stored on each action and rendered inline in the
    // review UI. Only top-level (non-action-specific) warnings are returned here
    // to avoid double-counting in the warning badge and global warning strip.

    (actions, warnings)
}

// --- Draft synthesis ---

/// Vertical spacing between auto-positioned nodes (pixels in canvas coords).
const NODE_Y_SPACING: f32 = 100.0;
const NODE_X_POSITION: f32 = 250.0;

/// Synthesize a linear workflow draft from normalized walkthrough actions.
///
/// Pure function — no I/O. Produces a valid `Workflow` with linear edges.
pub fn synthesize_draft(
    actions: &[WalkthroughAction],
    workflow_id: Uuid,
    workflow_name: &str,
) -> crate::Workflow {
    use crate::{
        ClickParams, Edge, FocusMethod, FocusWindowParams, Node, NodeType, Position,
        PressKeyParams, ScrollParams, TypeTextParams, Workflow,
    };

    let mut workflow = Workflow {
        id: workflow_id,
        name: workflow_name.to_string(),
        nodes: Vec::new(),
        edges: Vec::new(),
    };

    for (i, action) in actions.iter().enumerate() {
        let position = Position {
            x: NODE_X_POSITION,
            y: (i as f32) * NODE_Y_SPACING,
        };

        let (node_type, name) = match &action.kind {
            WalkthroughActionKind::LaunchApp { app_name, app_kind } => (
                NodeType::FocusWindow(FocusWindowParams {
                    method: FocusMethod::AppName,
                    value: Some(app_name.clone()),
                    bring_to_front: true,
                    app_kind: *app_kind,
                }),
                format!("Launch {app_name}"),
            ),

            WalkthroughActionKind::FocusWindow {
                app_name,
                window_title,
                app_kind,
            } => (
                NodeType::FocusWindow(FocusWindowParams {
                    method: FocusMethod::AppName,
                    value: Some(app_name.clone()),
                    bring_to_front: true,
                    app_kind: *app_kind,
                }),
                match window_title {
                    Some(t) => format!("Focus '{t}'"),
                    None => format!("Focus {app_name}"),
                },
            ),

            WalkthroughActionKind::Click {
                x,
                y,
                button,
                click_count,
            } => {
                // Use the best target candidate.
                let best_target = action
                    .target_candidates
                    .iter()
                    .find_map(|c| c.preferred_label().map(|s| s.to_string()));

                // Fallback: if no text target, try image crop.
                let image_crop_b64 = if best_target.is_none() {
                    action.target_candidates.iter().find_map(|c| match c {
                        TargetCandidate::ImageCrop { image_b64, .. } => Some(image_b64.clone()),
                        _ => None,
                    })
                } else {
                    None
                };

                let (params, name) = if let Some(ref target) = best_target {
                    (
                        ClickParams {
                            target: Some(target.clone()),
                            x: None,
                            y: None,
                            button: *button,
                            click_count: *click_count,
                            ..Default::default()
                        },
                        format!("Click '{target}'"),
                    )
                } else if let Some(ref b64) = image_crop_b64 {
                    (
                        ClickParams {
                            template_image: Some(b64.clone()),
                            button: *button,
                            click_count: *click_count,
                            ..Default::default()
                        },
                        format!("Click (image match at {x:.0}, {y:.0})"),
                    )
                } else {
                    (
                        ClickParams {
                            target: None,
                            x: Some(*x),
                            y: Some(*y),
                            button: *button,
                            click_count: *click_count,
                            ..Default::default()
                        },
                        format!("Click ({x:.0}, {y:.0})"),
                    )
                };
                (NodeType::Click(params), name)
            }

            WalkthroughActionKind::TypeText { text } => {
                let display = if text.chars().count() > 20 {
                    let truncated: String = text.chars().take(20).collect();
                    format!("Type '{truncated}'...")
                } else {
                    format!("Type '{text}'")
                };
                (
                    NodeType::TypeText(TypeTextParams { text: text.clone() }),
                    display,
                )
            }

            WalkthroughActionKind::PressKey { key, modifiers } => {
                let name = if modifiers.is_empty() {
                    format!("Press {key}")
                } else {
                    format!("Press {}+{key}", modifiers.join("+"))
                };
                (
                    NodeType::PressKey(PressKeyParams {
                        key: key.clone(),
                        modifiers: modifiers.clone(),
                    }),
                    name,
                )
            }

            WalkthroughActionKind::Scroll { delta_y } => (
                NodeType::Scroll(ScrollParams {
                    delta_y: *delta_y as i32,
                    x: None,
                    y: None,
                }),
                format!("Scroll {}", if *delta_y < 0.0 { "up" } else { "down" }),
            ),
        };

        let node = Node::new(node_type, position, name);
        workflow.nodes.push(node);
    }

    // Wire linear edges.
    for i in 0..workflow.nodes.len().saturating_sub(1) {
        let from = workflow.nodes[i].id;
        let to = workflow.nodes[i + 1].id;
        workflow.edges.push(Edge {
            from,
            to,
            output: None,
        });
    }

    workflow
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_walkthrough_status_default_is_idle() {
        assert_eq!(WalkthroughStatus::default(), WalkthroughStatus::Idle);
    }

    #[test]
    fn test_session_serialization_skips_events_and_actions() {
        let session = WalkthroughSession {
            id: Uuid::new_v4(),
            workflow_id: Uuid::new_v4(),
            started_at: 1_700_000_000_000,
            ended_at: None,
            status: WalkthroughStatus::Recording,
            events: vec![WalkthroughEvent {
                id: Uuid::new_v4(),
                timestamp: 1_700_000_000_100,
                kind: WalkthroughEventKind::Paused,
            }],
            actions: vec![],
            warnings: vec![],
        };

        let json = serde_json::to_string(&session).expect("serialize");
        assert!(!json.contains("Paused"));

        let deserialized: WalkthroughSession = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(deserialized.id, session.id);
        assert_eq!(deserialized.status, WalkthroughStatus::Recording);
        assert!(deserialized.events.is_empty());
        assert!(deserialized.actions.is_empty());
    }

    #[test]
    fn test_walkthrough_status_serde_produces_expected_strings() {
        let variants = vec![
            (WalkthroughStatus::Idle, "\"Idle\""),
            (WalkthroughStatus::Recording, "\"Recording\""),
            (WalkthroughStatus::Paused, "\"Paused\""),
            (WalkthroughStatus::Processing, "\"Processing\""),
            (WalkthroughStatus::Review, "\"Review\""),
            (WalkthroughStatus::Applied, "\"Applied\""),
            (WalkthroughStatus::Cancelled, "\"Cancelled\""),
        ];
        for (variant, expected) in &variants {
            let json = serde_json::to_string(variant).expect("serialize");
            assert_eq!(
                &json, expected,
                "WalkthroughStatus::{variant:?} serialized incorrectly"
            );
        }
    }

    #[test]
    fn test_event_kind_serialization_roundtrip() {
        let kinds = vec![
            WalkthroughEventKind::AppFocused {
                app_name: "Calculator".to_string(),
                pid: 1234,
                window_title: Some("Calculator".to_string()),
                app_kind: AppKind::Native,
            },
            WalkthroughEventKind::MouseClicked {
                x: 100.0,
                y: 200.0,
                button: MouseButton::Left,
                click_count: 1,
                modifiers: vec![],
                cdp_element: None,
            },
            WalkthroughEventKind::KeyPressed {
                key: "Enter".to_string(),
                modifiers: vec![],
            },
            WalkthroughEventKind::TextCommitted {
                text: "hello".to_string(),
            },
            WalkthroughEventKind::Scrolled {
                delta_y: -3.0,
                x: None,
                y: None,
            },
            WalkthroughEventKind::ScreenshotCaptured {
                path: "/tmp/shot.png".to_string(),
                kind: ScreenshotKind::BeforeClick,
                meta: None,
                image_b64: None,
            },
            WalkthroughEventKind::VlmLabelResolved {
                label: "Submit".to_string(),
            },
            WalkthroughEventKind::Paused,
            WalkthroughEventKind::Resumed,
            WalkthroughEventKind::Stopped,
        ];

        for kind in &kinds {
            let event = WalkthroughEvent {
                id: Uuid::new_v4(),
                timestamp: 1_700_000_000_000,
                kind: kind.clone(),
            };
            let json = serde_json::to_string(&event).expect("serialize");
            let deserialized: WalkthroughEvent = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(deserialized.id, event.id);
        }
    }

    #[test]
    fn test_app_focused_backward_compat_without_app_kind() {
        let json =
            r#"{"type":"AppFocused","app_name":"Calculator","pid":1234,"window_title":null}"#;
        let kind: WalkthroughEventKind = serde_json::from_str(json).unwrap();
        match kind {
            WalkthroughEventKind::AppFocused { app_kind, .. } => {
                assert_eq!(app_kind, AppKind::Native);
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn test_action_kind_serialization_roundtrip() {
        let kinds = vec![
            WalkthroughActionKind::LaunchApp {
                app_name: "Calculator".to_string(),
                app_kind: AppKind::Native,
            },
            WalkthroughActionKind::FocusWindow {
                app_name: "Calculator".to_string(),
                window_title: Some("Calculator".to_string()),
                app_kind: AppKind::Native,
            },
            WalkthroughActionKind::Click {
                x: 100.0,
                y: 200.0,
                button: MouseButton::Left,
                click_count: 1,
            },
            WalkthroughActionKind::TypeText {
                text: "hello".to_string(),
            },
            WalkthroughActionKind::PressKey {
                key: "Enter".to_string(),
                modifiers: vec![],
            },
            WalkthroughActionKind::Scroll { delta_y: -3.0 },
        ];

        for kind in &kinds {
            let json = serde_json::to_string(kind).expect("serialize");
            let _deserialized: WalkthroughActionKind =
                serde_json::from_str(&json).expect("deserialize");
        }
    }

    #[test]
    fn test_target_candidate_serialization_roundtrip() {
        let candidates = vec![
            TargetCandidate::AccessibilityLabel {
                label: "Submit".to_string(),
                role: Some("AXButton".to_string()),
            },
            TargetCandidate::OcrText {
                text: "Submit".to_string(),
            },
            TargetCandidate::ImageCrop {
                path: "/tmp/crop.png".to_string(),
                image_b64: "abc123".to_string(),
            },
            TargetCandidate::Coordinates { x: 100.0, y: 200.0 },
            TargetCandidate::CdpElement {
                text: "Submit".to_string(),
                uid: "e1".to_string(),
            },
        ];

        for candidate in &candidates {
            let json = serde_json::to_string(candidate).expect("serialize");
            let _deserialized: TargetCandidate = serde_json::from_str(&json).expect("deserialize");
        }
    }

    #[test]
    fn test_annotations_default() {
        let annotations = WalkthroughAnnotations::default();
        assert!(annotations.deleted_node_ids.is_empty());
        assert!(annotations.renamed_nodes.is_empty());
        assert!(annotations.target_overrides.is_empty());
        assert!(annotations.variable_promotions.is_empty());
    }

    #[test]
    fn test_storage_create_and_save_session() {
        let dir = std::env::temp_dir()
            .join("clickweave_test_wt")
            .join(Uuid::new_v4().to_string());
        let storage = WalkthroughStorage::new(&dir);

        let session = WalkthroughSession {
            id: Uuid::new_v4(),
            workflow_id: Uuid::new_v4(),
            started_at: 1_700_000_000_000,
            ended_at: None,
            status: WalkthroughStatus::Recording,
            events: vec![],
            actions: vec![],
            warnings: vec![],
        };

        let session_dir = storage
            .create_session_dir(&session)
            .expect("create session dir");
        assert!(session_dir.exists());
        assert!(session_dir.join("artifacts").exists());

        storage
            .save_session(&session_dir, &session)
            .expect("save session");
        assert!(session_dir.join("session.json").exists());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_storage_append_event() {
        let dir = std::env::temp_dir()
            .join("clickweave_test_wt")
            .join(Uuid::new_v4().to_string());
        let storage = WalkthroughStorage::new(&dir);

        let session = WalkthroughSession {
            id: Uuid::new_v4(),
            workflow_id: Uuid::new_v4(),
            started_at: 1_700_000_000_000,
            ended_at: None,
            status: WalkthroughStatus::Recording,
            events: vec![],
            actions: vec![],
            warnings: vec![],
        };

        let session_dir = storage
            .create_session_dir(&session)
            .expect("create session dir");

        let event = WalkthroughEvent {
            id: Uuid::new_v4(),
            timestamp: 1_700_000_000_100,
            kind: WalkthroughEventKind::AppFocused {
                app_name: "Calculator".to_string(),
                pid: 1234,
                window_title: Some("Calculator".to_string()),
                app_kind: AppKind::Native,
            },
        };

        storage
            .append_event(&session_dir, &event)
            .expect("append event");

        let content =
            std::fs::read_to_string(session_dir.join("events.jsonl")).expect("read events.jsonl");
        assert!(content.contains("Calculator"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_storage_read_events_roundtrip() {
        let dir = std::env::temp_dir()
            .join("clickweave_test_wt")
            .join(Uuid::new_v4().to_string());
        let storage = WalkthroughStorage::new(&dir);
        let session = WalkthroughSession::new(Uuid::new_v4());
        let session_dir = storage.create_session_dir(&session).expect("create dir");

        let ev1 = WalkthroughEvent {
            id: Uuid::new_v4(),
            timestamp: 1000,
            kind: WalkthroughEventKind::AppFocused {
                app_name: "Calculator".into(),
                pid: 100,
                window_title: None,
                app_kind: AppKind::Native,
            },
        };
        let ev2 = WalkthroughEvent {
            id: Uuid::new_v4(),
            timestamp: 2000,
            kind: WalkthroughEventKind::KeyPressed {
                key: "return".into(),
                modifiers: vec![],
            },
        };
        storage.append_event(&session_dir, &ev1).expect("append");
        storage.append_event(&session_dir, &ev2).expect("append");

        let events = storage.read_events(&session_dir).expect("read events");
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].id, ev1.id);
        assert_eq!(events[1].id, ev2.id);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_build_action_node_map_1_to_1() {
        let actions = vec![
            WalkthroughAction {
                id: Uuid::new_v4(),
                kind: WalkthroughActionKind::Click {
                    x: 0.0,
                    y: 0.0,
                    button: MouseButton::Left,
                    click_count: 1,
                },
                app_name: None,
                window_title: None,
                target_candidates: vec![],
                artifact_paths: vec![],
                source_event_ids: vec![],
                confidence: ActionConfidence::High,
                warnings: vec![],
                screenshot_meta: None,
            },
            WalkthroughAction {
                id: Uuid::new_v4(),
                kind: WalkthroughActionKind::TypeText {
                    text: "hello".into(),
                },
                app_name: None,
                window_title: None,
                target_candidates: vec![],
                artifact_paths: vec![],
                source_event_ids: vec![],
                confidence: ActionConfidence::High,
                warnings: vec![],
                screenshot_meta: None,
            },
        ];
        let draft = synthesize_draft(&actions, Uuid::new_v4(), "test");
        let map = build_action_node_map(&actions, &draft);

        assert_eq!(map.len(), 2);
        assert_eq!(map[0].action_id, actions[0].id);
        assert_eq!(map[0].node_id, draft.nodes[0].id);
        assert_eq!(map[1].action_id, actions[1].id);
        assert_eq!(map[1].node_id, draft.nodes[1].id);
    }

    #[test]
    fn test_build_action_node_map_empty() {
        let map = build_action_node_map(&[], &crate::Workflow::default());
        assert!(map.is_empty());
    }

    mod normalize_tests {
        use super::*;

        fn make_event(timestamp: u64, kind: WalkthroughEventKind) -> WalkthroughEvent {
            WalkthroughEvent {
                id: Uuid::new_v4(),
                timestamp,
                kind,
            }
        }

        #[test]
        fn test_first_app_focus_becomes_launch_app() {
            let events = vec![make_event(
                1000,
                WalkthroughEventKind::AppFocused {
                    app_name: "Calculator".into(),
                    pid: 100,
                    window_title: Some("Calculator".into()),
                    app_kind: AppKind::Native,
                },
            )];
            let (actions, _warnings) = normalize_events(&events);
            assert_eq!(actions.len(), 1);
            assert!(matches!(
                &actions[0].kind,
                WalkthroughActionKind::LaunchApp { app_name, .. } if app_name == "Calculator"
            ));
            assert_eq!(actions[0].confidence, ActionConfidence::High);
        }

        #[test]
        fn test_repeated_focus_same_app_collapsed() {
            let events = vec![
                make_event(
                    1000,
                    WalkthroughEventKind::AppFocused {
                        app_name: "Calculator".into(),
                        pid: 100,
                        window_title: None,
                        app_kind: AppKind::Native,
                    },
                ),
                make_event(
                    1100,
                    WalkthroughEventKind::AppFocused {
                        app_name: "Calculator".into(),
                        pid: 100,
                        window_title: None,
                        app_kind: AppKind::Native,
                    },
                ),
            ];
            let (actions, _) = normalize_events(&events);
            assert_eq!(actions.len(), 1);
        }

        #[test]
        fn test_refocus_previously_seen_app_becomes_focus_window() {
            let events = vec![
                make_event(
                    1000,
                    WalkthroughEventKind::AppFocused {
                        app_name: "Calculator".into(),
                        pid: 100,
                        window_title: None,
                        app_kind: AppKind::Native,
                    },
                ),
                make_event(
                    2000,
                    WalkthroughEventKind::AppFocused {
                        app_name: "Notes".into(),
                        pid: 200,
                        window_title: None,
                        app_kind: AppKind::Native,
                    },
                ),
                make_event(
                    3000,
                    WalkthroughEventKind::AppFocused {
                        app_name: "Calculator".into(),
                        pid: 100,
                        window_title: None,
                        app_kind: AppKind::Native,
                    },
                ),
            ];
            let (actions, _) = normalize_events(&events);
            assert_eq!(actions.len(), 3);
            assert!(matches!(
                &actions[0].kind,
                WalkthroughActionKind::LaunchApp { .. }
            ));
            assert!(matches!(
                &actions[1].kind,
                WalkthroughActionKind::LaunchApp { .. }
            ));
            assert!(matches!(
                &actions[2].kind,
                WalkthroughActionKind::FocusWindow { .. }
            ));
        }

        #[test]
        fn test_contiguous_text_coalesced() {
            let events = vec![
                make_event(
                    1000,
                    WalkthroughEventKind::TextCommitted { text: "h".into() },
                ),
                make_event(
                    1050,
                    WalkthroughEventKind::TextCommitted { text: "e".into() },
                ),
                make_event(
                    1100,
                    WalkthroughEventKind::TextCommitted { text: "l".into() },
                ),
                make_event(
                    1150,
                    WalkthroughEventKind::TextCommitted { text: "l".into() },
                ),
                make_event(
                    1200,
                    WalkthroughEventKind::TextCommitted { text: "o".into() },
                ),
            ];
            let (actions, _) = normalize_events(&events);
            assert_eq!(actions.len(), 1);
            assert!(matches!(
                &actions[0].kind,
                WalkthroughActionKind::TypeText { text } if text == "hello"
            ));
        }

        #[test]
        fn test_text_broken_by_click() {
            let events = vec![
                make_event(
                    1000,
                    WalkthroughEventKind::TextCommitted { text: "ab".into() },
                ),
                make_event(
                    2000,
                    WalkthroughEventKind::MouseClicked {
                        x: 100.0,
                        y: 200.0,
                        button: MouseButton::Left,
                        click_count: 1,
                        modifiers: vec![],
                        cdp_element: None,
                    },
                ),
                make_event(
                    3000,
                    WalkthroughEventKind::TextCommitted { text: "cd".into() },
                ),
            ];
            let (actions, _) = normalize_events(&events);
            assert_eq!(actions.len(), 3); // TypeText, Click, TypeText
        }

        #[test]
        fn test_text_broken_by_idle_gap() {
            let events = vec![
                make_event(
                    1000,
                    WalkthroughEventKind::TextCommitted { text: "ab".into() },
                ),
                make_event(
                    4000,
                    WalkthroughEventKind::TextCommitted { text: "cd".into() },
                ), // >2s gap
            ];
            let (actions, _) = normalize_events(&events);
            assert_eq!(actions.len(), 2);
        }

        #[test]
        fn test_click_with_nearby_ocr_gets_medium_confidence() {
            let events = vec![
                make_event(
                    1000,
                    WalkthroughEventKind::MouseClicked {
                        x: 100.0,
                        y: 200.0,
                        button: MouseButton::Left,
                        click_count: 1,
                        modifiers: vec![],
                        cdp_element: None,
                    },
                ),
                make_event(
                    1100,
                    WalkthroughEventKind::OcrCaptured {
                        annotations: vec![
                            OcrAnnotation {
                                text: "Submit".into(),
                                x: 102.0,
                                y: 198.0,
                            },
                            OcrAnnotation {
                                text: "Cancel".into(),
                                x: 300.0,
                                y: 198.0,
                            },
                        ],
                        click_x: 100.0,
                        click_y: 200.0,
                    },
                ),
            ];
            let (actions, _) = normalize_events(&events);
            assert_eq!(actions.len(), 1);
            assert!(matches!(
                &actions[0].kind,
                WalkthroughActionKind::Click { .. }
            ));
            assert!(
                actions[0]
                    .target_candidates
                    .iter()
                    .any(|c| matches!(c, TargetCandidate::OcrText { text } if text == "Submit"))
            );
            assert_eq!(actions[0].confidence, ActionConfidence::Medium);
        }

        #[test]
        fn test_click_without_ocr_gets_low_confidence() {
            let events = vec![make_event(
                1000,
                WalkthroughEventKind::MouseClicked {
                    x: 100.0,
                    y: 200.0,
                    button: MouseButton::Left,
                    click_count: 1,
                    modifiers: vec![],
                    cdp_element: None,
                },
            )];
            let (actions, _) = normalize_events(&events);
            assert_eq!(actions.len(), 1);
            assert_eq!(actions[0].confidence, ActionConfidence::Low);
            assert!(
                actions[0]
                    .target_candidates
                    .iter()
                    .any(|c| matches!(c, TargetCandidate::Coordinates { .. }))
            );
        }

        #[test]
        fn test_click_with_vlm_label_gets_medium_confidence() {
            let events = vec![
                make_event(
                    1000,
                    WalkthroughEventKind::MouseClicked {
                        x: 100.0,
                        y: 200.0,
                        button: MouseButton::Left,
                        click_count: 1,
                        modifiers: vec![],
                        cdp_element: None,
                    },
                ),
                make_event(
                    1000,
                    WalkthroughEventKind::VlmLabelResolved {
                        label: "Send".to_string(),
                    },
                ),
            ];
            let (actions, _) = normalize_events(&events);
            assert_eq!(actions.len(), 1);
            assert!(
                actions[0]
                    .target_candidates
                    .iter()
                    .any(|c| matches!(c, TargetCandidate::VlmLabel { label } if label == "Send"))
            );
            // VLM label alone should be at least Medium confidence
            assert!(actions[0].confidence != ActionConfidence::Low);
        }

        #[test]
        fn test_click_with_ax_label_and_vlm_label_keeps_ax_first() {
            let events = vec![
                make_event(
                    1000,
                    WalkthroughEventKind::MouseClicked {
                        x: 100.0,
                        y: 200.0,
                        button: MouseButton::Left,
                        click_count: 1,
                        modifiers: vec![],
                        cdp_element: None,
                    },
                ),
                make_event(
                    1000,
                    WalkthroughEventKind::AccessibilityElementCaptured {
                        label: "Submit".to_string(),
                        role: Some("AXButton".to_string()),
                    },
                ),
                make_event(
                    1000,
                    WalkthroughEventKind::VlmLabelResolved {
                        label: "Submit Button".to_string(),
                    },
                ),
            ];
            let (actions, _) = normalize_events(&events);
            assert_eq!(actions.len(), 1);
            // AX label should be first candidate
            assert!(matches!(
                &actions[0].target_candidates[0],
                TargetCandidate::AccessibilityLabel { label, .. } if label == "Submit"
            ));
            // VLM label should be second
            assert!(matches!(
                &actions[0].target_candidates[1],
                TargetCandidate::VlmLabel { label } if label == "Submit Button"
            ));
            // Actionable AX label means High confidence
            assert_eq!(actions[0].confidence, ActionConfidence::High);
        }

        #[test]
        fn test_cdp_element_resolved_via_vlm_hint_when_ax_is_window_title() {
            let click_id = Uuid::new_v4();
            let events = vec![
                WalkthroughEvent {
                    id: click_id,
                    timestamp: 1000,
                    kind: WalkthroughEventKind::MouseClicked {
                        x: 45.0,
                        y: 473.0,
                        button: MouseButton::Left,
                        click_count: 1,
                        modifiers: vec![],
                        cdp_element: None,
                    },
                },
                make_event(
                    1000,
                    WalkthroughEventKind::CdpSnapshotCaptured {
                        snapshot_text: concat!(
                            "uid=1_0 RootWebArea \"MyApp\"\n",
                            "  uid=1_1 button \"Go back\"\n",
                            "  uid=1_2 treeitem \"Direct Messages\" level=\"1\" selectable\n",
                            "  uid=1_3 button \"Settings\"\n",
                        )
                        .to_string(),
                        click_event_id: click_id,
                    },
                ),
                // AX label is the window title — won't match any single CDP element.
                make_event(
                    1000,
                    WalkthroughEventKind::AccessibilityElementCaptured {
                        label: "MyApp - Main Window".to_string(),
                        role: Some("AXWindow".to_string()),
                    },
                ),
                // VLM label matches the specific element.
                make_event(
                    1000,
                    WalkthroughEventKind::VlmLabelResolved {
                        label: "Direct Messages".to_string(),
                    },
                ),
            ];
            let (actions, _) = normalize_events(&events);
            assert_eq!(actions.len(), 1);
            // CDP element should be the first candidate (highest priority).
            assert!(
                matches!(
                    &actions[0].target_candidates[0],
                    TargetCandidate::CdpElement { text, uid }
                        if text == "Direct Messages" && uid == "1_2"
                ),
                "Expected CdpElement as first candidate, got: {:?}",
                &actions[0].target_candidates
            );
            // CDP element presence means High confidence.
            assert_eq!(actions[0].confidence, ActionConfidence::High);
        }

        #[test]
        fn test_key_pressed_becomes_press_key() {
            let events = vec![make_event(
                1000,
                WalkthroughEventKind::KeyPressed {
                    key: "return".into(),
                    modifiers: vec![],
                },
            )];
            let (actions, _) = normalize_events(&events);
            assert_eq!(actions.len(), 1);
            assert!(matches!(
                &actions[0].kind,
                WalkthroughActionKind::PressKey { key, .. } if key == "return"
            ));
        }

        #[test]
        fn test_scroll_preserved() {
            let events = vec![make_event(
                1000,
                WalkthroughEventKind::Scrolled {
                    delta_y: -5.0,
                    x: Some(100.0),
                    y: Some(200.0),
                },
            )];
            let (actions, _) = normalize_events(&events);
            assert_eq!(actions.len(), 1);
            assert!(matches!(
                &actions[0].kind,
                WalkthroughActionKind::Scroll { delta_y } if *delta_y == -5.0
            ));
        }

        #[test]
        fn test_rapid_scrolls_coalesced() {
            let events = vec![
                make_event(
                    1000,
                    WalkthroughEventKind::Scrolled {
                        delta_y: -2.0,
                        x: Some(100.0),
                        y: Some(200.0),
                    },
                ),
                make_event(
                    1050,
                    WalkthroughEventKind::Scrolled {
                        delta_y: -3.0,
                        x: Some(100.0),
                        y: Some(200.0),
                    },
                ),
                make_event(
                    1100,
                    WalkthroughEventKind::Scrolled {
                        delta_y: -1.0,
                        x: Some(100.0),
                        y: Some(200.0),
                    },
                ),
            ];
            let (actions, _) = normalize_events(&events);
            assert_eq!(actions.len(), 1);
            assert!(matches!(
                &actions[0].kind,
                WalkthroughActionKind::Scroll { delta_y } if *delta_y == -6.0
            ));
        }

        #[test]
        fn test_paused_resumed_stopped_events_skipped() {
            let events = vec![
                make_event(1000, WalkthroughEventKind::Paused),
                make_event(2000, WalkthroughEventKind::Resumed),
                make_event(3000, WalkthroughEventKind::Stopped),
            ];
            let (actions, _) = normalize_events(&events);
            assert!(actions.is_empty());
        }

        #[test]
        fn test_screenshot_events_skipped() {
            let events = vec![make_event(
                1000,
                WalkthroughEventKind::ScreenshotCaptured {
                    path: "/tmp/shot.png".into(),
                    kind: ScreenshotKind::AfterClick,
                    meta: None,
                    image_b64: None,
                },
            )];
            let (actions, _) = normalize_events(&events);
            assert!(actions.is_empty());
        }

        #[test]
        fn test_app_kind_propagated_to_actions() {
            let events = vec![make_event(
                1000,
                WalkthroughEventKind::AppFocused {
                    app_name: "Discord".into(),
                    pid: 100,
                    window_title: None,
                    app_kind: AppKind::ElectronApp,
                },
            )];
            let (actions, _) = normalize_events(&events);
            assert_eq!(actions.len(), 1);
            match &actions[0].kind {
                WalkthroughActionKind::LaunchApp { app_kind, .. } => {
                    assert_eq!(*app_kind, AppKind::ElectronApp);
                }
                _ => panic!("expected LaunchApp"),
            }
        }
    }

    mod synthesis_tests {
        use super::*;
        use crate::{FocusMethod, NodeType};

        fn make_action(kind: WalkthroughActionKind) -> WalkthroughAction {
            WalkthroughAction {
                id: Uuid::new_v4(),
                kind,
                app_name: None,
                window_title: None,
                target_candidates: vec![],
                artifact_paths: vec![],
                source_event_ids: vec![],
                confidence: ActionConfidence::High,
                warnings: vec![],
                screenshot_meta: None,
            }
        }

        #[test]
        fn test_empty_actions_produces_empty_workflow() {
            let wf = synthesize_draft(&[], Uuid::new_v4(), "Test");
            assert!(wf.nodes.is_empty());
            assert!(wf.edges.is_empty());
        }

        #[test]
        fn test_launch_app_becomes_focus_window_node() {
            let actions = vec![make_action(WalkthroughActionKind::LaunchApp {
                app_name: "Calculator".into(),
                app_kind: AppKind::Native,
            })];
            let wf = synthesize_draft(&actions, Uuid::new_v4(), "Test");
            assert_eq!(wf.nodes.len(), 1);
            assert!(matches!(
                &wf.nodes[0].node_type,
                NodeType::FocusWindow(p) if p.method == FocusMethod::AppName && p.value.as_deref() == Some("Calculator")
            ));
            assert_eq!(wf.nodes[0].name, "Launch Calculator");
        }

        #[test]
        fn test_app_kind_propagated_to_focus_window_node() {
            let actions = vec![make_action(WalkthroughActionKind::LaunchApp {
                app_name: "Discord".into(),
                app_kind: AppKind::ElectronApp,
            })];
            let wf = synthesize_draft(&actions, Uuid::new_v4(), "Test");
            assert_eq!(wf.nodes.len(), 1);
            match &wf.nodes[0].node_type {
                NodeType::FocusWindow(p) => {
                    assert_eq!(p.app_kind, AppKind::ElectronApp);
                }
                _ => panic!("expected FocusWindow"),
            }
        }

        #[test]
        fn test_click_with_ocr_target_uses_target_field() {
            let mut action = make_action(WalkthroughActionKind::Click {
                x: 100.0,
                y: 200.0,
                button: MouseButton::Left,
                click_count: 1,
            });
            action.target_candidates = vec![
                TargetCandidate::OcrText {
                    text: "Submit".into(),
                },
                TargetCandidate::Coordinates { x: 100.0, y: 200.0 },
            ];
            let wf = synthesize_draft(&[action], Uuid::new_v4(), "Test");
            assert_eq!(wf.nodes.len(), 1);
            assert!(matches!(
                &wf.nodes[0].node_type,
                NodeType::Click(p) if p.target.as_deref() == Some("Submit")
            ));
            assert_eq!(wf.nodes[0].name, "Click 'Submit'");
        }

        #[test]
        fn test_click_coordinates_only() {
            let action = make_action(WalkthroughActionKind::Click {
                x: 100.0,
                y: 200.0,
                button: MouseButton::Left,
                click_count: 1,
            });
            let wf = synthesize_draft(&[action], Uuid::new_v4(), "Test");
            assert!(matches!(
                &wf.nodes[0].node_type,
                NodeType::Click(p) if p.target.is_none() && p.x == Some(100.0)
            ));
        }

        #[test]
        fn test_linear_edges_between_nodes() {
            let actions = vec![
                make_action(WalkthroughActionKind::LaunchApp {
                    app_name: "App".into(),
                    app_kind: AppKind::Native,
                }),
                make_action(WalkthroughActionKind::TypeText {
                    text: "hello".into(),
                }),
                make_action(WalkthroughActionKind::PressKey {
                    key: "return".into(),
                    modifiers: vec![],
                }),
            ];
            let wf = synthesize_draft(&actions, Uuid::new_v4(), "Test");
            assert_eq!(wf.nodes.len(), 3);
            assert_eq!(wf.edges.len(), 2);
            assert_eq!(wf.edges[0].from, wf.nodes[0].id);
            assert_eq!(wf.edges[0].to, wf.nodes[1].id);
            assert_eq!(wf.edges[1].from, wf.nodes[1].id);
            assert_eq!(wf.edges[1].to, wf.nodes[2].id);
        }

        #[test]
        fn test_nodes_auto_positioned_vertically() {
            let actions = vec![
                make_action(WalkthroughActionKind::TypeText { text: "a".into() }),
                make_action(WalkthroughActionKind::TypeText { text: "b".into() }),
            ];
            let wf = synthesize_draft(&actions, Uuid::new_v4(), "Test");
            assert!(wf.nodes[1].position.y > wf.nodes[0].position.y);
        }

        #[test]
        fn test_scroll_delta_cast_to_i32() {
            let action = make_action(WalkthroughActionKind::Scroll { delta_y: -5.7 });
            let wf = synthesize_draft(&[action], Uuid::new_v4(), "Test");
            assert!(matches!(&wf.nodes[0].node_type, NodeType::Scroll(p) if p.delta_y == -5));
        }
    }
}
