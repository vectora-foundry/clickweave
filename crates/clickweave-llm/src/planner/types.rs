use std::collections::HashMap;

use clickweave_core::{Edge, Node, Workflow};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

// ── Public types ────────────────────────────────────────────────

/// A single step in the planner's output.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "step_type")]
pub enum PlanStep {
    Tool {
        tool_name: String,
        arguments: Value,
        #[serde(default)]
        name: Option<String>,
    },
    AiTransform {
        kind: String,
        input_ref: String,
        #[serde(default)]
        output_schema: Option<Value>,
        #[serde(default)]
        name: Option<String>,
    },
    AiStep {
        prompt: String,
        #[serde(default)]
        allowed_tools: Option<Vec<String>>,
        #[serde(default)]
        max_tool_calls: Option<u32>,
        #[serde(default)]
        timeout_ms: Option<u64>,
        #[serde(default)]
        name: Option<String>,
    },
    If {
        #[serde(default)]
        name: Option<String>,
        condition: clickweave_core::Condition,
    },
    Loop {
        #[serde(default)]
        name: Option<String>,
        exit_condition: clickweave_core::Condition,
        #[serde(default)]
        max_iterations: Option<u32>,
    },
    EndLoop {
        #[serde(default)]
        name: Option<String>,
        loop_id: String,
    },
    /// Catch-all for unrecognised step types (e.g. LLM-invented "End").
    /// Nodes with this variant are silently filtered out during workflow construction.
    #[serde(other)]
    Unknown,
}

/// The raw planner LLM output.
#[derive(Debug, Deserialize)]
pub struct PlannerOutput {
    #[serde(default)]
    pub steps: Vec<Value>,
}

/// A node in the graph-based planner output.
#[derive(Debug, Clone, Deserialize)]
pub struct PlanNode {
    pub id: String,
    #[serde(flatten)]
    pub step: PlanStep,
    #[serde(default)]
    pub role: Option<String>,
    #[serde(default)]
    pub expected_outcome: Option<String>,
}

/// An edge in the graph-based planner output.
#[derive(Debug, Clone, Deserialize)]
pub struct PlanEdge {
    pub from: String,
    pub to: String,
    #[serde(default)]
    pub output: Option<clickweave_core::EdgeOutput>,
}

/// Graph-based planner output (for control flow workflows).
///
/// All collections are kept as raw `Value` so that individual malformed
/// entries (missing required fields, unknown enum variants) don't crash
/// the entire deserialization — they are parsed one-by-one during
/// workflow construction.
#[derive(Debug, Deserialize)]
pub struct PlannerGraphOutput {
    pub nodes: Vec<Value>,
    #[serde(default)]
    pub edges: Vec<Value>,
}

/// Result of planning a workflow.
#[derive(Debug)]
pub struct PlanResult {
    pub workflow: Workflow,
    pub warnings: Vec<String>,
}

// ── Patch types ─────────────────────────────────────────────────

/// Output from the patcher LLM.
#[derive(Debug, Deserialize)]
pub(crate) struct PatcherOutput {
    #[serde(default)]
    pub add: Vec<Value>,
    #[serde(default)]
    pub add_nodes: Vec<Value>,
    #[serde(default)]
    pub add_edges: Vec<Value>,
    #[serde(default)]
    pub remove_node_ids: Vec<String>,
    #[serde(default)]
    pub update: Vec<Value>,
}

/// A node update from the patcher (only changed fields).
#[derive(Debug, Deserialize)]
pub(crate) struct PatchNodeUpdate {
    pub node_id: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub node_type: Option<Value>,
    /// Flat alternative: LLMs often echo the node summary format.
    #[serde(default)]
    pub tool_name: Option<String>,
    #[serde(default)]
    pub arguments: Option<Value>,
}

/// Result of patching a workflow.
pub struct PatchResult {
    pub added_nodes: Vec<Node>,
    pub removed_node_ids: Vec<Uuid>,
    pub updated_nodes: Vec<Node>,
    pub added_edges: Vec<Edge>,
    pub removed_edges: Vec<Edge>,
    pub warnings: Vec<String>,
    /// Maps added_node_id -> anchor_node_id for `insert_before` splicing.
    pub insert_before_map: HashMap<Uuid, Uuid>,
}
