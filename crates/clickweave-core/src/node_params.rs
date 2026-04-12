use crate::AppKind;
use crate::output_schema::VerificationMethod;
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

// --- CDP target enum ---

/// Distinguishes how a CDP element target was produced, so the executor can
/// choose the right resolution strategy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(tag = "kind", content = "value")]
pub enum CdpTarget {
    /// Precise element name from `cdp_find_elements` or walkthrough recording.
    ExactLabel(String),
    /// Semantic description (e.g. "the message input field") — always resolved
    /// via snapshot + LLM at execution time.
    Intent(String),
    /// Concrete DOM UID resolved at execution time (for Run mode / decision cache).
    ResolvedUid(String),
}

impl Default for CdpTarget {
    fn default() -> Self {
        Self::Intent(String::new())
    }
}

impl CdpTarget {
    /// The inner string regardless of variant.
    pub fn as_str(&self) -> &str {
        match self {
            Self::ExactLabel(s) | Self::Intent(s) | Self::ResolvedUid(s) => s,
        }
    }
}

// --- CDP node params ---

/// Macro to generate backward-compatible deserialization for CDP params structs.
/// Old format `{"uid": "..."}` deserializes as `CdpTarget::ExactLabel(...)`.
macro_rules! impl_cdp_target_deser {
    ($ty:ident { $($extra_field:ident : $extra_ty:ty),* $(,)? }) => {
        impl<'de> Deserialize<'de> for $ty {
            fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                #[derive(Deserialize)]
                struct Raw {
                    #[serde(default)]
                    target: Option<CdpTarget>,
                    #[serde(default)]
                    uid: Option<String>,
                    $(
                        #[serde(default)]
                        $extra_field: $extra_ty,
                    )*
                    #[serde(default)]
                    verification_method: Option<VerificationMethod>,
                    #[serde(default)]
                    verification_assertion: Option<String>,
                }
                let raw = Raw::deserialize(deserializer)?;
                Ok(Self {
                    target: match (raw.target, raw.uid) {
                        (Some(t), _) => t,
                        (None, Some(uid)) => CdpTarget::ExactLabel(uid),
                        (None, None) => CdpTarget::default(),
                    },
                    $( $extra_field: raw.$extra_field, )*
                    verification_method: raw.verification_method,
                    verification_assertion: raw.verification_assertion,
                })
            }
        }
    };
}

#[derive(Debug, Clone, Default, Serialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct CdpClickParams {
    pub target: CdpTarget,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verification_method: Option<VerificationMethod>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verification_assertion: Option<String>,
}

impl_cdp_target_deser!(CdpClickParams {});

#[derive(Debug, Clone, Default, Serialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct CdpHoverParams {
    pub target: CdpTarget,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verification_method: Option<VerificationMethod>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verification_assertion: Option<String>,
}

impl_cdp_target_deser!(CdpHoverParams {});

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct CdpFillParams {
    pub uid: String,
    pub value: String,
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
    Cancelled,
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
    fn click_params_default_has_no_verification() {
        let params = ClickParams::default();
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

    #[test]
    fn cdp_target_serde_roundtrip() {
        let target = CdpTarget::ExactLabel("Friends".into());
        let json = serde_json::to_string(&target).unwrap();
        assert!(json.contains("\"kind\":\"ExactLabel\""));
        let back: CdpTarget = serde_json::from_str(&json).unwrap();
        assert_eq!(back, CdpTarget::ExactLabel("Friends".into()));
    }

    #[test]
    fn cdp_target_as_str() {
        assert_eq!(CdpTarget::ExactLabel("a".into()).as_str(), "a");
        assert_eq!(CdpTarget::Intent("b".into()).as_str(), "b");
        assert_eq!(CdpTarget::ResolvedUid("c".into()).as_str(), "c");
    }

    #[test]
    fn cdp_click_params_new_format_roundtrip() {
        let params = CdpClickParams {
            target: CdpTarget::Intent("message input".into()),
            ..Default::default()
        };
        let json = serde_json::to_string(&params).unwrap();
        let back: CdpClickParams = serde_json::from_str(&json).unwrap();
        assert_eq!(back.target, CdpTarget::Intent("message input".into()));
    }

    #[test]
    fn cdp_click_params_legacy_uid_deserializes_as_exact_label() {
        let json = r#"{"uid": "Friends"}"#;
        let params: CdpClickParams = serde_json::from_str(json).unwrap();
        assert_eq!(params.target, CdpTarget::ExactLabel("Friends".into()));
    }

    #[test]
    fn cdp_hover_params_legacy_uid_deserializes_as_exact_label() {
        let json = r#"{"uid": "Submit"}"#;
        let params: CdpHoverParams = serde_json::from_str(json).unwrap();
        assert_eq!(params.target, CdpTarget::ExactLabel("Submit".into()));
    }

    #[test]
    fn cdp_click_params_legacy_preserves_verification_fields() {
        let json = r#"{"uid": "OK", "verification_assertion": "button visible"}"#;
        let params: CdpClickParams = serde_json::from_str(json).unwrap();
        assert_eq!(params.target, CdpTarget::ExactLabel("OK".into()));
        assert_eq!(
            params.verification_assertion.as_deref(),
            Some("button visible")
        );
    }

    #[test]
    fn cdp_click_params_missing_both_fields_defaults_to_intent() {
        let json = r#"{}"#;
        let params: CdpClickParams = serde_json::from_str(json).unwrap();
        assert_eq!(params.target, CdpTarget::default());
        assert!(matches!(params.target, CdpTarget::Intent(ref s) if s.is_empty()));
    }
}
