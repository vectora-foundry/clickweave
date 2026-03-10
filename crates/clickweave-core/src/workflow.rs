use crate::node_params::*;
use serde::{Deserialize, Serialize};
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
}

impl Default for Workflow {
    fn default() -> Self {
        Self {
            id: Uuid::new_v4(),
            name: "New Workflow".to_string(),
            nodes: vec![],
            edges: vec![],
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

impl Node {
    pub fn new(node_type: NodeType, position: Position, name: impl Into<String>) -> Self {
        Self {
            id: Uuid::new_v4(),
            node_type,
            position,
            name: name.into(),
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
        let node = Node::new(node_type, position, name);
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub enum NodeCategory {
    Ai,
    Vision,
    Input,
    Window,
    AppDebugKit,
    ControlFlow,
}

impl NodeCategory {
    pub fn display_name(&self) -> &'static str {
        match self {
            NodeCategory::Ai => "AI",
            NodeCategory::Vision => "Vision / Discovery",
            NodeCategory::Input => "Input",
            NodeCategory::Window => "Window",
            NodeCategory::AppDebugKit => "AppDebugKit",
            NodeCategory::ControlFlow => "Control Flow",
        }
    }

    pub fn icon(&self) -> &'static str {
        match self {
            NodeCategory::Ai => "🤖",
            NodeCategory::Vision => "👁",
            NodeCategory::Input => "🖱",
            NodeCategory::Window => "🪟",
            NodeCategory::AppDebugKit => "🔧",
            NodeCategory::ControlFlow => "🔀",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(tag = "type")]
pub enum NodeType {
    AiStep(AiStepParams),
    TakeScreenshot(TakeScreenshotParams),
    FindText(FindTextParams),
    FindImage(FindImageParams),
    Click(ClickParams),
    Hover(HoverParams),
    TypeText(TypeTextParams),
    PressKey(PressKeyParams),
    Scroll(ScrollParams),
    ListWindows(ListWindowsParams),
    FocusWindow(FocusWindowParams),
    McpToolCall(McpToolCallParams),
    AppDebugKitOp(AppDebugKitParams),
    If(IfParams),
    Switch(SwitchParams),
    Loop(LoopParams),
    EndLoop(EndLoopParams),
}

impl NodeType {
    pub fn category(&self) -> NodeCategory {
        match self {
            NodeType::AiStep(_) => NodeCategory::Ai,
            NodeType::TakeScreenshot(_) | NodeType::FindText(_) | NodeType::FindImage(_) => {
                NodeCategory::Vision
            }
            NodeType::Click(_)
            | NodeType::Hover(_)
            | NodeType::TypeText(_)
            | NodeType::PressKey(_)
            | NodeType::Scroll(_) => NodeCategory::Input,
            NodeType::ListWindows(_) | NodeType::FocusWindow(_) => NodeCategory::Window,
            NodeType::McpToolCall(_) | NodeType::AppDebugKitOp(_) => NodeCategory::AppDebugKit,
            NodeType::If(_) | NodeType::Switch(_) | NodeType::Loop(_) | NodeType::EndLoop(_) => {
                NodeCategory::ControlFlow
            }
        }
    }

    /// Returns true for node types that only observe/query state without modifying it.
    /// Used to skip supervision inside loops where "not found" is expected behavior.
    pub fn is_read_only(&self) -> bool {
        matches!(
            self,
            NodeType::FindText(_)
                | NodeType::FindImage(_)
                | NodeType::TakeScreenshot(_)
                | NodeType::ListWindows(_)
        )
    }

    pub fn display_name(&self) -> &'static str {
        match self {
            NodeType::AiStep(_) => "AI Step",
            NodeType::TakeScreenshot(_) => "Take Screenshot",
            NodeType::FindText(_) => "Find Text",
            NodeType::FindImage(_) => "Find Image",
            NodeType::Click(_) => "Click",
            NodeType::Hover(_) => "Hover",
            NodeType::TypeText(_) => "Type Text",
            NodeType::PressKey(_) => "Press Key",
            NodeType::Scroll(_) => "Scroll",
            NodeType::ListWindows(_) => "List Windows",
            NodeType::FocusWindow(_) => "Focus Window",
            NodeType::McpToolCall(_) => "MCP Tool Call",
            NodeType::AppDebugKitOp(_) => "AppDebugKit Op",
            NodeType::If(_) => "If",
            NodeType::Switch(_) => "Switch",
            NodeType::Loop(_) => "Loop",
            NodeType::EndLoop(_) => "End Loop",
        }
    }

    /// Human-readable description of what this node does, for LLM verification prompts.
    pub fn action_description(&self) -> String {
        match self {
            NodeType::Click(p) => match &p.target {
                Some(t) => format!("Clicked on '{}'", t.text()),
                None if p.template_image.is_some() => "Clicked on image match".to_string(),
                None => format!(
                    "Clicked at ({}, {})",
                    p.x.unwrap_or(0.0),
                    p.y.unwrap_or(0.0)
                ),
            },
            NodeType::Hover(p) => match &p.target {
                Some(t) => format!("Hovered over '{}'", t.text()),
                None if p.template_image.is_some() => "Hovered over image match".to_string(),
                None => format!(
                    "Hovered at ({}, {})",
                    p.x.unwrap_or(0.0),
                    p.y.unwrap_or(0.0)
                ),
            },
            NodeType::TypeText(p) => format!("Typed '{}'", p.text),
            NodeType::PressKey(p) => format!("Pressed key '{}'", p.key),
            NodeType::Scroll(p) => format!("Scrolled by {}", p.delta_y),
            NodeType::FocusWindow(p) => match &p.value {
                Some(v) => format!("Focused window '{}'", v),
                None => "Focused window".to_string(),
            },
            NodeType::ListWindows(_) => "Listed windows".to_string(),
            NodeType::FindText(p) => format!("Searched for text '{}'", p.search_text),
            NodeType::FindImage(_) => "Searched for image template".to_string(),
            NodeType::TakeScreenshot(_) => "Took a screenshot".to_string(),
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
            NodeType::Click(_) => "🖱",
            NodeType::Hover(_) => "👆",
            NodeType::TypeText(_) => "⌨",
            NodeType::PressKey(_) => "⌨",
            NodeType::Scroll(_) => "📜",
            NodeType::ListWindows(_) => "📋",
            NodeType::FocusWindow(_) => "🪟",
            NodeType::McpToolCall(_) => "🔧",
            NodeType::AppDebugKitOp(_) => "🔧",
            NodeType::If(_) | NodeType::Switch(_) => "\u{2442}",
            NodeType::Loop(_) | NodeType::EndLoop(_) => "\u{21BB}",
        }
    }

    pub fn is_deterministic(&self) -> bool {
        !matches!(self, NodeType::AiStep(_))
    }

    /// All available node types with default parameters.
    pub fn all_defaults() -> Vec<NodeType> {
        vec![
            NodeType::AiStep(AiStepParams::default()),
            NodeType::TakeScreenshot(TakeScreenshotParams::default()),
            NodeType::FindText(FindTextParams::default()),
            NodeType::FindImage(FindImageParams::default()),
            NodeType::Click(ClickParams::default()),
            NodeType::Hover(HoverParams::default()),
            NodeType::TypeText(TypeTextParams::default()),
            NodeType::PressKey(PressKeyParams::default()),
            NodeType::Scroll(ScrollParams::default()),
            NodeType::ListWindows(ListWindowsParams::default()),
            NodeType::FocusWindow(FocusWindowParams::default()),
            NodeType::McpToolCall(McpToolCallParams::default()),
            NodeType::AppDebugKitOp(AppDebugKitParams::default()),
            NodeType::If(IfParams {
                condition: Condition {
                    left: ValueRef::Variable {
                        name: String::new(),
                    },
                    operator: Operator::Equals,
                    right: ValueRef::Literal {
                        value: LiteralValue::Bool { value: true },
                    },
                },
            }),
            NodeType::Switch(SwitchParams { cases: vec![] }),
            NodeType::Loop(LoopParams {
                exit_condition: Condition {
                    left: ValueRef::Variable {
                        name: String::new(),
                    },
                    operator: Operator::Equals,
                    right: ValueRef::Literal {
                        value: LiteralValue::Bool { value: true },
                    },
                },
                max_iterations: 100,
            }),
            NodeType::EndLoop(EndLoopParams {
                loop_id: Uuid::nil(),
            }),
        ]
    }
}

/// Sanitize a node name for use as a variable prefix.
/// Converts to lowercase, replaces non-alphanumeric chars (except `_`) with underscores.
///
/// Examples: `"Find Text"` → `"find_text"`, `"Click (Login Button)"` → `"click__login_button_"`
pub fn sanitize_node_name(name: &str) -> String {
    name.to_lowercase()
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_node_type_serialization_roundtrip() {
        for nt in NodeType::all_defaults() {
            let json = serde_json::to_string(&nt).expect("serialize");
            let deserialized: NodeType = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(nt.display_name(), deserialized.display_name());
            assert_eq!(nt.category(), deserialized.category());
        }
    }

    #[test]
    fn test_node_type_category_correctness() {
        assert_eq!(
            NodeType::AiStep(AiStepParams::default()).category(),
            NodeCategory::Ai
        );
        assert_eq!(
            NodeType::TakeScreenshot(TakeScreenshotParams::default()).category(),
            NodeCategory::Vision
        );
        assert_eq!(
            NodeType::FindText(FindTextParams::default()).category(),
            NodeCategory::Vision
        );
        assert_eq!(
            NodeType::FindImage(FindImageParams::default()).category(),
            NodeCategory::Vision
        );
        assert_eq!(
            NodeType::Click(ClickParams::default()).category(),
            NodeCategory::Input
        );
        assert_eq!(
            NodeType::Hover(HoverParams::default()).category(),
            NodeCategory::Input
        );
        assert_eq!(
            NodeType::TypeText(TypeTextParams::default()).category(),
            NodeCategory::Input
        );
        assert_eq!(
            NodeType::Scroll(ScrollParams::default()).category(),
            NodeCategory::Input
        );
        assert_eq!(
            NodeType::ListWindows(ListWindowsParams::default()).category(),
            NodeCategory::Window
        );
        assert_eq!(
            NodeType::FocusWindow(FocusWindowParams::default()).category(),
            NodeCategory::Window
        );
        assert_eq!(
            NodeType::AppDebugKitOp(AppDebugKitParams::default()).category(),
            NodeCategory::AppDebugKit
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
        assert!(NodeType::ListWindows(ListWindowsParams::default()).is_deterministic());
        assert!(NodeType::FocusWindow(FocusWindowParams::default()).is_deterministic());
        assert!(NodeType::AppDebugKitOp(AppDebugKitParams::default()).is_deterministic());

        let dummy_condition = Condition {
            left: ValueRef::Variable {
                name: String::new(),
            },
            operator: Operator::Equals,
            right: ValueRef::Literal {
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
    fn test_all_defaults_covers_all_categories() {
        let defaults = NodeType::all_defaults();
        assert_eq!(defaults.len(), 17);

        let categories: std::collections::HashSet<NodeCategory> =
            defaults.iter().map(|nt| nt.category()).collect();
        assert!(categories.contains(&NodeCategory::Ai));
        assert!(categories.contains(&NodeCategory::Vision));
        assert!(categories.contains(&NodeCategory::Input));
        assert!(categories.contains(&NodeCategory::Window));
        assert!(categories.contains(&NodeCategory::AppDebugKit));
        assert!(categories.contains(&NodeCategory::ControlFlow));
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
        // No edges - both are entry points
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
        wf.add_edge(b, a); // cycle

        let order = wf.execution_order();
        // Should not hang, should visit each node at most once
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
    fn test_sanitize_node_name_simple() {
        assert_eq!(sanitize_node_name("Find Text"), "find_text");
    }

    #[test]
    fn test_sanitize_node_name_special_chars() {
        assert_eq!(
            sanitize_node_name("Click (Login Button)"),
            "click__login_button_"
        );
    }

    #[test]
    fn test_sanitize_node_name_preserves_underscores() {
        assert_eq!(sanitize_node_name("my_node_1"), "my_node_1");
    }

    #[test]
    fn test_sanitize_node_name_empty() {
        assert_eq!(sanitize_node_name(""), "");
    }

    #[test]
    fn test_condition_serialization_roundtrip() {
        let conditions = vec![
            Condition {
                left: ValueRef::Variable {
                    name: "result".to_string(),
                },
                operator: Operator::Equals,
                right: ValueRef::Literal {
                    value: LiteralValue::String {
                        value: "success".to_string(),
                    },
                },
            },
            Condition {
                left: ValueRef::Variable {
                    name: "count".to_string(),
                },
                operator: Operator::GreaterThan,
                right: ValueRef::Literal {
                    value: LiteralValue::Number { value: 5.0 },
                },
            },
            Condition {
                left: ValueRef::Variable {
                    name: "done".to_string(),
                },
                operator: Operator::Equals,
                right: ValueRef::Literal {
                    value: LiteralValue::Bool { value: true },
                },
            },
        ];
        for condition in &conditions {
            let json = serde_json::to_string(condition).expect("serialize Condition");
            let deserialized: Condition =
                serde_json::from_str(&json).expect("deserialize Condition");
            // Verify round-trip by re-serializing
            let json2 = serde_json::to_string(&deserialized).expect("re-serialize Condition");
            assert_eq!(json, json2);
        }
    }
}
