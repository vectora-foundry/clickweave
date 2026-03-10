use crate::walkthrough::AppKind;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

// --- Parameter structs ---

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct AiStepParams {
    pub prompt: String,
    pub button_text: Option<String>,
    pub template_image: Option<String>,
    pub max_tool_calls: Option<u32>,
    pub allowed_tools: Option<Vec<String>>,
    #[serde(default)]
    pub timeout_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct TakeScreenshotParams {
    pub mode: ScreenshotMode,
    pub target: Option<String>,
    pub include_ocr: bool,
}

impl Default for TakeScreenshotParams {
    fn default() -> Self {
        Self {
            mode: ScreenshotMode::Screen,
            target: None,
            include_ocr: true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub enum ScreenshotMode {
    Screen,
    Window,
    Region,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct FindTextParams {
    pub search_text: String,
    pub match_mode: MatchMode,
    pub scope: Option<String>,
    pub select_result: Option<String>,
}

impl Default for FindTextParams {
    fn default() -> Self {
        Self {
            search_text: String::new(),
            match_mode: MatchMode::Contains,
            scope: None,
            select_result: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub enum MatchMode {
    Contains,
    Exact,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct FindImageParams {
    pub template_image: Option<String>,
    pub threshold: f64,
    pub max_results: u32,
}

impl Default for FindImageParams {
    fn default() -> Self {
        Self {
            template_image: None,
            threshold: 0.88,
            max_results: 3,
        }
    }
}

/// macOS window control button (traffic light) actions.
///
/// At execution time these are resolved to window-relative clicks by
/// querying the focused window's position and applying a fixed pixel offset.
/// This is more reliable than keyboard shortcuts — e.g. Cmd+W closes a tab
/// in tabbed apps, not the window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub enum WindowControlAction {
    Close,
    Minimize,
    Maximize,
    /// Green button in "zoom" mode — resizes/maximizes the window without
    /// entering full screen. Same physical button as Maximize but different
    /// macOS subrole (`AXZoomButton` vs `AXFullScreenButton`).
    Zoom,
}

impl WindowControlAction {
    /// Pixel offset from the window's top-left corner to the button center.
    /// Standard macOS traffic light positions (consistent across versions).
    pub fn window_offset(self) -> (f64, f64) {
        match self {
            Self::Close => (14.0, 14.0),
            Self::Minimize => (34.0, 14.0),
            Self::Maximize | Self::Zoom => (54.0, 14.0),
        }
    }

    pub fn display_name(self) -> &'static str {
        match self {
            Self::Close => "Close window",
            Self::Minimize => "Minimize window",
            Self::Maximize => "Maximize window",
            Self::Zoom => "Zoom window",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(tag = "type")]
pub enum ClickTarget {
    Text {
        text: String,
    },
    CdpElement {
        name: String,
        role: Option<String>,
        href: Option<String>,
        parent_role: Option<String>,
        parent_name: Option<String>,
    },
    /// macOS window control button — resolved at execution time to
    /// window-relative coordinates via `list_windows`.
    WindowControl {
        action: WindowControlAction,
    },
}

impl ClickTarget {
    /// Return the human-readable text for this target, regardless of variant.
    pub fn text(&self) -> &str {
        match self {
            Self::Text { text } => text,
            Self::CdpElement { name, .. } => name,
            Self::WindowControl { action } => action.display_name(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct ClickParams {
    pub target: Option<ClickTarget>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub template_image: Option<String>,
    pub x: Option<f64>,
    pub y: Option<f64>,
    pub button: MouseButton,
    pub click_count: u32,
}

impl Default for ClickParams {
    fn default() -> Self {
        Self {
            target: None,
            template_image: None,
            x: None,
            y: None,
            button: MouseButton::Left,
            click_count: 1,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub enum MouseButton {
    Left,
    Right,
    Center,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct TypeTextParams {
    pub text: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct PressKeyParams {
    pub key: String,
    pub modifiers: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct ScrollParams {
    pub delta_y: i32,
    pub x: Option<f64>,
    pub y: Option<f64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct ListWindowsParams {
    pub app_name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct FocusWindowParams {
    pub method: FocusMethod,
    pub value: Option<String>,
    pub bring_to_front: bool,
    #[serde(default)]
    pub app_kind: AppKind,
}

impl Default for FocusWindowParams {
    fn default() -> Self {
        Self {
            method: FocusMethod::AppName,
            value: None,
            bring_to_front: true,
            app_kind: AppKind::Native,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub enum FocusMethod {
    WindowId,
    AppName,
    Pid,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct McpToolCallParams {
    pub tool_name: String,
    pub arguments: Value,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct AppDebugKitParams {
    pub operation_name: String,
    pub parameters: Value,
}

// --- Control flow parameter structs ---

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct IfParams {
    pub condition: Condition,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct SwitchParams {
    /// Evaluated in order; first matching case wins.
    pub cases: Vec<SwitchCase>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct SwitchCase {
    /// Label shown on the edge, e.g. "Has error".
    pub name: String,
    pub condition: Condition,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct LoopParams {
    /// Loop exits when this condition becomes true.
    /// NOTE: Uses do-while semantics — the exit condition is NOT checked on the
    /// first iteration (iteration 0). The loop body always runs at least once.
    /// This is intentional for UI automation where the common pattern is
    /// "try action, check if it worked, loop if not."
    pub exit_condition: Condition,
    /// Safety cap to prevent infinite loops. Default: 100.
    /// If max_iterations is hit, the loop exits with a warning trace event
    /// (loop_exited with reason "max_iterations"), which likely indicates
    /// something unexpected happened.
    pub max_iterations: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct EndLoopParams {
    /// Explicit pairing with the Loop node. Stored as UUID rather than inferred
    /// from graph structure for safety and simpler validation.
    /// When EndLoop is reached during execution, the walker jumps directly to
    /// this Loop node, which then re-evaluates its exit condition.
    pub loop_id: Uuid,
}

// --- Condition system ---
// Used by If, Switch, and Loop nodes to evaluate simple comparisons.
// Conditions reference runtime variables produced by upstream nodes.

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct Condition {
    pub left: ValueRef,
    pub operator: Operator,
    pub right: ValueRef,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(tag = "type")]
pub enum ValueRef {
    Variable { name: String },
    Literal { value: LiteralValue },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(tag = "type")]
pub enum LiteralValue {
    String { value: String },
    Number { value: f64 },
    Bool { value: bool },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub enum Operator {
    Equals,
    NotEquals,
    GreaterThan,
    LessThan,
    GreaterThanOrEqual,
    LessThanOrEqual,
    Contains,
    NotContains,
    IsEmpty,
    IsNotEmpty,
}

// --- Execution mode ---

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub enum ExecutionMode {
    #[default]
    Test,
    Run,
}

// --- Trace & check types ---

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub enum TraceLevel {
    Off,
    #[default]
    Minimal,
    Full,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub enum NodeRole {
    #[default]
    Default,
    Verification,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub enum CheckType {
    TextPresent,
    TemplateFound,
    WindowTitleMatches,
    ScreenshotMatch,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub enum CheckVerdict {
    Pass,
    Fail,
    Warn,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct CheckResult {
    pub check_name: String,
    pub check_type: CheckType,
    pub verdict: CheckVerdict,
    pub reasoning: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct NodeVerdict {
    pub node_id: Uuid,
    pub node_name: String,
    pub check_results: Vec<CheckResult>,
    pub expected_outcome_verdict: Option<CheckResult>,
}

// --- Run types ---

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub enum RunStatus {
    #[default]
    Ok,
    Failed,
    Stopped,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct NodeRun {
    pub run_id: Uuid,
    pub node_id: Uuid,
    #[serde(default)]
    pub node_name: String,
    #[serde(default)]
    pub execution_dir: String,
    pub started_at: u64,
    pub ended_at: Option<u64>,
    pub status: RunStatus,
    pub trace_level: TraceLevel,
    pub events: Vec<TraceEvent>,
    pub artifacts: Vec<Artifact>,
    pub observed_summary: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct TraceEvent {
    pub timestamp: u64,
    pub event_type: String,
    pub payload: Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub enum ArtifactKind {
    Screenshot,
    Ocr,
    TemplateMatch,
    Log,
    Other,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct Artifact {
    pub artifact_id: Uuid,
    pub kind: ArtifactKind,
    pub path: String,
    pub metadata: Value,
    pub overlays: Vec<Value>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn click_target_text_serde_roundtrip() {
        let target = ClickTarget::Text {
            text: "Submit".into(),
        };
        let json = serde_json::to_string(&target).unwrap();
        assert!(json.contains("\"type\":\"Text\""));
        let back: ClickTarget = serde_json::from_str(&json).unwrap();
        assert!(matches!(back, ClickTarget::Text { text } if text == "Submit"));
    }

    #[test]
    fn click_target_cdp_element_serde_roundtrip() {
        let target = ClickTarget::CdpElement {
            name: "Friends".into(),
            role: Some("link".into()),
            href: Some("https://example.com".into()),
            parent_role: None,
            parent_name: None,
        };
        let json = serde_json::to_string(&target).unwrap();
        assert!(json.contains("\"type\":\"CdpElement\""));
        let back: ClickTarget = serde_json::from_str(&json).unwrap();
        assert!(
            matches!(back, ClickTarget::CdpElement { name, role, href, .. }
            if name == "Friends" && role.as_deref() == Some("link") && href.as_deref() == Some("https://example.com"))
        );
    }

    #[test]
    fn click_target_text_method() {
        let text = ClickTarget::Text {
            text: "Submit".into(),
        };
        assert_eq!(text.text(), "Submit");

        let cdp = ClickTarget::CdpElement {
            name: "Friends".into(),
            role: None,
            href: None,
            parent_role: None,
            parent_name: None,
        };
        assert_eq!(cdp.text(), "Friends");
    }

    #[test]
    fn click_params_with_click_target_serde() {
        let params = ClickParams {
            target: Some(ClickTarget::CdpElement {
                name: "Friends".into(),
                role: Some("link".into()),
                href: None,
                parent_role: None,
                parent_name: None,
            }),
            ..Default::default()
        };
        let json = serde_json::to_string(&params).unwrap();
        let back: ClickParams = serde_json::from_str(&json).unwrap();
        assert!(matches!(
            back.target,
            Some(ClickTarget::CdpElement { ref name, .. }) if name == "Friends"
        ));
    }
}
