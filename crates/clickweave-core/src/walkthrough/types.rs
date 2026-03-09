use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::MouseButton;
use crate::storage::now_millis;

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
    CdpClickResolved {
        name: String,
        role: Option<String>,
        href: Option<String>,
        parent_role: Option<String>,
        parent_name: Option<String>,
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
    pub(crate) fn new(
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
    /// Element captured via Chrome DevTools Protocol click listener.
    CdpElement {
        name: String,
        role: Option<String>,
        href: Option<String>,
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
            Self::CdpElement { name, .. } => Some(name),
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

/// Build a 1:1 action->node mapping for a deterministic draft where
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
