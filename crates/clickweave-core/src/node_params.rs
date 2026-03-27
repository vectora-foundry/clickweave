use crate::AppKind;
use crate::output_schema::{OutputRef, VerificationMethod};
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_ref: Option<OutputRef>,
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

/// Standard macOS traffic light button Y offset (center of button from window top).
/// Based on the default NSWindow title bar. Custom title bars (e.g. Electron apps
/// with `titleBarStyle: 'hidden'`) may use different positions.
const TRAFFIC_LIGHT_Y: f64 = 14.0;
/// X offsets for close / minimize / maximize button centers from window left edge.
const TRAFFIC_LIGHT_CLOSE_X: f64 = 14.0;
const TRAFFIC_LIGHT_MINIMIZE_X: f64 = 34.0;
const TRAFFIC_LIGHT_MAXIMIZE_X: f64 = 54.0;

impl WindowControlAction {
    /// Pixel offset from the window's top-left corner to the button center.
    ///
    /// These are standard macOS traffic light positions for the default `NSWindow`
    /// title bar. They are stable across macOS versions (10.x through 15.x) but
    /// may be incorrect for apps with custom title bars or hidden traffic lights.
    pub fn window_offset(self) -> (f64, f64) {
        match self {
            Self::Close => (TRAFFIC_LIGHT_CLOSE_X, TRAFFIC_LIGHT_Y),
            Self::Minimize => (TRAFFIC_LIGHT_MINIMIZE_X, TRAFFIC_LIGHT_Y),
            Self::Maximize | Self::Zoom => (TRAFFIC_LIGHT_MAXIMIZE_X, TRAFFIC_LIGHT_Y),
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
    Text { text: String },
    Coordinates { x: f64, y: f64 },
    WindowControl { action: WindowControlAction },
}

impl ClickTarget {
    pub fn text(&self) -> &str {
        match self {
            Self::Text { text } => text,
            Self::Coordinates { .. } => "",
            Self::WindowControl { action } => action.display_name(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct ClickParams {
    pub target: Option<ClickTarget>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_ref: Option<OutputRef>,
    pub button: MouseButton,
    pub click_count: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verification_method: Option<VerificationMethod>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verification_assertion: Option<String>,
}

impl Default for ClickParams {
    fn default() -> Self {
        Self {
            target: None,
            target_ref: None,
            button: MouseButton::Left,
            click_count: 1,
            verification_method: None,
            verification_assertion: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct HoverParams {
    pub target: Option<ClickTarget>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_ref: Option<OutputRef>,
    pub dwell_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verification_method: Option<VerificationMethod>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verification_assertion: Option<String>,
}

impl Default for HoverParams {
    fn default() -> Self {
        Self {
            target: None,
            target_ref: None,
            dwell_ms: 500,
            verification_method: None,
            verification_assertion: None,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text_ref: Option<OutputRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verification_method: Option<VerificationMethod>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verification_assertion: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct PressKeyParams {
    pub key: String,
    pub modifiers: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verification_method: Option<VerificationMethod>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verification_assertion: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct ScrollParams {
    pub delta_y: i32,
    pub x: Option<f64>,
    pub y: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verification_method: Option<VerificationMethod>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verification_assertion: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct FocusWindowParams {
    pub method: FocusMethod,
    pub value: Option<String>,
    pub bring_to_front: bool,
    #[serde(default)]
    pub app_kind: AppKind,
    #[serde(default)]
    pub chrome_profile_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value_ref: Option<OutputRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verification_method: Option<VerificationMethod>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verification_assertion: Option<String>,
}

impl Default for FocusWindowParams {
    fn default() -> Self {
        Self {
            method: FocusMethod::AppName,
            value: None,
            bring_to_front: true,
            app_kind: AppKind::Native,
            chrome_profile_id: None,
            value_ref: None,
            verification_method: None,
            verification_assertion: None,
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

// --- New native node params ---

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct DragParams {
    pub from_x: Option<f64>,
    pub from_y: Option<f64>,
    pub to_x: Option<f64>,
    pub to_y: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_ref: Option<OutputRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to_ref: Option<OutputRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verification_method: Option<VerificationMethod>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verification_assertion: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct LaunchAppParams {
    pub app_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verification_method: Option<VerificationMethod>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verification_assertion: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct QuitAppParams {
    pub app_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verification_method: Option<VerificationMethod>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verification_assertion: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct FindAppParams {
    pub search: String,
}

// --- CDP node params ---

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct CdpClickParams {
    pub uid: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verification_method: Option<VerificationMethod>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verification_assertion: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct CdpHoverParams {
    pub uid: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verification_method: Option<VerificationMethod>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verification_assertion: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct CdpFillParams {
    pub uid: String,
    pub value: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value_ref: Option<OutputRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verification_method: Option<VerificationMethod>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verification_assertion: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct CdpTypeParams {
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text_ref: Option<OutputRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verification_method: Option<VerificationMethod>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verification_assertion: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct CdpPressKeyParams {
    pub key: String,
    #[serde(default)]
    pub modifiers: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verification_method: Option<VerificationMethod>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verification_assertion: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct CdpNavigateParams {
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url_ref: Option<OutputRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verification_method: Option<VerificationMethod>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verification_assertion: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct CdpNewPageParams {
    #[serde(default)]
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url_ref: Option<OutputRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verification_method: Option<VerificationMethod>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verification_assertion: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct CdpClosePageParams {
    #[serde(default)]
    pub page_index: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verification_method: Option<VerificationMethod>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verification_assertion: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct CdpSelectPageParams {
    pub page_index: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verification_method: Option<VerificationMethod>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verification_assertion: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct CdpWaitParams {
    pub text: String,
    #[serde(default = "default_cdp_wait_timeout")]
    pub timeout_ms: u64,
}

fn default_cdp_wait_timeout() -> u64 {
    10_000
}

impl Default for CdpWaitParams {
    fn default() -> Self {
        Self {
            text: String::new(),
            timeout_ms: default_cdp_wait_timeout(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct CdpHandleDialogParams {
    pub accept: bool,
    #[serde(default)]
    pub prompt_text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verification_method: Option<VerificationMethod>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verification_assertion: Option<String>,
}

impl Default for CdpHandleDialogParams {
    fn default() -> Self {
        Self {
            accept: true,
            prompt_text: None,
            verification_method: None,
            verification_assertion: None,
        }
    }
}

// --- Generic node params ---

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
    pub left: crate::output_schema::OutputRef,
    pub operator: Operator,
    pub right: crate::output_schema::ConditionValue,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(tag = "type")]
pub enum LiteralValue {
    String { value: String },
    Number { value: f64 },
    Bool { value: bool },
}

impl LiteralValue {
    /// Convert to a serde_json::Value.
    pub fn to_json_value(&self) -> serde_json::Value {
        match self {
            LiteralValue::String { value } => serde_json::Value::String(value.clone()),
            LiteralValue::Number { value } => serde_json::Number::from_f64(*value)
                .map(serde_json::Value::Number)
                .unwrap_or(serde_json::Value::Null),
            LiteralValue::Bool { value } => serde_json::Value::Bool(*value),
        }
    }
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
    fn click_target_coordinates_serde_roundtrip() {
        let target = ClickTarget::Coordinates { x: 100.0, y: 200.0 };
        let json = serde_json::to_string(&target).unwrap();
        assert!(json.contains("\"type\":\"Coordinates\""));
        let back: ClickTarget = serde_json::from_str(&json).unwrap();
        assert!(
            matches!(back, ClickTarget::Coordinates { x, y } if (x - 100.0).abs() < f64::EPSILON && (y - 200.0).abs() < f64::EPSILON)
        );
    }

    #[test]
    fn click_target_text_method() {
        let text = ClickTarget::Text {
            text: "Submit".into(),
        };
        assert_eq!(text.text(), "Submit");

        let coords = ClickTarget::Coordinates { x: 10.0, y: 20.0 };
        assert_eq!(coords.text(), "");
    }

    #[test]
    fn click_params_default_has_no_refs() {
        let params = ClickParams::default();
        assert!(params.target_ref.is_none());
        assert!(params.verification_method.is_none());
        assert!(params.verification_assertion.is_none());
    }

    #[test]
    fn cdp_wait_params_default_timeout() {
        let params = CdpWaitParams::default();
        assert_eq!(params.timeout_ms, 10_000);
    }

    #[test]
    fn cdp_handle_dialog_params_default_accept() {
        let params = CdpHandleDialogParams::default();
        assert!(params.accept);
        assert!(params.prompt_text.is_none());
    }
}
