use crate::node_params::*;
use crate::output_schema::{NodeContext, OutputRole};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use thiserror::Error;
use uuid::Uuid;

/// Typed errors produced by [`validate_workflow`]. Kept as an enum so new
/// validation checks (cycle detection, dangling edges, duplicate auto-ids,
/// etc.) can be added without churning the downstream `Display` surface.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum WorkflowValidationError {
    #[error("Workflow has no nodes")]
    Empty,
}

/// Basic workflow validation: ensures the workflow has at least one node.
pub fn validate_workflow(workflow: &Workflow) -> Result<(), WorkflowValidationError> {
    if workflow.nodes.is_empty() {
        return Err(WorkflowValidationError::Empty);
    }
    Ok(())
}

const DEFAULT_SUPERVISION_RETRIES: u32 = 2;
fn default_supervision_retries() -> u32 {
    DEFAULT_SUPERVISION_RETRIES
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct Workflow {
    pub id: Uuid,
    pub name: String,
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
    #[serde(default)]
    pub groups: Vec<NodeGroup>,
    #[serde(default)]
    pub next_id_counters: HashMap<String, u32>,
    #[serde(default)]
    pub intent: Option<String>,
}

impl Default for Workflow {
    fn default() -> Self {
        Self {
            id: Uuid::new_v4(),
            name: "New Workflow".to_string(),
            nodes: vec![],
            edges: vec![],
            groups: vec![],
            next_id_counters: HashMap::new(),
            intent: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct Node {
    pub id: Uuid,
    pub node_type: NodeType,
    pub position: Position,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default)]
    pub auto_id: String,
    pub enabled: bool,
    pub timeout_ms: Option<u64>,
    pub settle_ms: Option<u64>,
    pub retries: u32,
    #[serde(default = "default_supervision_retries")]
    pub supervision_retries: u32,
    pub trace_level: TraceLevel,
    #[serde(default)]
    pub role: NodeRole,
    pub expected_outcome: Option<String>,
    /// Provenance stamp: the agent generation ID that produced this node.
    /// `None` for nodes added by the user, by deterministic walkthrough
    /// synthesis, or loaded from a pre-upgrade workflow file. Used by
    /// Clear-conversation and selective-delete to scope operations to
    /// agent-built nodes only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_run_id: Option<Uuid>,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct Position {
    pub x: f32,
    pub y: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct Edge {
    pub from: Uuid,
    pub to: Uuid,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct NodeGroup {
    pub id: Uuid,
    pub name: String,
    pub color: String,
    pub node_ids: Vec<Uuid>,
    pub parent_group_id: Option<Uuid>,
}

impl Node {
    pub fn new(
        node_type: NodeType,
        position: Position,
        name: impl Into<String>,
        auto_id: impl Into<String>,
    ) -> Self {
        Self {
            id: Uuid::new_v4(),
            node_type,
            position,
            name: name.into(),
            description: None,
            auto_id: auto_id.into(),
            enabled: true,
            timeout_ms: None,
            settle_ms: None,
            retries: 0,
            supervision_retries: default_supervision_retries(),
            trace_level: TraceLevel::Minimal,
            role: NodeRole::Default,
            expected_outcome: None,
            source_run_id: None,
        }
    }

    /// Stamp a run-id provenance onto this node, consuming self.
    pub fn with_run_id(mut self, run_id: Uuid) -> Self {
        self.source_run_id = Some(run_id);
        self
    }
}

impl Workflow {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            ..Default::default()
        }
    }

    pub fn add_node(&mut self, node_type: NodeType, position: Position) -> Uuid {
        let name = node_type.display_name().to_string();
        let auto_id = crate::auto_id::assign_auto_id(&node_type, &mut self.next_id_counters);
        let node = Node::new(node_type, position, name, auto_id);
        let id = node.id;
        self.nodes.push(node);
        id
    }

    pub fn add_edge(&mut self, from: Uuid, to: Uuid) {
        self.edges.push(Edge { from, to });
    }

    pub fn find_node(&self, id: Uuid) -> Option<&Node> {
        self.nodes.iter().find(|n| n.id == id)
    }

    pub fn find_node_mut(&mut self, id: Uuid) -> Option<&mut Node> {
        self.nodes.iter_mut().find(|n| n.id == id)
    }

    pub fn remove_node(&mut self, id: Uuid) {
        self.nodes.retain(|n| n.id != id);
        self.edges.retain(|e| e.from != id && e.to != id);
    }

    pub fn remove_edge(&mut self, from: Uuid, to: Uuid) {
        self.edges.retain(|e| !(e.from == from && e.to == to));
    }

    /// Ensure `next_id_counters` are at least as high as the max `auto_id`
    /// seen in the workflow. Raises counters but never lowers them, preserving
    /// the monotonic high-water mark so deleted nodes don't release their IDs.
    pub fn fixup_auto_ids(&mut self) {
        let ids: Vec<&str> = self
            .nodes
            .iter()
            .filter(|n| !n.auto_id.is_empty())
            .map(|n| n.auto_id.as_str())
            .collect();
        crate::auto_id::fixup_counters(&ids, &mut self.next_id_counters);
    }
}

// --- Node type system ---

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(tag = "type")]
pub enum NodeType {
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
    /// load so that old workflows don't hard-fail; the UI can display them
    /// as disabled/unsupported.
    #[serde(other)]
    Unknown,
}

impl NodeType {
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

    /// Returns true for node types that only observe/query state without modifying it.
    /// Used to skip supervision inside loops where "not found" is expected behavior.
    pub fn is_read_only(&self) -> bool {
        matches!(
            self,
            NodeType::FindText(_)
                | NodeType::FindImage(_)
                | NodeType::FindApp(_)
                | NodeType::TakeScreenshot(_)
                | NodeType::CdpWait(_)
        )
    }

    /// Returns true for node types that type or fill text into an input field.
    /// Used to determine whether a supervision retry should re-run the preceding
    /// click to re-establish focus.
    pub fn is_text_input(&self) -> bool {
        matches!(
            self,
            NodeType::TypeText(_)
                | NodeType::CdpFill(_)
                | NodeType::CdpType(_)
                | NodeType::AxSetValue(_)
        )
    }

    /// Returns true for node types that establish element-level focus (clicks).
    /// Used to identify predecessor nodes that should be re-run before retrying
    /// a text-input node.
    pub fn is_focus_establishing(&self) -> bool {
        // `AxClick` deliberately excluded — it dispatches without stealing
        // focus, so it does not re-establish keyboard focus for a following
        // text-input node.
        matches!(self, NodeType::Click(_) | NodeType::CdpClick(_))
    }

    /// Extract the primary text target from node types that resolve elements by name.
    /// Returns `None` for node types that don't have a text-based target (e.g.
    /// coordinate clicks, key presses, AI steps).
    pub fn target_text(&self) -> Option<&str> {
        match self {
            NodeType::Click(p) => p
                .target
                .as_ref()
                .map(|t| t.text())
                .filter(|s| !s.is_empty()),
            NodeType::Hover(p) => p
                .target
                .as_ref()
                .map(|t| t.text())
                .filter(|s| !s.is_empty()),
            NodeType::FindText(p) => Some(p.search_text.as_str()).filter(|s| !s.is_empty()),
            NodeType::CdpClick(p) => Some(p.target.as_str()).filter(|s| !s.is_empty()),
            NodeType::CdpHover(p) => Some(p.target.as_str()).filter(|s| !s.is_empty()),
            NodeType::CdpWait(p) => Some(p.text.as_str()).filter(|s| !s.is_empty()),
            NodeType::AxClick(p) => Some(p.target.as_str()).filter(|s| !s.is_empty()),
            NodeType::AxSetValue(p) => Some(p.target.as_str()).filter(|s| !s.is_empty()),
            NodeType::AxSelect(p) => Some(p.target.as_str()).filter(|s| !s.is_empty()),
            _ => None,
        }
    }

    pub fn display_name(&self) -> &'static str {
        match self {
            NodeType::FindText(_) => "Find Text",
            NodeType::FindImage(_) => "Find Image",
            NodeType::FindApp(_) => "Find App",
            NodeType::TakeScreenshot(_) => "Take Screenshot",
            NodeType::Click(_) => "Click",
            NodeType::Hover(_) => "Hover",
            NodeType::Drag(_) => "Drag",
            NodeType::TypeText(_) => "Type Text",
            NodeType::PressKey(_) => "Press Key",
            NodeType::Scroll(_) => "Scroll",
            NodeType::FocusWindow(_) => "Focus Window",
            NodeType::LaunchApp(_) => "Launch App",
            NodeType::QuitApp(_) => "Quit App",
            NodeType::CdpWait(_) => "CDP Wait",
            NodeType::CdpClick(_) => "CDP Click",
            NodeType::CdpHover(_) => "CDP Hover",
            NodeType::CdpFill(_) => "CDP Fill",
            NodeType::CdpType(_) => "CDP Type",
            NodeType::CdpPressKey(_) => "CDP Press Key",
            NodeType::CdpNavigate(_) => "CDP Navigate",
            NodeType::CdpNewPage(_) => "CDP New Page",
            NodeType::CdpClosePage(_) => "CDP Close Page",
            NodeType::CdpSelectPage(_) => "CDP Select Page",
            NodeType::CdpHandleDialog(_) => "CDP Handle Dialog",
            NodeType::AxClick(_) => "AX Click",
            NodeType::AxSetValue(_) => "AX Set Value",
            NodeType::AxSelect(_) => "AX Select",
            NodeType::AiStep(_) => "AI Step",
            NodeType::McpToolCall(_) => "MCP Tool Call",
            NodeType::AppDebugKitOp(_) => "AppDebugKit Op",
            NodeType::Unknown => "Unknown",
        }
    }

    /// Human-readable description of what this node does, for LLM verification prompts.
    pub fn action_description(&self) -> String {
        match self {
            NodeType::Click(p) => match &p.target {
                Some(t) if !t.text().is_empty() => format!("Clicked on '{}'", t.text()),
                Some(ClickTarget::Coordinates { x, y }) => {
                    format!("Clicked at ({}, {})", x, y)
                }
                Some(ClickTarget::WindowControl { action }) => {
                    format!("Clicked {}", action.display_name().to_lowercase())
                }
                _ => "Clicked".to_string(),
            },
            NodeType::Hover(p) => match &p.target {
                Some(t) if !t.text().is_empty() => format!("Hovered over '{}'", t.text()),
                Some(ClickTarget::Coordinates { x, y }) => {
                    format!("Hovered at ({}, {})", x, y)
                }
                Some(ClickTarget::WindowControl { action }) => {
                    format!("Hovered {}", action.display_name().to_lowercase())
                }
                _ => "Hovered".to_string(),
            },
            NodeType::Drag(p) => format!(
                "Dragged from ({}, {}) to ({}, {})",
                p.from_x.unwrap_or(0.0),
                p.from_y.unwrap_or(0.0),
                p.to_x.unwrap_or(0.0),
                p.to_y.unwrap_or(0.0)
            ),
            NodeType::TypeText(p) => format!("Typed '{}'", p.text),
            NodeType::PressKey(p) => format!("Pressed key '{}'", p.key),
            NodeType::Scroll(p) => format!("Scrolled by {}", p.delta_y),
            NodeType::FocusWindow(p) => match &p.target {
                FocusTarget::AppName(name) if !name.is_empty() => {
                    format!("Focused window '{}'", name)
                }
                FocusTarget::WindowId(id) => format!("Focused window id {}", id),
                FocusTarget::Pid(pid) => format!("Focused window pid {}", pid),
                FocusTarget::AppName(_) => "Focused window".to_string(),
            },
            NodeType::LaunchApp(p) => format!("Launched app '{}'", p.app_name),
            NodeType::QuitApp(p) => format!("Quit app '{}'", p.app_name),
            NodeType::FindText(p) => format!("Searched for text '{}'", p.search_text),
            NodeType::FindImage(_) => "Searched for image template".to_string(),
            NodeType::FindApp(p) => format!("Searched for app '{}'", p.search),
            NodeType::TakeScreenshot(_) => "Took a screenshot".to_string(),
            NodeType::CdpWait(p) => format!("Waited for text '{}'", p.text),
            NodeType::CdpClick(p) => format!("CDP clicked element '{}'", p.target.as_str()),
            NodeType::CdpHover(p) => format!("CDP hovered element '{}'", p.target.as_str()),
            NodeType::CdpFill(p) => format!("CDP filled with '{}'", p.value),
            NodeType::CdpType(p) => format!("CDP typed '{}'", p.text),
            NodeType::CdpPressKey(p) => format!("CDP pressed key '{}'", p.key),
            NodeType::CdpNavigate(p) => format!("CDP navigated to '{}'", p.url),
            NodeType::CdpNewPage(p) => {
                if p.url.is_empty() {
                    "CDP opened new page".to_string()
                } else {
                    format!("CDP opened new page '{}'", p.url)
                }
            }
            NodeType::CdpClosePage(_) => "CDP closed page".to_string(),
            NodeType::CdpSelectPage(p) => format!("CDP selected page {}", p.page_index),
            NodeType::CdpHandleDialog(p) => {
                if p.accept {
                    "CDP accepted dialog".to_string()
                } else {
                    "CDP dismissed dialog".to_string()
                }
            }
            NodeType::AxClick(p) => format!("AX clicked element '{}'", p.target.as_str()),
            NodeType::AxSetValue(p) => format!("AX set value to '{}'", p.value),
            NodeType::AxSelect(p) => format!("AX selected row '{}'", p.target.as_str()),
            NodeType::McpToolCall(p) => format!("Called tool '{}'", p.tool_name),
            NodeType::AppDebugKitOp(p) => format!("Called AppDebugKit '{}'", p.operation_name),
            _ => self.display_name().to_string(),
        }
    }

    pub fn icon(&self) -> &'static str {
        match self {
            NodeType::AiStep(_) => "🤖",
            NodeType::TakeScreenshot(_) => "📸",
            NodeType::FindText(_) => "🔍",
            NodeType::FindImage(_) => "🖼",
            NodeType::FindApp(_) => "🔍",
            NodeType::Click(_) | NodeType::CdpClick(_) | NodeType::AxClick(_) => "🖱",
            NodeType::Hover(_) | NodeType::CdpHover(_) => "👆",
            NodeType::Drag(_) => "↔",
            NodeType::TypeText(_)
            | NodeType::CdpType(_)
            | NodeType::CdpFill(_)
            | NodeType::AxSetValue(_) => "⌨",
            NodeType::PressKey(_) | NodeType::CdpPressKey(_) => "⌨",
            NodeType::Scroll(_) => "📜",
            NodeType::FocusWindow(_) => "🪟",
            NodeType::LaunchApp(_) => "🚀",
            NodeType::QuitApp(_) => "❌",
            NodeType::CdpWait(_) => "⏳",
            NodeType::CdpNavigate(_) => "🌐",
            NodeType::CdpNewPage(_) => "📄",
            NodeType::CdpClosePage(_) => "🗑",
            NodeType::CdpSelectPage(_) => "📑",
            NodeType::CdpHandleDialog(_) => "💬",
            NodeType::AxSelect(_) => "✅",
            NodeType::McpToolCall(_) | NodeType::AppDebugKitOp(_) => "🔧",
            NodeType::Unknown => "❓",
        }
    }

    pub fn is_deterministic(&self) -> bool {
        !matches!(self, NodeType::AiStep(_))
    }

    /// Returns the verification config on this node, if any.
    ///
    /// Uses the [`HasVerification`](crate::output_schema::HasVerification)
    /// trait impl that every action params struct carries so new node types
    /// don't have to be added to a giant match arm.
    pub fn verification(&self) -> Option<&crate::output_schema::VerificationConfig> {
        use crate::output_schema::HasVerification;
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

    /// Returns the verification method configured on this node, if any.
    pub fn verification_method(&self) -> Option<crate::output_schema::VerificationMethod> {
        self.verification().and_then(|v| v.verification_method)
    }

    /// Returns the verification assertion configured on this node, if any.
    pub fn verification_assertion(&self) -> Option<&str> {
        self.verification()
            .and_then(|v| v.verification_assertion.as_deref())
    }

    /// Returns true when the node has both a verification method and a
    /// non-empty assertion — the executor's requirement for producing
    /// verification output variables.
    pub fn has_verification(&self) -> bool {
        self.verification().is_some_and(|v| {
            v.verification_method.is_some()
                && v.verification_assertion
                    .as_deref()
                    .is_some_and(|a| !a.is_empty())
        })
    }

    /// All available node types with default parameters.
    pub fn all_defaults() -> Vec<NodeType> {
        vec![
            // Native — Query
            NodeType::FindText(FindTextParams::default()),
            NodeType::FindImage(FindImageParams::default()),
            NodeType::FindApp(FindAppParams::default()),
            NodeType::TakeScreenshot(TakeScreenshotParams::default()),
            // Native — Action
            NodeType::Click(ClickParams::default()),
            NodeType::Hover(HoverParams::default()),
            NodeType::Drag(DragParams::default()),
            NodeType::TypeText(TypeTextParams::default()),
            NodeType::PressKey(PressKeyParams::default()),
            NodeType::Scroll(ScrollParams::default()),
            NodeType::FocusWindow(FocusWindowParams::default()),
            NodeType::LaunchApp(LaunchAppParams::default()),
            NodeType::QuitApp(QuitAppParams::default()),
            // CDP — Query
            NodeType::CdpWait(CdpWaitParams::default()),
            // CDP — Action
            NodeType::CdpClick(CdpClickParams::default()),
            NodeType::CdpHover(CdpHoverParams::default()),
            NodeType::CdpFill(CdpFillParams::default()),
            NodeType::CdpType(CdpTypeParams::default()),
            NodeType::CdpPressKey(CdpPressKeyParams::default()),
            NodeType::CdpNavigate(CdpNavigateParams::default()),
            NodeType::CdpNewPage(CdpNewPageParams::default()),
            NodeType::CdpClosePage(CdpClosePageParams::default()),
            NodeType::CdpSelectPage(CdpSelectPageParams::default()),
            NodeType::CdpHandleDialog(CdpHandleDialogParams::default()),
            // AX (macOS accessibility dispatch) — Action
            NodeType::AxClick(AxClickParams::default()),
            NodeType::AxSetValue(AxSetValueParams::default()),
            NodeType::AxSelect(AxSelectParams::default()),
            // AI
            NodeType::AiStep(AiStepParams::default()),
            // Generic
            NodeType::McpToolCall(McpToolCallParams::default()),
            NodeType::AppDebugKitOp(AppDebugKitParams::default()),
        ]
    }

    /// Look up a default NodeType by its display name.
    pub fn default_for_name(name: &str) -> Option<NodeType> {
        Some(match name {
            "Find Text" => NodeType::FindText(FindTextParams::default()),
            "Find Image" => NodeType::FindImage(FindImageParams::default()),
            "Find App" => NodeType::FindApp(FindAppParams::default()),
            "Take Screenshot" => NodeType::TakeScreenshot(TakeScreenshotParams::default()),
            "Click" => NodeType::Click(ClickParams::default()),
            "Hover" => NodeType::Hover(HoverParams::default()),
            "Drag" => NodeType::Drag(DragParams::default()),
            "Type Text" => NodeType::TypeText(TypeTextParams::default()),
            "Press Key" => NodeType::PressKey(PressKeyParams::default()),
            "Scroll" => NodeType::Scroll(ScrollParams::default()),
            "Focus Window" => NodeType::FocusWindow(FocusWindowParams::default()),
            "Launch App" => NodeType::LaunchApp(LaunchAppParams::default()),
            "Quit App" => NodeType::QuitApp(QuitAppParams::default()),
            "CDP Wait" => NodeType::CdpWait(CdpWaitParams::default()),
            "CDP Click" => NodeType::CdpClick(CdpClickParams::default()),
            "CDP Hover" => NodeType::CdpHover(CdpHoverParams::default()),
            "CDP Fill" => NodeType::CdpFill(CdpFillParams::default()),
            "CDP Type" => NodeType::CdpType(CdpTypeParams::default()),
            "CDP Press Key" => NodeType::CdpPressKey(CdpPressKeyParams::default()),
            "CDP Navigate" => NodeType::CdpNavigate(CdpNavigateParams::default()),
            "CDP New Page" => NodeType::CdpNewPage(CdpNewPageParams::default()),
            "CDP Close Page" => NodeType::CdpClosePage(CdpClosePageParams::default()),
            "CDP Select Page" => NodeType::CdpSelectPage(CdpSelectPageParams::default()),
            "CDP Handle Dialog" => NodeType::CdpHandleDialog(CdpHandleDialogParams::default()),
            "AX Click" => NodeType::AxClick(AxClickParams::default()),
            "AX Set Value" => NodeType::AxSetValue(AxSetValueParams::default()),
            "AX Select" => NodeType::AxSelect(AxSelectParams::default()),
            "AI Step" => NodeType::AiStep(AiStepParams::default()),
            "MCP Tool Call" => NodeType::McpToolCall(McpToolCallParams::default()),
            "AppDebugKit Op" => NodeType::AppDebugKitOp(AppDebugKitParams::default()),
            _ => return None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_workflow_rejects_empty() {
        let workflow = Workflow::new("Empty");
        let err = validate_workflow(&workflow).expect_err("expected Empty error");
        assert_eq!(err, WorkflowValidationError::Empty);
        assert_eq!(err.to_string(), "Workflow has no nodes");
    }

    #[test]
    fn validate_workflow_accepts_non_empty() {
        let mut workflow = Workflow::new("With nodes");
        workflow.add_node(
            NodeType::TakeScreenshot(TakeScreenshotParams::default()),
            Position { x: 0.0, y: 0.0 },
        );
        assert!(validate_workflow(&workflow).is_ok());
    }

    #[test]
    fn test_node_type_serialization_roundtrip() {
        for nt in NodeType::all_defaults() {
            let json = serde_json::to_string(&nt).expect("serialize");
            let deserialized: NodeType = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(nt.display_name(), deserialized.display_name());
            assert_eq!(nt.output_role(), deserialized.output_role());
        }
    }

    #[test]
    fn test_node_type_output_role_correctness() {
        assert_eq!(
            NodeType::AiStep(AiStepParams::default()).output_role(),
            OutputRole::Ai
        );
        assert_eq!(
            NodeType::TakeScreenshot(TakeScreenshotParams::default()).output_role(),
            OutputRole::Query
        );
        assert_eq!(
            NodeType::FindText(FindTextParams::default()).output_role(),
            OutputRole::Query
        );
        assert_eq!(
            NodeType::FindImage(FindImageParams::default()).output_role(),
            OutputRole::Query
        );
        assert_eq!(
            NodeType::FindApp(FindAppParams::default()).output_role(),
            OutputRole::Query
        );
        assert_eq!(
            NodeType::Click(ClickParams::default()).output_role(),
            OutputRole::Action
        );
        assert_eq!(
            NodeType::Hover(HoverParams::default()).output_role(),
            OutputRole::Action
        );
        assert_eq!(
            NodeType::Drag(DragParams::default()).output_role(),
            OutputRole::Action
        );
        assert_eq!(
            NodeType::TypeText(TypeTextParams::default()).output_role(),
            OutputRole::Action
        );
        assert_eq!(
            NodeType::Scroll(ScrollParams::default()).output_role(),
            OutputRole::Action
        );
        assert_eq!(
            NodeType::FocusWindow(FocusWindowParams::default()).output_role(),
            OutputRole::Action
        );
        assert_eq!(
            NodeType::LaunchApp(LaunchAppParams::default()).output_role(),
            OutputRole::Action
        );
        assert_eq!(
            NodeType::QuitApp(QuitAppParams::default()).output_role(),
            OutputRole::Action
        );
        assert_eq!(
            NodeType::CdpClick(CdpClickParams::default()).output_role(),
            OutputRole::Action
        );
        assert_eq!(
            NodeType::CdpWait(CdpWaitParams::default()).output_role(),
            OutputRole::Query
        );
        assert_eq!(
            NodeType::AppDebugKitOp(AppDebugKitParams::default()).output_role(),
            OutputRole::Generic
        );
    }

    #[test]
    fn test_node_type_is_deterministic() {
        assert!(!NodeType::AiStep(AiStepParams::default()).is_deterministic());
        assert!(NodeType::TakeScreenshot(TakeScreenshotParams::default()).is_deterministic());
        assert!(NodeType::Click(ClickParams::default()).is_deterministic());
        assert!(NodeType::Hover(HoverParams::default()).is_deterministic());
        assert!(NodeType::TypeText(TypeTextParams::default()).is_deterministic());
        assert!(NodeType::Scroll(ScrollParams::default()).is_deterministic());
        assert!(NodeType::FindText(FindTextParams::default()).is_deterministic());
        assert!(NodeType::FindImage(FindImageParams::default()).is_deterministic());
        assert!(NodeType::FocusWindow(FocusWindowParams::default()).is_deterministic());
        assert!(NodeType::AppDebugKitOp(AppDebugKitParams::default()).is_deterministic());
        assert!(NodeType::CdpClick(CdpClickParams::default()).is_deterministic());
        assert!(NodeType::CdpWait(CdpWaitParams::default()).is_deterministic());
    }

    #[test]
    fn test_workflow_serialization_roundtrip() {
        let mut wf = Workflow::new("Test Workflow");
        let a = wf.add_node(
            NodeType::AiStep(AiStepParams {
                prompt: "Do something".to_string(),
                ..Default::default()
            }),
            Position { x: 0.0, y: 0.0 },
        );
        let b = wf.add_node(
            NodeType::TakeScreenshot(TakeScreenshotParams::default()),
            Position { x: 100.0, y: 0.0 },
        );
        wf.add_edge(a, b);

        let json = serde_json::to_string_pretty(&wf).expect("serialize workflow");
        let deserialized: Workflow = serde_json::from_str(&json).expect("deserialize workflow");

        assert_eq!(deserialized.name, "Test Workflow");
        assert_eq!(deserialized.nodes.len(), 2);
        assert_eq!(deserialized.edges.len(), 1);
    }

    #[test]
    fn test_remove_node_cleans_edges() {
        let mut wf = Workflow::default();
        let a = wf.add_node(
            NodeType::Click(ClickParams::default()),
            Position { x: 0.0, y: 0.0 },
        );
        let b = wf.add_node(
            NodeType::TypeText(TypeTextParams::default()),
            Position { x: 100.0, y: 0.0 },
        );
        wf.add_edge(a, b);

        wf.remove_node(a);
        assert_eq!(wf.nodes.len(), 1);
        assert_eq!(wf.edges.len(), 0);
    }

    #[test]
    fn test_workflow_without_groups_deserializes() {
        let json = r#"{"id":"00000000-0000-0000-0000-000000000001","name":"Old Workflow","nodes":[],"edges":[]}"#;
        let wf: Workflow = serde_json::from_str(json).expect("should deserialize without groups");
        assert!(wf.groups.is_empty());
        assert!(wf.next_id_counters.is_empty());
    }

    #[test]
    fn test_node_group_serialization_roundtrip() {
        let mut wf = Workflow::new("Grouped Workflow");
        let a = wf.add_node(
            NodeType::Click(ClickParams::default()),
            Position { x: 0.0, y: 0.0 },
        );
        let b = wf.add_node(
            NodeType::TypeText(TypeTextParams::default()),
            Position { x: 100.0, y: 0.0 },
        );
        wf.groups.push(NodeGroup {
            id: Uuid::new_v4(),
            name: "Login Flow".to_string(),
            color: "#6366f1".to_string(),
            node_ids: vec![a, b],
            parent_group_id: None,
        });
        let json = serde_json::to_string_pretty(&wf).expect("serialize");
        let deserialized: Workflow = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(deserialized.groups.len(), 1);
        assert_eq!(deserialized.groups[0].name, "Login Flow");
        assert_eq!(deserialized.groups[0].node_ids.len(), 2);
    }

    #[test]
    fn test_auto_id_assigned_on_add_node() {
        let mut wf = Workflow::default();
        let a = wf.add_node(
            NodeType::FindText(FindTextParams::default()),
            Position { x: 0.0, y: 0.0 },
        );
        let b = wf.add_node(
            NodeType::FindText(FindTextParams::default()),
            Position { x: 100.0, y: 0.0 },
        );
        let node_a = wf.find_node(a).unwrap();
        let node_b = wf.find_node(b).unwrap();
        assert_eq!(node_a.auto_id, "find_text_1");
        assert_eq!(node_b.auto_id, "find_text_2");
    }

    /// Regression test for Round 1 finding R1.L1: every variant in
    /// `all_defaults()` must round-trip through `display_name` →
    /// `default_for_name`. The two registries were originally out of sync
    /// for the AX variants — this test pins the round-trip so any future
    /// variant addition that forgets one side fails the test rather than
    /// silently dropping off the Tauri palette.
    #[test]
    fn display_name_and_default_for_name_round_trip_all_defaults() {
        for nt in NodeType::all_defaults() {
            let name = nt.display_name();
            let resolved = NodeType::default_for_name(name).unwrap_or_else(|| {
                panic!(
                    "default_for_name is missing an arm for display_name \"{}\"",
                    name
                )
            });
            assert_eq!(
                std::mem::discriminant(&nt),
                std::mem::discriminant(&resolved),
                "default_for_name(\"{}\") returned the wrong NodeType variant",
                name,
            );
        }
    }

    /// Regression test for Round 1 finding R1.M1: AX dispatch nodes must
    /// surface their configured verification through
    /// `NodeType::verification()`. Before the fix the match arm fell
    /// through to `_ => None`, silently dropping any configured
    /// verification.
    #[test]
    fn ax_dispatch_nodes_surface_verification_config() {
        use crate::output_schema::{VerificationConfig, VerificationMethod};

        let verification = VerificationConfig {
            verification_method: Some(VerificationMethod::Vlm),
            verification_assertion: Some("button is highlighted".to_string()),
        };

        let click = NodeType::AxClick(AxClickParams {
            target: AxTarget::ResolvedUid("a1g1".into()),
            verification: verification.clone(),
        });
        let set_value = NodeType::AxSetValue(AxSetValueParams {
            target: AxTarget::ResolvedUid("a2g1".into()),
            value: "hello".into(),
            verification: verification.clone(),
        });
        let select = NodeType::AxSelect(AxSelectParams {
            target: AxTarget::ResolvedUid("a3g1".into()),
            verification: verification.clone(),
        });

        for (label, nt) in [
            ("AxClick", click),
            ("AxSetValue", set_value),
            ("AxSelect", select),
        ] {
            let got = nt.verification().unwrap_or_else(|| {
                panic!("{label}::verification() returned None despite configured method")
            });
            assert_eq!(got.verification_method, verification.verification_method);
            assert_eq!(
                got.verification_assertion,
                verification.verification_assertion
            );
        }
    }
}

#[cfg(test)]
mod node_provenance_tests {
    use super::*;
    use crate::node_params::CdpWaitParams;

    #[test]
    fn node_new_source_run_id_is_none() {
        let node = Node::new(
            NodeType::CdpWait(CdpWaitParams::default()),
            Position { x: 0.0, y: 0.0 },
            "test",
            "",
        );
        assert!(node.source_run_id.is_none());
    }

    #[test]
    fn node_missing_source_run_id_deserializes_as_none() {
        // Legacy workflows on disk have no `source_run_id` field.
        let json = r#"{
            "id": "00000000-0000-0000-0000-000000000001",
            "node_type": { "type": "CdpWait", "text": "", "timeout_ms": 1000 },
            "position": { "x": 0.0, "y": 0.0 },
            "name": "legacy",
            "enabled": true,
            "timeout_ms": null,
            "settle_ms": null,
            "retries": 0,
            "trace_level": "Minimal"
        }"#;
        let node: Node = serde_json::from_str(json).expect("parse");
        assert!(
            node.source_run_id.is_none(),
            "legacy nodes must deserialize with source_run_id = None"
        );
    }

    #[test]
    fn with_run_id_sets_field() {
        let node = Node::new(
            NodeType::CdpWait(CdpWaitParams::default()),
            Position { x: 0.0, y: 0.0 },
            "t",
            "",
        );
        let run_id = Uuid::new_v4();
        let stamped = node.with_run_id(run_id);
        assert_eq!(stamped.source_run_id, Some(run_id));
    }
}
