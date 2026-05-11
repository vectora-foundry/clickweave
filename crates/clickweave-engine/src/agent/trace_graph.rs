//! Engine-private agent trace-graph types.
//!
//! These types are the renamed, canvas-free successors of the `Workflow`,
//! `Node`, `Edge`, and `NodeType` types that previously lived in
//! `clickweave-core/src/workflow.rs`. The canvas layout fields (`Position`,
//! `NodeGroup`, `groups`, `next_id_counters`) have been dropped because no
//! engine consumer needs them. The `specta` derives have been dropped because
//! the trace graph is no longer serialised across the Tauri IPC boundary.
//!
//! `AgentTraceGraph` is the in-memory structure the runner accumulates while
//! executing. `TraceNode` and `TraceEdge` carry the minimal provenance data
//! needed by the skill extractor (`produced_node_ids`, `source_run_id`).
//! `TraceNodeKind` is the renamed `NodeType` enum, now private to the engine.
//! `tool_mapping` (also moved to this crate) references `TraceNodeKind`
//! instead of the old `clickweave_core::NodeType`.

use clickweave_core::output_schema::{NodeContext, OutputRole};
use clickweave_core::{
    AiStepParams, AppDebugKitParams, AxClickParams, AxSelectParams, AxSetValueParams,
    CdpClickParams, CdpClosePageParams, CdpFillParams, CdpHandleDialogParams, CdpHoverParams,
    CdpNavigateParams, CdpNewPageParams, CdpPressKeyParams, CdpSelectPageParams, CdpTypeParams,
    CdpWaitParams, ClickParams, DragParams, FindAppParams, FindImageParams, FindTextParams,
    FocusWindowParams, HoverParams, LaunchAppParams, McpToolCallParams, PressKeyParams,
    QuitAppParams, ScrollParams, TakeScreenshotParams, TypeTextParams,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// The engine-private renamed successor to `clickweave_core::NodeType`.
/// Specta derives dropped — the trace graph is never serialised across the
/// Tauri IPC boundary.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum TraceNodeKind {
    // Native — Query
    FindText(FindTextParams),
    FindImage(FindImageParams),
    FindApp(FindAppParams),
    TakeScreenshot(TakeScreenshotParams),
    // Native — Action
    Click(ClickParams),
    Hover(HoverParams),
    Drag(DragParams),
    TypeText(TypeTextParams),
    PressKey(PressKeyParams),
    Scroll(ScrollParams),
    FocusWindow(FocusWindowParams),
    LaunchApp(LaunchAppParams),
    QuitApp(QuitAppParams),
    // CDP — Query
    CdpWait(CdpWaitParams),
    // CDP — Action
    CdpClick(CdpClickParams),
    CdpHover(CdpHoverParams),
    CdpFill(CdpFillParams),
    CdpType(CdpTypeParams),
    CdpPressKey(CdpPressKeyParams),
    CdpNavigate(CdpNavigateParams),
    CdpNewPage(CdpNewPageParams),
    CdpClosePage(CdpClosePageParams),
    CdpSelectPage(CdpSelectPageParams),
    CdpHandleDialog(CdpHandleDialogParams),
    // AX — macOS accessibility-tree dispatch (background-safe)
    AxClick(AxClickParams),
    AxSetValue(AxSetValueParams),
    AxSelect(AxSelectParams),
    // AI
    AiStep(AiStepParams),
    // Generic
    McpToolCall(McpToolCallParams),
    AppDebugKitOp(AppDebugKitParams),
    /// Placeholder for removed or unrecognized node types. Preserved on
    /// load so that old trace records don't hard-fail; the UI can display them
    /// as disabled/unsupported.
    #[serde(other)]
    Unknown,
}

impl TraceNodeKind {
    pub fn output_role(&self) -> OutputRole {
        match self {
            Self::FindText(_)
            | Self::FindImage(_)
            | Self::FindApp(_)
            | Self::TakeScreenshot(_)
            | Self::CdpWait(_) => OutputRole::Query,

            Self::Click(_)
            | Self::Hover(_)
            | Self::Drag(_)
            | Self::TypeText(_)
            | Self::PressKey(_)
            | Self::Scroll(_)
            | Self::FocusWindow(_)
            | Self::LaunchApp(_)
            | Self::QuitApp(_)
            | Self::CdpClick(_)
            | Self::CdpHover(_)
            | Self::CdpFill(_)
            | Self::CdpType(_)
            | Self::CdpPressKey(_)
            | Self::CdpNavigate(_)
            | Self::CdpNewPage(_)
            | Self::CdpClosePage(_)
            | Self::CdpSelectPage(_)
            | Self::CdpHandleDialog(_)
            | Self::AxClick(_)
            | Self::AxSetValue(_)
            | Self::AxSelect(_) => OutputRole::Action,

            Self::AiStep(_) => OutputRole::Ai,

            Self::McpToolCall(_) | Self::AppDebugKitOp(_) | Self::Unknown => OutputRole::Generic,
        }
    }

    pub fn node_context(&self) -> NodeContext {
        match self {
            Self::FindText(_)
            | Self::FindImage(_)
            | Self::FindApp(_)
            | Self::TakeScreenshot(_)
            | Self::Click(_)
            | Self::Hover(_)
            | Self::Drag(_)
            | Self::TypeText(_)
            | Self::PressKey(_)
            | Self::Scroll(_)
            | Self::FocusWindow(_)
            | Self::LaunchApp(_)
            | Self::QuitApp(_)
            | Self::AxClick(_)
            | Self::AxSetValue(_)
            | Self::AxSelect(_) => NodeContext::Native,

            Self::CdpWait(_)
            | Self::CdpClick(_)
            | Self::CdpHover(_)
            | Self::CdpFill(_)
            | Self::CdpType(_)
            | Self::CdpPressKey(_)
            | Self::CdpNavigate(_)
            | Self::CdpNewPage(_)
            | Self::CdpClosePage(_)
            | Self::CdpSelectPage(_)
            | Self::CdpHandleDialog(_) => NodeContext::Cdp,

            _ => NodeContext::Independent,
        }
    }

    pub fn is_read_only(&self) -> bool {
        matches!(
            self,
            TraceNodeKind::FindText(_)
                | TraceNodeKind::FindImage(_)
                | TraceNodeKind::FindApp(_)
                | TraceNodeKind::TakeScreenshot(_)
                | TraceNodeKind::CdpWait(_)
        )
    }

    pub fn is_text_input(&self) -> bool {
        matches!(
            self,
            TraceNodeKind::TypeText(_)
                | TraceNodeKind::CdpFill(_)
                | TraceNodeKind::CdpType(_)
                | TraceNodeKind::AxSetValue(_)
        )
    }

    pub fn is_focus_establishing(&self) -> bool {
        matches!(self, TraceNodeKind::Click(_) | TraceNodeKind::CdpClick(_))
    }

    pub fn target_text(&self) -> Option<&str> {
        match self {
            TraceNodeKind::Click(p) => p
                .target
                .as_ref()
                .map(|t| t.text())
                .filter(|s| !s.is_empty()),
            TraceNodeKind::Hover(p) => p
                .target
                .as_ref()
                .map(|t| t.text())
                .filter(|s| !s.is_empty()),
            TraceNodeKind::FindText(p) => Some(p.search_text.as_str()).filter(|s| !s.is_empty()),
            TraceNodeKind::CdpClick(p) => Some(p.target.as_str()).filter(|s| !s.is_empty()),
            TraceNodeKind::CdpHover(p) => Some(p.target.as_str()).filter(|s| !s.is_empty()),
            TraceNodeKind::CdpWait(p) => Some(p.text.as_str()).filter(|s| !s.is_empty()),
            TraceNodeKind::AxClick(p) => Some(p.target.as_str()).filter(|s| !s.is_empty()),
            TraceNodeKind::AxSetValue(p) => Some(p.target.as_str()).filter(|s| !s.is_empty()),
            TraceNodeKind::AxSelect(p) => Some(p.target.as_str()).filter(|s| !s.is_empty()),
            _ => None,
        }
    }

    pub fn display_name(&self) -> &'static str {
        match self {
            TraceNodeKind::FindText(_) => "Find Text",
            TraceNodeKind::FindImage(_) => "Find Image",
            TraceNodeKind::FindApp(_) => "Find App",
            TraceNodeKind::TakeScreenshot(_) => "Take Screenshot",
            TraceNodeKind::Click(_) => "Click",
            TraceNodeKind::Hover(_) => "Hover",
            TraceNodeKind::Drag(_) => "Drag",
            TraceNodeKind::TypeText(_) => "Type Text",
            TraceNodeKind::PressKey(_) => "Press Key",
            TraceNodeKind::Scroll(_) => "Scroll",
            TraceNodeKind::FocusWindow(_) => "Focus Window",
            TraceNodeKind::LaunchApp(_) => "Launch App",
            TraceNodeKind::QuitApp(_) => "Quit App",
            TraceNodeKind::CdpWait(_) => "CDP Wait",
            TraceNodeKind::CdpClick(_) => "CDP Click",
            TraceNodeKind::CdpHover(_) => "CDP Hover",
            TraceNodeKind::CdpFill(_) => "CDP Fill",
            TraceNodeKind::CdpType(_) => "CDP Type",
            TraceNodeKind::CdpPressKey(_) => "CDP Press Key",
            TraceNodeKind::CdpNavigate(_) => "CDP Navigate",
            TraceNodeKind::CdpNewPage(_) => "CDP New Page",
            TraceNodeKind::CdpClosePage(_) => "CDP Close Page",
            TraceNodeKind::CdpSelectPage(_) => "CDP Select Page",
            TraceNodeKind::CdpHandleDialog(_) => "CDP Handle Dialog",
            TraceNodeKind::AxClick(_) => "AX Click",
            TraceNodeKind::AxSetValue(_) => "AX Set Value",
            TraceNodeKind::AxSelect(_) => "AX Select",
            TraceNodeKind::AiStep(_) => "AI Step",
            TraceNodeKind::McpToolCall(_) => "MCP Tool Call",
            TraceNodeKind::AppDebugKitOp(_) => "AppDebugKit Op",
            TraceNodeKind::Unknown => "Unknown",
        }
    }

    pub fn is_deterministic(&self) -> bool {
        !matches!(self, TraceNodeKind::AiStep(_))
    }

    pub fn verification(&self) -> Option<&clickweave_core::output_schema::VerificationConfig> {
        use clickweave_core::output_schema::HasVerification;
        match self {
            Self::Click(p) => p.verification(),
            Self::Hover(p) => p.verification(),
            Self::Drag(p) => p.verification(),
            Self::TypeText(p) => p.verification(),
            Self::PressKey(p) => p.verification(),
            Self::Scroll(p) => p.verification(),
            Self::FocusWindow(p) => p.verification(),
            Self::LaunchApp(p) => p.verification(),
            Self::QuitApp(p) => p.verification(),
            Self::CdpClick(p) => p.verification(),
            Self::CdpHover(p) => p.verification(),
            Self::CdpFill(p) => p.verification(),
            Self::CdpType(p) => p.verification(),
            Self::CdpPressKey(p) => p.verification(),
            Self::CdpNavigate(p) => p.verification(),
            Self::CdpNewPage(p) => p.verification(),
            Self::CdpClosePage(p) => p.verification(),
            Self::CdpSelectPage(p) => p.verification(),
            Self::CdpHandleDialog(p) => p.verification(),
            Self::AxClick(p) => p.verification(),
            Self::AxSetValue(p) => p.verification(),
            Self::AxSelect(p) => p.verification(),
            _ => None,
        }
    }

    pub fn has_verification(&self) -> bool {
        self.verification().is_some_and(|v| {
            v.verification_method.is_some()
                && v.verification_assertion
                    .as_deref()
                    .is_some_and(|a| !a.is_empty())
        })
    }
}

/// Minimal edge record: directed link from one trace node to another.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraceEdge {
    pub from: Uuid,
    pub to: Uuid,
}

/// Minimal node record: a single tool-call step in the agent's execution trace.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraceNode {
    pub id: Uuid,
    pub node_kind: TraceNodeKind,
    pub name: String,
    pub auto_id: String,
    pub enabled: bool,
    /// Provenance stamp: the agent generation ID that produced this node.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_run_id: Option<Uuid>,
}

impl TraceNode {
    pub fn new(
        node_kind: TraceNodeKind,
        name: impl Into<String>,
        auto_id: impl Into<String>,
    ) -> Self {
        Self {
            id: Uuid::new_v4(),
            node_kind,
            name: name.into(),
            auto_id: auto_id.into(),
            enabled: true,
            source_run_id: None,
        }
    }

    /// Stamp a run-id provenance onto this node, consuming self.
    pub fn with_run_id(mut self, run_id: Uuid) -> Self {
        self.source_run_id = Some(run_id);
        self
    }
}

/// Accumulated trace of all tool-call steps the agent executed in one run.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AgentTraceGraph {
    pub id: Uuid,
    pub nodes: Vec<TraceNode>,
    pub edges: Vec<TraceEdge>,
}

impl AgentTraceGraph {
    pub fn new() -> Self {
        Self {
            id: Uuid::new_v4(),
            nodes: Vec::new(),
            edges: Vec::new(),
        }
    }

    pub fn add_node(&mut self, node: TraceNode) -> Uuid {
        let id = node.id;
        self.nodes.push(node);
        id
    }

    pub fn add_edge(&mut self, from: Uuid, to: Uuid) {
        self.edges.push(TraceEdge { from, to });
    }

    pub fn find_node(&self, id: Uuid) -> Option<&TraceNode> {
        self.nodes.iter().find(|n| n.id == id)
    }
}
