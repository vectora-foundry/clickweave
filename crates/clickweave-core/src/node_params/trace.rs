use super::*;

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
    AxClick,
    AxSetValue,
    AxSelect,
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
            "ax_click" => Self::AxClick,
            "ax_set_value" => Self::AxSetValue,
            "ax_select" => Self::AxSelect,
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
            Self::AxClick => "ax_click",
            Self::AxSetValue => "ax_set_value",
            Self::AxSelect => "ax_select",
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
