use crate::AppKind;
use crate::output_schema::{HasVerification, VerificationConfig, VerificationMethod};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

// --- Verification helpers ---

/// Generate a default-tolerant `Deserialize` impl + [`HasVerification`] impl
/// for an action params struct that carries a flattened [`VerificationConfig`]
/// as its `verification` field.
///
/// The derived `Deserialize` on the struct itself would also work, but going
/// through a `Raw` helper lets the struct use `#[serde(default)]`-style
/// semantics across *all* fields without listing `#[serde(default)]` on every
/// one, and guarantees that missing-everywhere JSON produces the struct's
/// `Default`. It also lets the [`HasVerification`] impl live inside the
/// macro so there's no chance of drift between the data layout and the
/// accessor.
macro_rules! impl_verification_deser {
    (
        $ty:ident {
            $( $field:ident : $field_ty:ty = $default:expr ),* $(,)?
        }
    ) => {
        impl<'de> Deserialize<'de> for $ty {
            fn deserialize<D: serde::Deserializer<'de>>(
                deserializer: D,
            ) -> Result<Self, D::Error> {
                #[derive(Deserialize)]
                struct Raw {
                    $(
                        #[serde(default)]
                        $field: Option<$field_ty>,
                    )*
                    #[serde(default)]
                    verification_method: Option<VerificationMethod>,
                    #[serde(default)]
                    verification_assertion: Option<String>,
                }
                let raw = Raw::deserialize(deserializer)?;
                let verification = VerificationConfig {
                    verification_method: raw.verification_method,
                    verification_assertion: raw.verification_assertion,
                };
                Ok($ty {
                    $( $field: raw.$field.unwrap_or_else(|| $default), )*
                    verification,
                })
            }
        }

        impl HasVerification for $ty {
            fn verification(&self) -> Option<&VerificationConfig> {
                if self.verification.is_empty() {
                    None
                } else {
                    Some(&self.verification)
                }
            }
        }
    };
}

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

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct FindTextParams {
    #[serde(default)]
    pub search_text: String,
    #[serde(default)]
    pub scope: Option<String>,
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

#[derive(Debug, Clone, Default, Serialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct ClickParams {
    #[serde(default)]
    pub target: Option<ClickTarget>,
    pub button: MouseButton,
    pub click_count: u32,
    #[serde(flatten, default)]
    pub verification: VerificationConfig,
}

impl_verification_deser!(
    ClickParams {
        target: Option<ClickTarget> = None,
        button: MouseButton = MouseButton::Left,
        click_count: u32 = 1,
    }
);

#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct HoverParams {
    #[serde(default)]
    pub target: Option<ClickTarget>,
    pub dwell_ms: u64,
    #[serde(flatten, default)]
    pub verification: VerificationConfig,
}

impl Default for HoverParams {
    fn default() -> Self {
        Self {
            target: None,
            dwell_ms: 500,
            verification: VerificationConfig::default(),
        }
    }
}

impl_verification_deser!(
    HoverParams {
        target: Option<ClickTarget> = None,
        dwell_ms: u64 = 500,
    }
);

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub enum MouseButton {
    #[default]
    Left,
    Right,
    Center,
}

#[derive(Debug, Clone, Default, Serialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct TypeTextParams {
    pub text: String,
    #[serde(flatten, default)]
    pub verification: VerificationConfig,
}

impl_verification_deser!(TypeTextParams {
    text: String = String::new(),
});

#[derive(Debug, Clone, Default, Serialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct PressKeyParams {
    pub key: String,
    #[serde(default)]
    pub modifiers: Vec<String>,
    #[serde(flatten, default)]
    pub verification: VerificationConfig,
}

impl_verification_deser!(
    PressKeyParams {
        key: String = String::new(),
        modifiers: Vec<String> = Vec::new(),
    }
);

#[derive(Debug, Clone, Default, Serialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct ScrollParams {
    pub delta_y: i32,
    #[serde(default)]
    pub x: Option<f64>,
    #[serde(default)]
    pub y: Option<f64>,
    #[serde(flatten, default)]
    pub verification: VerificationConfig,
}

impl_verification_deser!(
    ScrollParams {
        delta_y: i32 = 0,
        x: Option<f64> = None,
        y: Option<f64> = None,
    }
);

/// Typed target for `FocusWindowParams`.
///
/// Replaces the previous stringly-typed `method: FocusMethod` + `value: Option<String>`
/// pair. Serialized as a flattened tagged enum so the on-disk shape keeps the
/// `method` / `value` keys at the top level of `FocusWindowParams`.
///
/// Legacy workflow files that stored the window id or pid as a stringified
/// number still deserialize correctly via the custom `Deserialize` impl on
/// `FocusWindowParams`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(tag = "method", content = "value")]
pub enum FocusTarget {
    AppName(String),
    WindowId(u64),
    Pid(u32),
}

impl Default for FocusTarget {
    /// Unconfigured focus targets default to an empty app-name string. This
    /// keeps the UI editor — which enumerates `AppName | WindowId | Pid` —
    /// and the `focus_window` tool mapping (empty name produces `args: {}`)
    /// behavior-compatible with the legacy `FocusMethod::AppName + value: None`
    /// shape.
    fn default() -> Self {
        FocusTarget::AppName(String::new())
    }
}

#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct FocusWindowParams {
    #[serde(flatten)]
    pub target: FocusTarget,
    pub bring_to_front: bool,
    #[serde(default)]
    pub app_kind: AppKind,
    #[serde(default)]
    pub chrome_profile_id: Option<String>,
    #[serde(flatten, default)]
    pub verification: VerificationConfig,
}

impl Default for FocusWindowParams {
    fn default() -> Self {
        Self {
            target: FocusTarget::default(),
            bring_to_front: true,
            app_kind: AppKind::Native,
            chrome_profile_id: None,
            verification: VerificationConfig::default(),
        }
    }
}

impl<'de> Deserialize<'de> for FocusWindowParams {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Raw {
            // Legacy + new: method + value carry the target. `value` can be
            // either a string (AppName, or legacy stringified u64/u32) or a
            // number (new shape for WindowId/Pid).
            #[serde(default)]
            method: Option<String>,
            #[serde(default)]
            value: serde_json::Value,
            #[serde(default = "bring_to_front_default")]
            bring_to_front: bool,
            #[serde(default)]
            app_kind: AppKind,
            #[serde(default)]
            chrome_profile_id: Option<String>,
            #[serde(default)]
            verification_method: Option<VerificationMethod>,
            #[serde(default)]
            verification_assertion: Option<String>,
        }

        fn bring_to_front_default() -> bool {
            true
        }

        let raw = Raw::deserialize(deserializer)?;

        let target = parse_focus_target(raw.method.as_deref(), &raw.value)
            .map_err(serde::de::Error::custom)?;

        let verification = VerificationConfig {
            verification_method: raw.verification_method,
            verification_assertion: raw.verification_assertion,
        };

        Ok(FocusWindowParams {
            target,
            bring_to_front: raw.bring_to_front,
            app_kind: raw.app_kind,
            chrome_profile_id: raw.chrome_profile_id,
            verification,
        })
    }
}

/// Parse a `(method, value)` pair from the on-disk representation into a
/// `FocusTarget`. Accepts both the new typed numeric shape for `WindowId`/`Pid`
/// and the legacy stringified-number shape.
fn parse_focus_target(
    method: Option<&str>,
    value: &serde_json::Value,
) -> Result<FocusTarget, String> {
    // Legacy unconfigured nodes: no method, or method=AppName with null/empty
    // value. Map to the default (empty-string AppName) so the UI editor
    // receives `method: "AppName"` as it always has.
    let Some(method) = method else {
        return Ok(FocusTarget::default());
    };
    match method {
        "AppName" => match value {
            serde_json::Value::Null => Ok(FocusTarget::default()),
            serde_json::Value::String(s) if s.is_empty() => Ok(FocusTarget::default()),
            serde_json::Value::String(s) => Ok(FocusTarget::AppName(s.clone())),
            other => Err(format!(
                "FocusWindowParams: expected string 'value' for AppName, got {other}"
            )),
        },
        "WindowId" => coerce_u64(value)
            .map(FocusTarget::WindowId)
            .map_err(|msg| format!("FocusWindowParams.value (WindowId): {msg}")),
        "Pid" => coerce_u32(value)
            .map(FocusTarget::Pid)
            .map_err(|msg| format!("FocusWindowParams.value (Pid): {msg}")),
        // Legacy {"method":"None"} workflow files — still migrate to the
        // empty AppName default rather than a dedicated None variant.
        "None" => Ok(FocusTarget::default()),
        other => Err(format!("FocusWindowParams: unknown method '{other}'")),
    }
}

fn coerce_u64(value: &serde_json::Value) -> Result<u64, String> {
    match value {
        serde_json::Value::Null => Err("missing".into()),
        serde_json::Value::Number(n) => n.as_u64().ok_or_else(|| format!("not a u64: {n}")),
        serde_json::Value::String(s) => s
            .parse::<u64>()
            .map_err(|e| format!("not parseable as u64: {s} ({e})")),
        other => Err(format!("expected number or string, got {other}")),
    }
}

fn coerce_u32(value: &serde_json::Value) -> Result<u32, String> {
    coerce_u64(value)
        .and_then(|n| u32::try_from(n).map_err(|_| format!("value {n} does not fit in u32")))
}

impl HasVerification for FocusWindowParams {
    fn verification(&self) -> Option<&VerificationConfig> {
        if self.verification.is_empty() {
            None
        } else {
            Some(&self.verification)
        }
    }
}

// --- New native node params ---

#[derive(Debug, Clone, Default, Serialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct DragParams {
    #[serde(default)]
    pub from_x: Option<f64>,
    #[serde(default)]
    pub from_y: Option<f64>,
    #[serde(default)]
    pub to_x: Option<f64>,
    #[serde(default)]
    pub to_y: Option<f64>,
    #[serde(flatten, default)]
    pub verification: VerificationConfig,
}

impl_verification_deser!(
    DragParams {
        from_x: Option<f64> = None,
        from_y: Option<f64> = None,
        to_x: Option<f64> = None,
        to_y: Option<f64> = None,
    }
);

#[derive(Debug, Clone, Default, Serialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct LaunchAppParams {
    pub app_name: String,
    #[serde(flatten, default)]
    pub verification: VerificationConfig,
}

impl_verification_deser!(LaunchAppParams {
    app_name: String = String::new(),
});

#[derive(Debug, Clone, Default, Serialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct QuitAppParams {
    pub app_name: String,
    #[serde(flatten, default)]
    pub verification: VerificationConfig,
}

impl_verification_deser!(QuitAppParams {
    app_name: String = String::new(),
});

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
///
/// Accepts both the legacy on-disk shape `{"uid": "..."}` (deserialized as
/// `CdpTarget::ExactLabel(...)`) and the current tagged shape
/// `{"target": {"kind": "...", "value": "..."}}`. Also handles the migration
/// from split `verification_method` / `verification_assertion` fields to the
/// flattened [`VerificationConfig`] substruct.
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
                let verification = VerificationConfig {
                    verification_method: raw.verification_method,
                    verification_assertion: raw.verification_assertion,
                };
                Ok(Self {
                    target: match (raw.target, raw.uid) {
                        (Some(t), _) => t,
                        (None, Some(uid)) => CdpTarget::ExactLabel(uid),
                        (None, None) => CdpTarget::default(),
                    },
                    $( $extra_field: raw.$extra_field, )*
                    verification,
                })
            }
        }

        impl HasVerification for $ty {
            fn verification(&self) -> Option<&VerificationConfig> {
                if self.verification.is_empty() {
                    None
                } else {
                    Some(&self.verification)
                }
            }
        }
    };
}

#[derive(Debug, Clone, Default, Serialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct CdpClickParams {
    pub target: CdpTarget,
    #[serde(flatten, default)]
    pub verification: VerificationConfig,
}

impl_cdp_target_deser!(CdpClickParams {});

#[derive(Debug, Clone, Default, Serialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct CdpHoverParams {
    pub target: CdpTarget,
    #[serde(flatten, default)]
    pub verification: VerificationConfig,
}

impl_cdp_target_deser!(CdpHoverParams {});

#[derive(Debug, Clone, Default, Serialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct CdpFillParams {
    pub target: CdpTarget,
    pub value: String,
    #[serde(flatten, default)]
    pub verification: VerificationConfig,
}

impl_cdp_target_deser!(CdpFillParams { value: String });

#[derive(Debug, Clone, Default, Serialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct CdpTypeParams {
    pub text: String,
    #[serde(flatten, default)]
    pub verification: VerificationConfig,
}

impl_verification_deser!(CdpTypeParams {
    text: String = String::new(),
});

#[derive(Debug, Clone, Default, Serialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct CdpPressKeyParams {
    pub key: String,
    #[serde(default)]
    pub modifiers: Vec<String>,
    #[serde(flatten, default)]
    pub verification: VerificationConfig,
}

impl_verification_deser!(
    CdpPressKeyParams {
        key: String = String::new(),
        modifiers: Vec<String> = Vec::new(),
    }
);

#[derive(Debug, Clone, Default, Serialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct CdpNavigateParams {
    pub url: String,
    #[serde(flatten, default)]
    pub verification: VerificationConfig,
}

impl_verification_deser!(CdpNavigateParams {
    url: String = String::new(),
});

#[derive(Debug, Clone, Default, Serialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct CdpNewPageParams {
    #[serde(default)]
    pub url: String,
    #[serde(flatten, default)]
    pub verification: VerificationConfig,
}

impl_verification_deser!(CdpNewPageParams {
    url: String = String::new(),
});

#[derive(Debug, Clone, Default, Serialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct CdpClosePageParams {
    #[serde(default)]
    pub page_index: Option<u32>,
    #[serde(flatten, default)]
    pub verification: VerificationConfig,
}

impl_verification_deser!(
    CdpClosePageParams {
        page_index: Option<u32> = None,
    }
);

#[derive(Debug, Clone, Default, Serialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct CdpSelectPageParams {
    pub page_index: u32,
    #[serde(flatten, default)]
    pub verification: VerificationConfig,
}

impl_verification_deser!(CdpSelectPageParams {
    page_index: u32 = 0,
});

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

#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct CdpHandleDialogParams {
    pub accept: bool,
    #[serde(default)]
    pub prompt_text: Option<String>,
    #[serde(flatten, default)]
    pub verification: VerificationConfig,
}

impl Default for CdpHandleDialogParams {
    fn default() -> Self {
        Self {
            accept: true,
            prompt_text: None,
            verification: VerificationConfig::default(),
        }
    }
}

impl_verification_deser!(
    CdpHandleDialogParams {
        accept: bool = true,
        prompt_text: Option<String> = None,
    }
);

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
    pub event_type: TraceEventKind,
    pub payload: Value,
}

/// The canonical set of trace event kinds emitted by the executor.
///
/// Serialized as snake_case strings so the on-disk shape matches the literal
/// event-type strings that the engine has emitted historically. Legacy
/// `events.jsonl` files therefore load unchanged. Unknown strings deserialize
/// as [`TraceEventKind::Unknown`] so forward-compatible additions don't break
/// old readers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "snake_case")]
pub enum TraceEventKind {
    NodeStarted,
    ToolCall,
    ToolResult,
    StepCompleted,
    StepFailed,
    BranchEvaluated,
    LoopIteration,
    TargetResolved,
    ActionVerification,
    AmbiguityResolved,
    ElementResolved,
    MatchDisambiguated,
    AppResolved,
    CdpConnected,
    CdpClick,
    CdpHover,
    CdpFill,
    VisionSummary,
    VariableSet,
    Retry,
    SupervisionRetry,
    /// Forward-compatibility catch-all for event kinds that aren't in this
    /// enum yet. `#[serde(other)]` parses any unknown string into `Unknown`.
    #[serde(other)]
    Unknown,
}

impl From<&str> for TraceEventKind {
    fn from(s: &str) -> Self {
        // Direct match table inverse of [`TraceEventKind::as_str`]. A round-
        // trip test locks the two halves together so new variants surface as
        // test failures rather than silently routing through `Unknown`.
        match s {
            "node_started" => Self::NodeStarted,
            "tool_call" => Self::ToolCall,
            "tool_result" => Self::ToolResult,
            "step_completed" => Self::StepCompleted,
            "step_failed" => Self::StepFailed,
            "branch_evaluated" => Self::BranchEvaluated,
            "loop_iteration" => Self::LoopIteration,
            "target_resolved" => Self::TargetResolved,
            "action_verification" => Self::ActionVerification,
            "ambiguity_resolved" => Self::AmbiguityResolved,
            "element_resolved" => Self::ElementResolved,
            "match_disambiguated" => Self::MatchDisambiguated,
            "app_resolved" => Self::AppResolved,
            "cdp_connected" => Self::CdpConnected,
            "cdp_click" => Self::CdpClick,
            "cdp_hover" => Self::CdpHover,
            "cdp_fill" => Self::CdpFill,
            "vision_summary" => Self::VisionSummary,
            "variable_set" => Self::VariableSet,
            "retry" => Self::Retry,
            "supervision_retry" => Self::SupervisionRetry,
            _ => Self::Unknown,
        }
    }
}

impl From<String> for TraceEventKind {
    fn from(s: String) -> Self {
        Self::from(s.as_str())
    }
}

impl TraceEventKind {
    /// The snake_case wire form of this event kind, used by code paths that
    /// still need to construct the string representation directly (e.g. the
    /// CDP executor, which emits `cdp_click`/`cdp_hover`/`cdp_fill`).
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::NodeStarted => "node_started",
            Self::ToolCall => "tool_call",
            Self::ToolResult => "tool_result",
            Self::StepCompleted => "step_completed",
            Self::StepFailed => "step_failed",
            Self::BranchEvaluated => "branch_evaluated",
            Self::LoopIteration => "loop_iteration",
            Self::TargetResolved => "target_resolved",
            Self::ActionVerification => "action_verification",
            Self::AmbiguityResolved => "ambiguity_resolved",
            Self::ElementResolved => "element_resolved",
            Self::MatchDisambiguated => "match_disambiguated",
            Self::AppResolved => "app_resolved",
            Self::CdpConnected => "cdp_connected",
            Self::CdpClick => "cdp_click",
            Self::CdpHover => "cdp_hover",
            Self::CdpFill => "cdp_fill",
            Self::VisionSummary => "vision_summary",
            Self::VariableSet => "variable_set",
            Self::Retry => "retry",
            Self::SupervisionRetry => "supervision_retry",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub enum ArtifactKind {
    Screenshot,
    /// Catch-all for any artifact that doesn't fit a more specific category.
    /// Legacy kinds (`Ocr`, `TemplateMatch`, `Log`) were never produced —
    /// `#[serde(other)]` lets pre-removal artifact records still deserialize.
    #[serde(other)]
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
        assert!(params.verification.is_empty());
        assert!(HasVerification::verification(&params).is_none());
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
        let json = r#"{"uid": "OK", "verification_method": "Vlm", "verification_assertion": "button visible"}"#;
        let params: CdpClickParams = serde_json::from_str(json).unwrap();
        assert_eq!(params.target, CdpTarget::ExactLabel("OK".into()));
        let resolved =
            HasVerification::resolved_verification(&params).expect("verification should resolve");
        assert_eq!(resolved.method, VerificationMethod::Vlm);
        assert_eq!(resolved.assertion, "button visible");
    }

    #[test]
    fn cdp_click_params_missing_both_fields_defaults_to_intent() {
        let json = r#"{}"#;
        let params: CdpClickParams = serde_json::from_str(json).unwrap();
        assert_eq!(params.target, CdpTarget::default());
        assert!(matches!(params.target, CdpTarget::Intent(ref s) if s.is_empty()));
    }

    #[test]
    fn focus_window_params_legacy_string_window_id_deserializes_as_u64() {
        let json =
            r#"{"method":"WindowId","value":"42","bring_to_front":true,"app_kind":"Native"}"#;
        let params: FocusWindowParams = serde_json::from_str(json).unwrap();
        assert_eq!(params.target, FocusTarget::WindowId(42));
        assert!(params.bring_to_front);
    }

    #[test]
    fn focus_window_params_legacy_string_pid_deserializes_as_u32() {
        let json = r#"{"method":"Pid","value":"1234","bring_to_front":true,"app_kind":"Native"}"#;
        let params: FocusWindowParams = serde_json::from_str(json).unwrap();
        assert_eq!(params.target, FocusTarget::Pid(1234));
    }

    #[test]
    fn focus_window_params_numeric_window_id_roundtrips() {
        let params = FocusWindowParams {
            target: FocusTarget::WindowId(42),
            bring_to_front: true,
            ..Default::default()
        };
        let json = serde_json::to_string(&params).unwrap();
        assert!(json.contains("\"method\":\"WindowId\""));
        assert!(json.contains("\"value\":42"));
        let back: FocusWindowParams = serde_json::from_str(&json).unwrap();
        assert_eq!(back.target, FocusTarget::WindowId(42));
    }

    #[test]
    fn focus_window_params_app_name_roundtrips() {
        let params = FocusWindowParams {
            target: FocusTarget::AppName("Safari".into()),
            bring_to_front: true,
            ..Default::default()
        };
        let json = serde_json::to_string(&params).unwrap();
        let back: FocusWindowParams = serde_json::from_str(&json).unwrap();
        assert_eq!(back.target, FocusTarget::AppName("Safari".into()));
    }

    #[test]
    fn focus_window_params_legacy_null_value_preserves_app_name_method() {
        // Legacy shape: {method:"AppName", value:null} — the UI editor only
        // admits {AppName,WindowId,Pid}, so unconfigured nodes must decode
        // back to an empty-string AppName. Regression guard against an earlier
        // refactor that mapped them to a dedicated None variant (UI-unsafe).
        let json = r#"{"method":"AppName","value":null,"bring_to_front":true}"#;
        let params: FocusWindowParams = serde_json::from_str(json).unwrap();
        assert_eq!(params.target, FocusTarget::AppName(String::new()));
    }

    #[test]
    fn focus_window_params_legacy_none_method_becomes_app_name_default() {
        // Legacy shape: {method:"None"} — we removed the None variant from
        // the tagged enum, but workflow files that used it must still load.
        let json = r#"{"method":"None","bring_to_front":true}"#;
        let params: FocusWindowParams = serde_json::from_str(json).unwrap();
        assert_eq!(params.target, FocusTarget::AppName(String::new()));
    }

    #[test]
    fn click_params_legacy_split_verification_migrates_to_config() {
        // Legacy disk shape: verification_method / verification_assertion as
        // two separate sibling fields on the action params struct.
        let json = r#"{
            "target": null,
            "button": "Left",
            "click_count": 1,
            "verification_method": "Vlm",
            "verification_assertion": "button clicked"
        }"#;
        let params: ClickParams = serde_json::from_str(json).unwrap();
        let resolved =
            HasVerification::resolved_verification(&params).expect("verification should resolve");
        assert_eq!(resolved.method, VerificationMethod::Vlm);
        assert_eq!(resolved.assertion, "button clicked");
    }

    #[test]
    fn click_params_partial_verification_does_not_resolve() {
        // Only one half of the verification pair present — treat as "not
        // configured" rather than half-filling the config. The half that
        // *was* present still round-trips so users don't silently lose
        // their partial input.
        let json = r#"{
            "target": null,
            "button": "Left",
            "click_count": 1,
            "verification_assertion": "orphaned assertion"
        }"#;
        let params: ClickParams = serde_json::from_str(json).unwrap();
        assert!(HasVerification::resolved_verification(&params).is_none());
        assert_eq!(
            params.verification.verification_assertion.as_deref(),
            Some("orphaned assertion")
        );
    }

    #[test]
    fn click_params_new_shape_roundtrips() {
        let params = ClickParams {
            target: None,
            button: MouseButton::Left,
            click_count: 1,
            verification: VerificationConfig::new(VerificationMethod::Vlm, "assertion text"),
        };
        let json = serde_json::to_string(&params).unwrap();
        assert!(json.contains("\"verification_method\":\"Vlm\""));
        assert!(json.contains("\"verification_assertion\":\"assertion text\""));
        let back: ClickParams = serde_json::from_str(&json).unwrap();
        let resolved =
            HasVerification::resolved_verification(&back).expect("verification should resolve");
        assert_eq!(resolved.method, VerificationMethod::Vlm);
        assert_eq!(resolved.assertion, "assertion text");
    }

    #[test]
    fn trace_event_kind_snake_case_serialization() {
        let json = serde_json::to_string(&TraceEventKind::ToolCall).unwrap();
        assert_eq!(json, "\"tool_call\"");
        let back: TraceEventKind = serde_json::from_str(&json).unwrap();
        assert_eq!(back, TraceEventKind::ToolCall);
    }

    #[test]
    fn trace_event_kind_unknown_string_parses_as_unknown() {
        let back: TraceEventKind = serde_json::from_str("\"some_future_event\"").unwrap();
        assert_eq!(back, TraceEventKind::Unknown);
    }

    #[test]
    fn trace_event_kind_legacy_strings_all_parse() {
        let strings = [
            "node_started",
            "tool_call",
            "tool_result",
            "step_completed",
            "step_failed",
            "branch_evaluated",
            "loop_iteration",
            "target_resolved",
            "action_verification",
            "ambiguity_resolved",
            "element_resolved",
            "match_disambiguated",
            "app_resolved",
            "cdp_connected",
            "cdp_click",
            "cdp_hover",
            "cdp_fill",
            "vision_summary",
            "variable_set",
            "retry",
            "supervision_retry",
        ];
        for s in strings {
            let json = format!("\"{s}\"");
            let kind: TraceEventKind = serde_json::from_str(&json).expect(s);
            assert_ne!(
                kind,
                TraceEventKind::Unknown,
                "'{s}' should parse to a known variant"
            );
            assert_eq!(kind.as_str(), s, "as_str should round-trip for '{s}'");
        }
    }

    #[test]
    fn artifact_kind_legacy_values_parse_as_other() {
        for legacy in ["\"Ocr\"", "\"TemplateMatch\"", "\"Log\""] {
            let kind: ArtifactKind = serde_json::from_str(legacy).unwrap();
            assert_eq!(kind, ArtifactKind::Other);
        }
    }

    /// Lock the `From<&str>` match table to `as_str` so a new variant
    /// added to only one half surfaces as a test failure instead of
    /// silently routing through `Unknown`.
    #[test]
    fn trace_event_kind_from_str_round_trips_as_str() {
        let all = [
            TraceEventKind::NodeStarted,
            TraceEventKind::ToolCall,
            TraceEventKind::ToolResult,
            TraceEventKind::StepCompleted,
            TraceEventKind::StepFailed,
            TraceEventKind::BranchEvaluated,
            TraceEventKind::LoopIteration,
            TraceEventKind::TargetResolved,
            TraceEventKind::ActionVerification,
            TraceEventKind::AmbiguityResolved,
            TraceEventKind::ElementResolved,
            TraceEventKind::MatchDisambiguated,
            TraceEventKind::AppResolved,
            TraceEventKind::CdpConnected,
            TraceEventKind::CdpClick,
            TraceEventKind::CdpHover,
            TraceEventKind::CdpFill,
            TraceEventKind::VisionSummary,
            TraceEventKind::VariableSet,
            TraceEventKind::Retry,
            TraceEventKind::SupervisionRetry,
        ];
        for kind in all {
            let s = kind.as_str();
            let round_tripped: TraceEventKind = s.into();
            assert_eq!(
                round_tripped, kind,
                "From<&str> and as_str disagree for '{s}'",
            );
        }
        // Unknown strings must land in Unknown.
        let unknown: TraceEventKind = "some_future_event".into();
        assert_eq!(unknown, TraceEventKind::Unknown);
    }
}
