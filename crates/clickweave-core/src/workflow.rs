use crate::node_params::*;
use crate::output_schema::{NodeContext, OutputRole};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use uuid::Uuid;

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
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct Position {
    pub x: f32,
    pub y: f32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(tag = "type")]
pub enum EdgeOutput {
    IfTrue,
    IfFalse,
    SwitchCase {
        name: String,
    },
    SwitchDefault,
    /// Edge from Loop node into the loop body.
    LoopBody,
    /// Edge from Loop node when exit condition is met (or max iterations hit).
    LoopDone,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct Edge {
    pub from: Uuid,
    pub to: Uuid,
    /// Which output port this edge connects from. None for regular single-output edges.
    pub output: Option<EdgeOutput>,
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
            auto_id: auto_id.into(),
            enabled: true,
            timeout_ms: None,
            settle_ms: None,
            retries: 0,
            supervision_retries: default_supervision_retries(),
            trace_level: TraceLevel::Minimal,
            role: NodeRole::Default,
            expected_outcome: None,
        }
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
        self.edges.push(Edge {
            from,
            to,
            output: None,
        });
    }

    pub fn add_edge_with_output(&mut self, from: Uuid, to: Uuid, output: EdgeOutput) {
        self.edges.push(Edge {
            from,
            to,
            output: Some(output),
        });
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

    /// Find entry points: nodes with no incoming edges.
    /// EndLoop back-edges are excluded so loops don't break entry detection.
    fn entry_points(&self) -> Vec<Uuid> {
        let endloop_ids: std::collections::HashSet<Uuid> = self
            .nodes
            .iter()
            .filter(|n| matches!(n.node_type, NodeType::EndLoop(_)))
            .map(|n| n.id)
            .collect();

        let targets: std::collections::HashSet<Uuid> = self
            .edges
            .iter()
            .filter(|e| !endloop_ids.contains(&e.from))
            .map(|e| e.to)
            .collect();

        self.nodes
            .iter()
            .filter(|n| !targets.contains(&n.id))
            .map(|n| n.id)
            .collect()
    }

    /// Get execution order by walking edges from entry points linearly.
    ///
    /// Note: This only handles linear workflows. For workflows with control
    /// flow (If, Switch, Loop), use the graph walker in the engine instead.
    pub fn execution_order(&self) -> Vec<Uuid> {
        let entries = self.entry_points();
        if entries.is_empty() {
            return self.nodes.iter().map(|n| n.id).collect();
        }

        let mut order = Vec::new();
        let mut visited = std::collections::HashSet::new();

        for entry in entries {
            let mut current = entry;
            while visited.insert(current) {
                order.push(current);
                match self.edges.iter().find(|e| e.from == current) {
                    Some(edge) => current = edge.to,
                    None => break,
                }
            }
        }

        order
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
    // AI
    AiStep(AiStepParams),
    // Control Flow
    If(IfParams),
    Switch(SwitchParams),
    Loop(LoopParams),
    EndLoop(EndLoopParams),
    // Generic
    McpToolCall(McpToolCallParams),
    AppDebugKitOp(AppDebugKitParams),
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
            | Self::CdpHandleDialog(_) => OutputRole::Action,

            Self::AiStep(_) => OutputRole::Ai,

            Self::If(_) | Self::Switch(_) | Self::Loop(_) | Self::EndLoop(_) => {
                OutputRole::ControlFlow
            }

            Self::McpToolCall(_) | Self::AppDebugKitOp(_) => OutputRole::Generic,
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
            | Self::QuitApp(_) => NodeContext::Native,

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
            NodeType::AiStep(_) => "AI Step",
            NodeType::If(_) => "If",
            NodeType::Switch(_) => "Switch",
            NodeType::Loop(_) => "Loop",
            NodeType::EndLoop(_) => "End Loop",
            NodeType::McpToolCall(_) => "MCP Tool Call",
            NodeType::AppDebugKitOp(_) => "AppDebugKit Op",
        }
    }

    /// Human-readable description of what this node does, for LLM verification prompts.
    pub fn action_description(&self) -> String {
        match self {
            NodeType::Click(p) => {
                if let Some(ref r) = p.target_ref {
                    return format!("Click at {{{}.{}}}", r.node, r.field);
                }
                match &p.target {
                    Some(t) if !t.text().is_empty() => format!("Clicked on '{}'", t.text()),
                    Some(ClickTarget::Coordinates { x, y }) => {
                        format!("Clicked at ({}, {})", x, y)
                    }
                    Some(ClickTarget::WindowControl { action }) => {
                        format!("Clicked {}", action.display_name().to_lowercase())
                    }
                    _ => "Clicked".to_string(),
                }
            }
            NodeType::Hover(p) => {
                if let Some(ref r) = p.target_ref {
                    return format!("Hover at {{{}.{}}}", r.node, r.field);
                }
                match &p.target {
                    Some(t) if !t.text().is_empty() => format!("Hovered over '{}'", t.text()),
                    Some(ClickTarget::Coordinates { x, y }) => {
                        format!("Hovered at ({}, {})", x, y)
                    }
                    Some(ClickTarget::WindowControl { action }) => {
                        format!("Hovered {}", action.display_name().to_lowercase())
                    }
                    _ => "Hovered".to_string(),
                }
            }
            NodeType::Drag(p) => {
                if p.from_ref.is_some() || p.to_ref.is_some() {
                    return "Dragged (using refs)".to_string();
                }
                format!(
                    "Dragged from ({}, {}) to ({}, {})",
                    p.from_x.unwrap_or(0.0),
                    p.from_y.unwrap_or(0.0),
                    p.to_x.unwrap_or(0.0),
                    p.to_y.unwrap_or(0.0)
                )
            }
            NodeType::TypeText(p) => {
                if let Some(ref r) = p.text_ref {
                    return format!("Type {{{}.{}}}", r.node, r.field);
                }
                format!("Typed '{}'", p.text)
            }
            NodeType::PressKey(p) => format!("Pressed key '{}'", p.key),
            NodeType::Scroll(p) => format!("Scrolled by {}", p.delta_y),
            NodeType::FocusWindow(p) => {
                if let Some(ref r) = p.value_ref {
                    return format!("Focus window {{{}.{}}}", r.node, r.field);
                }
                match &p.value {
                    Some(v) => format!("Focused window '{}'", v),
                    None => "Focused window".to_string(),
                }
            }
            NodeType::LaunchApp(p) => format!("Launched app '{}'", p.app_name),
            NodeType::QuitApp(p) => format!("Quit app '{}'", p.app_name),
            NodeType::FindText(p) => format!("Searched for text '{}'", p.search_text),
            NodeType::FindImage(_) => "Searched for image template".to_string(),
            NodeType::FindApp(p) => format!("Searched for app '{}'", p.search),
            NodeType::TakeScreenshot(_) => "Took a screenshot".to_string(),
            NodeType::CdpWait(p) => format!("Waited for text '{}'", p.text),
            NodeType::CdpClick(p) => format!("CDP clicked element '{}'", p.target.as_str()),
            NodeType::CdpHover(p) => format!("CDP hovered element '{}'", p.target.as_str()),
            NodeType::CdpFill(p) => {
                if let Some(ref r) = p.value_ref {
                    return format!("CDP filled with {{{}.{}}}", r.node, r.field);
                }
                format!("CDP filled with '{}'", p.value)
            }
            NodeType::CdpType(p) => {
                if let Some(ref r) = p.text_ref {
                    return format!("CDP typed {{{}.{}}}", r.node, r.field);
                }
                format!("CDP typed '{}'", p.text)
            }
            NodeType::CdpPressKey(p) => format!("CDP pressed key '{}'", p.key),
            NodeType::CdpNavigate(p) => {
                if let Some(ref r) = p.url_ref {
                    return format!("CDP navigated to {{{}.{}}}", r.node, r.field);
                }
                format!("CDP navigated to '{}'", p.url)
            }
            NodeType::CdpNewPage(p) => {
                if let Some(ref r) = p.url_ref {
                    return format!("CDP opened new page {{{}.{}}}", r.node, r.field);
                }
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
            NodeType::Click(_) | NodeType::CdpClick(_) => "🖱",
            NodeType::Hover(_) | NodeType::CdpHover(_) => "👆",
            NodeType::Drag(_) => "↔",
            NodeType::TypeText(_) | NodeType::CdpType(_) | NodeType::CdpFill(_) => "⌨",
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
            NodeType::McpToolCall(_) | NodeType::AppDebugKitOp(_) => "🔧",
            NodeType::If(_) | NodeType::Switch(_) => "\u{2442}",
            NodeType::Loop(_) | NodeType::EndLoop(_) => "\u{21BB}",
        }
    }

    pub fn is_deterministic(&self) -> bool {
        !matches!(self, NodeType::AiStep(_))
    }

    /// Returns the verification method configured on this node, if any.
    pub fn verification_method(&self) -> Option<crate::output_schema::VerificationMethod> {
        match self {
            Self::Click(p) => p.verification_method,
            Self::Hover(p) => p.verification_method,
            Self::Drag(p) => p.verification_method,
            Self::TypeText(p) => p.verification_method,
            Self::PressKey(p) => p.verification_method,
            Self::Scroll(p) => p.verification_method,
            Self::FocusWindow(p) => p.verification_method,
            Self::LaunchApp(p) => p.verification_method,
            Self::QuitApp(p) => p.verification_method,
            Self::CdpClick(p) => p.verification_method,
            Self::CdpHover(p) => p.verification_method,
            Self::CdpFill(p) => p.verification_method,
            Self::CdpType(p) => p.verification_method,
            Self::CdpPressKey(p) => p.verification_method,
            Self::CdpNavigate(p) => p.verification_method,
            Self::CdpNewPage(p) => p.verification_method,
            Self::CdpClosePage(p) => p.verification_method,
            Self::CdpSelectPage(p) => p.verification_method,
            Self::CdpHandleDialog(p) => p.verification_method,
            _ => None,
        }
    }

    /// Returns the verification assertion configured on this node, if any.
    pub fn verification_assertion(&self) -> Option<&str> {
        match self {
            Self::Click(p) => p.verification_assertion.as_deref(),
            Self::Hover(p) => p.verification_assertion.as_deref(),
            Self::Drag(p) => p.verification_assertion.as_deref(),
            Self::TypeText(p) => p.verification_assertion.as_deref(),
            Self::PressKey(p) => p.verification_assertion.as_deref(),
            Self::Scroll(p) => p.verification_assertion.as_deref(),
            Self::FocusWindow(p) => p.verification_assertion.as_deref(),
            Self::LaunchApp(p) => p.verification_assertion.as_deref(),
            Self::QuitApp(p) => p.verification_assertion.as_deref(),
            Self::CdpClick(p) => p.verification_assertion.as_deref(),
            Self::CdpHover(p) => p.verification_assertion.as_deref(),
            Self::CdpFill(p) => p.verification_assertion.as_deref(),
            Self::CdpType(p) => p.verification_assertion.as_deref(),
            Self::CdpPressKey(p) => p.verification_assertion.as_deref(),
            Self::CdpNavigate(p) => p.verification_assertion.as_deref(),
            Self::CdpNewPage(p) => p.verification_assertion.as_deref(),
            Self::CdpClosePage(p) => p.verification_assertion.as_deref(),
            Self::CdpSelectPage(p) => p.verification_assertion.as_deref(),
            Self::CdpHandleDialog(p) => p.verification_assertion.as_deref(),
            _ => None,
        }
    }

    /// Returns true when both `verification_method` and `verification_assertion`
    /// are set, matching the executor's requirement for producing verification
    /// output variables.
    pub fn has_verification(&self) -> bool {
        self.verification_method().is_some()
            && self.verification_assertion().is_some_and(|a| !a.is_empty())
    }

    /// Returns all `(input_field_name, OutputRef)` pairs set on this node.
    ///
    /// The input field name corresponds to the `InputField::name` from
    /// `input_schema()`, allowing callers to look up `accepted_types`.
    pub fn ref_params(&self) -> Vec<(&'static str, &crate::output_schema::OutputRef)> {
        let mut refs = Vec::new();
        match self {
            Self::Click(p) => {
                if let Some(ref r) = p.target_ref {
                    refs.push(("target_ref", r));
                }
            }
            Self::Hover(p) => {
                if let Some(ref r) = p.target_ref {
                    refs.push(("target_ref", r));
                }
            }
            Self::Drag(p) => {
                if let Some(ref r) = p.from_ref {
                    refs.push(("from_ref", r));
                }
                if let Some(ref r) = p.to_ref {
                    refs.push(("to_ref", r));
                }
            }
            Self::TypeText(p) => {
                if let Some(ref r) = p.text_ref {
                    refs.push(("text_ref", r));
                }
            }
            Self::FocusWindow(p) => {
                if let Some(ref r) = p.value_ref {
                    refs.push(("value_ref", r));
                }
            }
            Self::AiStep(p) => {
                if let Some(ref r) = p.prompt_ref {
                    refs.push(("prompt_ref", r));
                }
            }
            Self::CdpFill(p) => {
                if let Some(ref r) = p.value_ref {
                    refs.push(("value_ref", r));
                }
            }
            Self::CdpType(p) => {
                if let Some(ref r) = p.text_ref {
                    refs.push(("text_ref", r));
                }
            }
            Self::CdpNavigate(p) => {
                if let Some(ref r) = p.url_ref {
                    refs.push(("url_ref", r));
                }
            }
            Self::CdpNewPage(p) => {
                if let Some(ref r) = p.url_ref {
                    refs.push(("url_ref", r));
                }
            }
            _ => {}
        }
        refs
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
            // AI
            NodeType::AiStep(AiStepParams::default()),
            // Control Flow
            NodeType::If(IfParams {
                condition: Condition {
                    left: crate::output_schema::OutputRef {
                        node: String::new(),
                        field: String::new(),
                    },
                    operator: Operator::Equals,
                    right: crate::output_schema::ConditionValue::Literal {
                        value: LiteralValue::Bool { value: true },
                    },
                },
            }),
            NodeType::Switch(SwitchParams { cases: vec![] }),
            NodeType::Loop(LoopParams {
                exit_condition: Condition {
                    left: crate::output_schema::OutputRef {
                        node: String::new(),
                        field: String::new(),
                    },
                    operator: Operator::Equals,
                    right: crate::output_schema::ConditionValue::Literal {
                        value: LiteralValue::Bool { value: true },
                    },
                },
                max_iterations: 100,
            }),
            NodeType::EndLoop(EndLoopParams {
                loop_id: Uuid::nil(),
            }),
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
    use crate::output_schema::{ConditionValue, OutputRef};

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

        let dummy_condition = Condition {
            left: OutputRef {
                node: String::new(),
                field: String::new(),
            },
            operator: Operator::Equals,
            right: ConditionValue::Literal {
                value: LiteralValue::Bool { value: true },
            },
        };
        assert!(
            NodeType::If(IfParams {
                condition: dummy_condition.clone()
            })
            .is_deterministic()
        );
        assert!(NodeType::Switch(SwitchParams { cases: vec![] }).is_deterministic());
        assert!(
            NodeType::Loop(LoopParams {
                exit_condition: dummy_condition,
                max_iterations: 100
            })
            .is_deterministic()
        );
        assert!(
            NodeType::EndLoop(EndLoopParams {
                loop_id: Uuid::nil()
            })
            .is_deterministic()
        );
    }

    #[test]
    fn test_all_defaults_covers_all_roles() {
        let defaults = NodeType::all_defaults();
        assert_eq!(defaults.len(), 31);

        let roles: std::collections::HashSet<OutputRole> =
            defaults.iter().map(|nt| nt.output_role()).collect();
        assert!(roles.contains(&OutputRole::Ai));
        assert!(roles.contains(&OutputRole::Query));
        assert!(roles.contains(&OutputRole::Action));
        assert!(roles.contains(&OutputRole::Generic));
        assert!(roles.contains(&OutputRole::ControlFlow));
    }

    #[test]
    fn test_execution_order_single_entry() {
        let mut wf = Workflow::default();
        let a = wf.add_node(
            NodeType::Click(ClickParams::default()),
            Position { x: 0.0, y: 0.0 },
        );
        let b = wf.add_node(
            NodeType::TypeText(TypeTextParams::default()),
            Position { x: 100.0, y: 0.0 },
        );
        let c = wf.add_node(
            NodeType::Scroll(ScrollParams::default()),
            Position { x: 200.0, y: 0.0 },
        );
        wf.add_edge(a, b);
        wf.add_edge(b, c);

        let order = wf.execution_order();
        assert_eq!(order, vec![a, b, c]);
    }

    #[test]
    fn test_execution_order_no_nodes() {
        let wf = Workflow::default();
        assert!(wf.execution_order().is_empty());
    }

    #[test]
    fn test_execution_order_disconnected() {
        let mut wf = Workflow::default();
        let a = wf.add_node(
            NodeType::Click(ClickParams::default()),
            Position { x: 0.0, y: 0.0 },
        );
        let b = wf.add_node(
            NodeType::TypeText(TypeTextParams::default()),
            Position { x: 100.0, y: 0.0 },
        );
        let order = wf.execution_order();
        assert_eq!(order.len(), 2);
        assert!(order.contains(&a));
        assert!(order.contains(&b));
    }

    #[test]
    fn test_execution_order_cycle_safety() {
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
        wf.add_edge(b, a);

        let order = wf.execution_order();
        assert!(order.len() <= 2);
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
    fn test_edge_output_serialization_roundtrip() {
        let variants = vec![
            EdgeOutput::IfTrue,
            EdgeOutput::IfFalse,
            EdgeOutput::SwitchCase {
                name: "Has error".to_string(),
            },
            EdgeOutput::SwitchDefault,
            EdgeOutput::LoopBody,
            EdgeOutput::LoopDone,
        ];
        for variant in &variants {
            let json = serde_json::to_string(variant).expect("serialize EdgeOutput");
            let deserialized: EdgeOutput =
                serde_json::from_str(&json).expect("deserialize EdgeOutput");
            assert_eq!(*variant, deserialized);
        }
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
    fn test_condition_serialization_roundtrip() {
        let conditions = vec![
            Condition {
                left: OutputRef {
                    node: "result".to_string(),
                    field: "result".to_string(),
                },
                operator: Operator::Equals,
                right: ConditionValue::Literal {
                    value: LiteralValue::String {
                        value: "success".to_string(),
                    },
                },
            },
            Condition {
                left: OutputRef {
                    node: "count".to_string(),
                    field: "result".to_string(),
                },
                operator: Operator::GreaterThan,
                right: ConditionValue::Literal {
                    value: LiteralValue::Number { value: 5.0 },
                },
            },
            Condition {
                left: OutputRef {
                    node: "done".to_string(),
                    field: "result".to_string(),
                },
                operator: Operator::Equals,
                right: ConditionValue::Literal {
                    value: LiteralValue::Bool { value: true },
                },
            },
        ];
        for condition in &conditions {
            let json = serde_json::to_string(condition).expect("serialize Condition");
            let deserialized: Condition =
                serde_json::from_str(&json).expect("deserialize Condition");
            let json2 = serde_json::to_string(&deserialized).expect("re-serialize Condition");
            assert_eq!(json, json2);
        }
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
}
