mod mapping;
mod parse;
mod patch;
mod plan;
mod prompt;
pub mod tool_use;

pub mod assistant;
pub mod conversation;
pub mod conversation_loop;
pub mod summarize;

#[cfg(test)]
mod tests;

use std::collections::{HashMap, HashSet};

use clickweave_core::{
    ConditionValue, Edge, EdgeOutput, Node, NodeRole, NodeType, OutputRef, Position, Workflow,
    tool_mapping,
};
use mapping::step_to_node_type;
use parse::{id_str_short, layout_nodes, step_rejected_reason};
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
    pub output: Option<EdgeOutput>,
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

/// Parse a slice of raw JSON values into typed items, skipping malformed
/// entries with warnings. Uses the `"id"` field (if present) to label
/// warnings; falls back to the array index.
fn parse_lenient<T: serde::de::DeserializeOwned>(raw: &[Value]) -> (Vec<T>, Vec<String>) {
    let mut items = Vec::new();
    let mut warnings = Vec::new();
    for (i, val) in raw.iter().enumerate() {
        match serde_json::from_value::<T>(val.clone()) {
            Ok(item) => items.push(item),
            Err(e) => {
                let label = val
                    .get("id")
                    .and_then(|v| v.as_str())
                    .or_else(|| val.get("from").and_then(|v| v.as_str()))
                    .map(String::from)
                    .unwrap_or_else(|| format!("#{}", i));
                warnings.push(format!("'{}' skipped (malformed): {}", label, e));
            }
        }
    }
    (items, warnings)
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
}

// ── Shared patch-building logic ─────────────────────────────────

/// Resolve a `PlanStep` from a `PatchNodeUpdate`.
///
/// Returns `Ok(Some(step))` when the update specifies a node_type change,
/// `Ok(None)` when it doesn't, and `Err(msg)` when parsing/inference fails.
pub(crate) fn resolve_update_step(
    update: &PatchNodeUpdate,
    existing_node_type: &NodeType,
) -> std::result::Result<Option<PlanStep>, String> {
    if let Some(nt_value) = &update.node_type {
        return serde_json::from_value::<PlanStep>(nt_value.clone())
            .map(Some)
            .map_err(|e| format!("failed to parse node_type: {}", e));
    }

    if update.tool_name.is_none() && update.arguments.is_none() {
        return Ok(None);
    }

    let tool_name = update.tool_name.clone().or_else(|| {
        tool_mapping::node_type_to_tool_invocation(existing_node_type)
            .ok()
            .map(|inv| inv.name)
    });

    let tool_name =
        tool_name.ok_or("arguments provided but cannot determine tool_name".to_string())?;

    Ok(Some(PlanStep::Tool {
        tool_name,
        arguments: update
            .arguments
            .clone()
            .unwrap_or(Value::Object(Default::default())),
        name: update.name.clone(),
    }))
}

/// Build a `PatchResult` from a `PatcherOutput` and the current workflow.
pub(crate) fn build_patch_from_output(
    output: &PatcherOutput,
    workflow: &Workflow,
    mcp_tools: &[Value],
    allow_ai_transforms: bool,
    allow_agent_steps: bool,
) -> PatchResult {
    let mut warnings = Vec::new();

    // Added nodes
    let mut added_nodes = Vec::new();
    let mut added_edges = Vec::new();
    let mut next_id_counters = workflow.next_id_counters.clone();
    let last_y = workflow
        .nodes
        .iter()
        .map(|n| n.position.y)
        .fold(0.0_f32, f32::max);

    // Reject mixed add + add_nodes — flat items would be left unwired
    if !output.add.is_empty() && !output.add_nodes.is_empty() {
        warnings.push(format!(
            "Ignored {} flat 'add' steps because 'add_nodes' is also present (mixed formats not supported)",
            output.add.len(),
        ));
    } else {
        let (add_steps, add_warnings) = parse_lenient::<plan::FlatPlanStep>(&output.add);
        warnings.extend(add_warnings);
        for (i, flat) in add_steps.iter().enumerate() {
            if let Some(reason) =
                step_rejected_reason(&flat.step, allow_ai_transforms, allow_agent_steps)
            {
                warnings.push(format!("Added step {} removed: {}", i, reason));
                continue;
            }
            match step_to_node_type(&flat.step, mcp_tools) {
                Ok((node_type, display_name)) => {
                    let auto_id =
                        clickweave_core::auto_id::assign_auto_id(&node_type, &mut next_id_counters);
                    let mut node = Node::new(
                        node_type,
                        Position {
                            x: 300.0,
                            y: last_y + 120.0 + (i as f32) * 120.0,
                        },
                        display_name,
                        auto_id,
                    );
                    if flat.role.as_deref() == Some("Verification") {
                        node.role = NodeRole::Verification;
                    }
                    node.expected_outcome = flat.expected_outcome.clone();
                    added_nodes.push(node);
                }
                Err(e) => warnings.push(format!("Added step {} skipped: {}", i, e)),
            }
        }
    }

    // Graph-based additions (add_nodes + add_edges)
    if !output.add_nodes.is_empty() {
        let mut id_map: HashMap<String, Uuid> = HashMap::new();
        // Map existing workflow node UUIDs so edges can reference them
        for node in &workflow.nodes {
            id_map.insert(node.id.to_string(), node.id);
        }

        let positions: Vec<Position> = (0..output.add_nodes.len())
            .map(|i| Position {
                x: 300.0,
                y: last_y + 120.0 + (i as f32) * 120.0,
            })
            .collect();

        let (new_nodes, new_edges, graph_warnings) = build_nodes_and_edges_from_graph(
            &output.add_nodes,
            &output.add_edges,
            &positions,
            &mut id_map,
            &mut next_id_counters,
            mcp_tools,
            allow_ai_transforms,
            allow_agent_steps,
        );
        added_nodes.extend(new_nodes);
        added_edges.extend(new_edges);
        warnings.extend(graph_warnings);
    }

    // Removed nodes
    let mut removed_node_ids = Vec::new();
    for id_str in &output.remove_node_ids {
        let id = match id_str.parse::<Uuid>() {
            Ok(id) => id,
            Err(_) => {
                warnings.push(format!("Remove: invalid node ID: {}", id_str));
                continue;
            }
        };
        if workflow.nodes.iter().any(|n| n.id == id) {
            removed_node_ids.push(id);
        } else {
            warnings.push(format!("Remove: node {} not found in workflow", id_str));
        }
    }

    // Updated nodes
    let (updates, update_warnings) = parse_lenient::<PatchNodeUpdate>(&output.update);
    warnings.extend(update_warnings);
    let mut updated_nodes = Vec::new();
    for update in &updates {
        let id = match update.node_id.parse::<Uuid>() {
            Ok(id) => id,
            Err(_) => {
                warnings.push(format!("Update: invalid node ID: {}", update.node_id));
                continue;
            }
        };
        let Some(existing) = workflow.nodes.iter().find(|n| n.id == id) else {
            warnings.push(format!("Update: node {} not found", update.node_id));
            continue;
        };
        let mut node = existing.clone();
        if let Some(name) = &update.name {
            node.name = name.clone();
        }
        let short_id = id_str_short(&id);
        match resolve_update_step(update, &existing.node_type) {
            Ok(Some(step)) => {
                if let Some(reason) =
                    step_rejected_reason(&step, allow_ai_transforms, allow_agent_steps)
                {
                    warnings.push(format!("Update {}: {}", short_id, reason));
                } else {
                    match step_to_node_type(&step, mcp_tools) {
                        Ok((node_type, _)) => node.node_type = node_type,
                        Err(e) => warnings.push(format!("Update {}: {}", short_id, e)),
                    }
                }
            }
            Ok(None) => {}
            Err(msg) => warnings.push(format!("Update {}: {}", short_id, msg)),
        }
        updated_nodes.push(node);
    }

    // Edges
    let mut removed_edges = Vec::new();

    // Linear edges for flat 'add' (only when graph-based add_nodes was NOT used)
    if output.add_nodes.is_empty() && !added_nodes.is_empty() {
        let last_existing = workflow
            .nodes
            .iter()
            .rev()
            .find(|n| !removed_node_ids.contains(&n.id));
        if let Some(last) = last_existing {
            added_edges.push(Edge {
                from: last.id,
                to: added_nodes[0].id,
                output: None,
            });
        }
        for pair in added_nodes.windows(2) {
            added_edges.push(Edge {
                from: pair[0].id,
                to: pair[1].id,
                output: None,
            });
        }
    }

    for edge in &workflow.edges {
        if removed_node_ids.contains(&edge.from) || removed_node_ids.contains(&edge.to) {
            removed_edges.push(edge.clone());
        }
    }

    // Translate OutputRefs in added/updated nodes from LLM temp IDs to auto-IDs
    let name_to_auto_id: HashMap<String, String> = workflow
        .nodes
        .iter()
        .chain(added_nodes.iter())
        .filter(|n| !n.auto_id.is_empty())
        .map(|n| (n.name.clone(), n.auto_id.clone()))
        .collect();
    for node in added_nodes.iter_mut().chain(updated_nodes.iter_mut()) {
        translate_node_refs(&name_to_auto_id, node);
    }

    PatchResult {
        added_nodes,
        removed_node_ids,
        updated_nodes,
        added_edges,
        removed_edges,
        warnings,
    }
}

/// Build a `PatchResult` from planner steps (all adds, no removes/updates).
pub(crate) fn build_plan_as_patch(
    raw_steps: &[Value],
    mcp_tools: &[Value],
    allow_ai_transforms: bool,
    allow_agent_steps: bool,
) -> PatchResult {
    let mut warnings = Vec::new();

    let (flat_steps, step_warnings) = parse_lenient::<plan::FlatPlanStep>(raw_steps);
    warnings.extend(step_warnings);

    let mut valid_steps = Vec::new();

    for (i, flat) in flat_steps.iter().enumerate() {
        if let Some(reason) =
            step_rejected_reason(&flat.step, allow_ai_transforms, allow_agent_steps)
        {
            warnings.push(format!("Step {} removed: {}", i, reason));
            continue;
        }
        valid_steps.push(flat);
    }

    let positions = layout_nodes(valid_steps.len());
    let mut added_nodes = Vec::new();
    let mut next_id_counters = HashMap::new();

    for (i, flat) in valid_steps.iter().enumerate() {
        match step_to_node_type(&flat.step, mcp_tools) {
            Ok((node_type, display_name)) => {
                let auto_id =
                    clickweave_core::auto_id::assign_auto_id(&node_type, &mut next_id_counters);
                let mut node = Node::new(node_type, positions[i], display_name, auto_id);
                if flat.role.as_deref() == Some("Verification") {
                    node.role = NodeRole::Verification;
                }
                node.expected_outcome = flat.expected_outcome.clone();
                added_nodes.push(node);
            }
            Err(e) => warnings.push(format!("Step {} skipped: {}", i, e)),
        }
    }

    let mut added_edges: Vec<Edge> = added_nodes
        .windows(2)
        .map(|pair| Edge {
            from: pair[0].id,
            to: pair[1].id,
            output: None,
        })
        .collect();

    // Flat plans don't carry explicit Loop→EndLoop ID links — pair them
    // by nesting order, then infer control flow edge labels and back-edges.
    pair_endloop_with_loop(&mut added_nodes, &mut warnings);
    infer_control_flow_edges(&added_nodes, &mut added_edges, &mut warnings);

    PatchResult {
        added_nodes,
        removed_node_ids: Vec::new(),
        updated_nodes: Vec::new(),
        added_edges,
        removed_edges: Vec::new(),
        warnings,
    }
}

/// Build a PatchResult from graph-format planner output (for the assistant empty-workflow path).
pub(crate) fn build_graph_plan_as_patch(
    graph: &PlannerGraphOutput,
    mcp_tools: &[Value],
    allow_ai_transforms: bool,
    allow_agent_steps: bool,
) -> PatchResult {
    let mut id_map = HashMap::new();
    let mut next_id_counters = HashMap::new();
    let positions = parse::layout_nodes(graph.nodes.len());

    let (added_nodes, added_edges, warnings) = build_nodes_and_edges_from_graph(
        &graph.nodes,
        &graph.edges,
        &positions,
        &mut id_map,
        &mut next_id_counters,
        mcp_tools,
        allow_ai_transforms,
        allow_agent_steps,
    );

    PatchResult {
        added_nodes,
        removed_node_ids: Vec::new(),
        updated_nodes: Vec::new(),
        added_edges,
        removed_edges: Vec::new(),
        warnings,
    }
}

/// Shared helper: convert graph-based plan nodes + edges into real Nodes + Edges.
///
/// Each raw `Value` in `raw_nodes` is deserialized individually so that one
/// malformed node (missing fields, unknown shape) doesn't crash the entire parse.
/// Creates nodes from successfully parsed entries, populates `id_map` (LLM ID →
/// real UUID), remaps EndLoop.loop_id references, and builds edges from `plan_edges`.
fn build_nodes_and_edges_from_graph(
    raw_nodes: &[Value],
    raw_edges: &[Value],
    positions: &[Position],
    id_map: &mut HashMap<String, Uuid>,
    next_id_counters: &mut HashMap<String, u32>,
    mcp_tools: &[Value],
    allow_ai_transforms: bool,
    allow_agent_steps: bool,
) -> (Vec<Node>, Vec<Edge>, Vec<String>) {
    let mut warnings = Vec::new();
    let mut nodes = Vec::new();

    // Parse each node individually — skip malformed ones with a warning.
    let (plan_nodes, node_warnings) = parse_lenient::<PlanNode>(raw_nodes);
    warnings.extend(node_warnings);

    // Create nodes and build ID map
    for (i, plan_node) in plan_nodes.iter().enumerate() {
        let pos = positions.get(i).copied().unwrap_or(Position {
            x: 300.0,
            y: 100.0 + (i as f32) * 120.0,
        });
        if let Some(reason) =
            step_rejected_reason(&plan_node.step, allow_ai_transforms, allow_agent_steps)
        {
            warnings.push(format!("Node '{}' removed: {}", plan_node.id, reason));
            continue;
        }
        match step_to_node_type(&plan_node.step, mcp_tools) {
            Ok((node_type, display_name)) => {
                let auto_id =
                    clickweave_core::auto_id::assign_auto_id(&node_type, next_id_counters);
                let mut node = Node::new(node_type, pos, display_name, auto_id);
                if plan_node.role.as_deref() == Some("Verification") {
                    node.role = NodeRole::Verification;
                }
                node.expected_outcome = plan_node.expected_outcome.clone();
                id_map.insert(plan_node.id.clone(), node.id);
                nodes.push(node);
            }
            Err(e) => warnings.push(format!("Node '{}' skipped: {}", plan_node.id, e)),
        }
    }

    // Remap EndLoop.loop_id from LLM IDs to real UUIDs
    for node in &mut nodes {
        if let NodeType::EndLoop(ref mut params) = node.node_type {
            let plan_node = plan_nodes
                .iter()
                .find(|pn| id_map.get(&pn.id) == Some(&node.id));
            if let Some(plan_node) = plan_node
                && let PlanStep::EndLoop { loop_id, .. } = &plan_node.step
            {
                match id_map.get(loop_id) {
                    Some(&real_id) => params.loop_id = real_id,
                    None => warnings.push(format!(
                        "EndLoop '{}' references unknown loop '{}'",
                        plan_node.id, loop_id
                    )),
                }
            }
        }
    }

    // Translate OutputRefs from graph IDs (n1, n2, …) to assigned auto-IDs
    let graph_id_to_auto_id: HashMap<String, String> = plan_nodes
        .iter()
        .filter_map(|pn| {
            let &uuid = id_map.get(&pn.id)?;
            let node = nodes.iter().find(|n| n.id == uuid)?;
            Some((pn.id.clone(), node.auto_id.clone()))
        })
        .collect();
    for node in &mut nodes {
        translate_node_refs(&graph_id_to_auto_id, node);
    }

    // Parse edges leniently — skip malformed ones with a warning.
    let (plan_edges, edge_warnings) = parse_lenient::<PlanEdge>(raw_edges);
    warnings.extend(edge_warnings);

    // Build edges with remapped IDs
    let mut edges = Vec::new();
    for plan_edge in &plan_edges {
        if plan_edge.to == "DONE" {
            continue;
        }
        let from = id_map.get(&plan_edge.from);
        let to = id_map.get(&plan_edge.to);
        match (from, to) {
            (Some(&from_id), Some(&to_id)) => {
                edges.push(Edge {
                    from: from_id,
                    to: to_id,
                    output: plan_edge.output.clone(),
                });
            }
            _ => warnings.push(format!(
                "Edge {}->{} skipped: node not found",
                plan_edge.from, plan_edge.to
            )),
        }
    }

    // Infer missing edge labels for control flow nodes (Loop, If)
    infer_control_flow_edges(&nodes, &mut edges, &mut warnings);

    (nodes, edges, warnings)
}

/// Pair EndLoop nodes with Loop nodes by nesting order (like matching parentheses).
///
/// Flat plans don't carry explicit Loop→EndLoop ID links. Walk through nodes
/// sequentially, push Loop IDs onto a stack, and pop when we see EndLoop.
pub(crate) fn pair_endloop_with_loop(nodes: &mut [Node], warnings: &mut Vec<String>) {
    let mut loop_stack: Vec<Uuid> = Vec::new();
    for node in nodes.iter_mut() {
        match &mut node.node_type {
            NodeType::Loop(_) => {
                loop_stack.push(node.id);
            }
            NodeType::EndLoop(params) if params.loop_id == uuid::Uuid::nil() => {
                match loop_stack.pop() {
                    Some(loop_id) => params.loop_id = loop_id,
                    None => {
                        warnings.push(format!("EndLoop '{}' has no matching Loop node", node.name))
                    }
                }
            }
            _ => {}
        }
    }
    for leftover_id in &loop_stack {
        if let Some(node) = nodes.iter().find(|n| n.id == *leftover_id) {
            warnings.push(format!("Loop '{}' has no matching EndLoop node", node.name));
        }
    }
}

/// Post-process edges to infer control flow labels that LLMs typically omit.
///
/// LLMs generate graph structures that are semantically correct but often miss
/// the `output` labels required for Loop (LoopBody/LoopDone) and If (IfTrue/IfFalse)
/// edges. This pass also fixes common structural issues like back-edges bypassing
/// EndLoop nodes.
pub(crate) fn infer_control_flow_edges(
    nodes: &[Node],
    edges: &mut Vec<Edge>,
    warnings: &mut Vec<String>,
) {
    // Collect EndLoop→Loop pairs: loop_id → endloop_node_id
    let endloop_for_loop: HashMap<Uuid, Uuid> = nodes
        .iter()
        .filter_map(|n| match &n.node_type {
            NodeType::EndLoop(p) => Some((p.loop_id, n.id)),
            _ => None,
        })
        .collect();

    let endloop_ids: HashSet<Uuid> = endloop_for_loop.values().copied().collect();

    // Control-flow node IDs — used in Phase 2 to decide whether to clear
    // stale output labels on rerouted back-edges. Regular execution nodes
    // get their labels cleared; control-flow nodes (If, Switch) keep theirs.
    let cf_node_ids: HashSet<Uuid> = nodes
        .iter()
        .filter(|n| {
            matches!(
                n.node_type,
                NodeType::If(_) | NodeType::Switch(_) | NodeType::Loop(_) | NodeType::EndLoop(_)
            )
        })
        .map(|n| n.id)
        .collect();

    // ── Phase 1: Label Loop outgoing edges ────────────────────────
    for node in nodes {
        if !matches!(node.node_type, NodeType::Loop(_)) {
            continue;
        }
        let loop_id = node.id;
        let endloop_id = endloop_for_loop.get(&loop_id).copied();

        let unlabeled: Vec<usize> = edges
            .iter()
            .enumerate()
            .filter(|(_, e)| e.from == loop_id && e.output.is_none())
            .map(|(i, _)| i)
            .collect();

        match unlabeled.len() {
            2 => {
                let done_is_first = endloop_id.is_some_and(|el_id| edges[unlabeled[0]].to == el_id);
                let done_is_second =
                    endloop_id.is_some_and(|el_id| edges[unlabeled[1]].to == el_id);
                let (body_idx, done_idx) = if done_is_first {
                    (unlabeled[1], unlabeled[0])
                } else {
                    if !done_is_second {
                        // Neither edge targets EndLoop — falling back to edge order
                        warnings.push(format!(
                            "Loop '{}': could not determine LoopBody/LoopDone from structure, using edge order",
                            node.name
                        ));
                    }
                    (unlabeled[0], unlabeled[1])
                };
                edges[body_idx].output = Some(EdgeOutput::LoopBody);
                edges[done_idx].output = Some(EdgeOutput::LoopDone);
            }
            1 => {
                edges[unlabeled[0]].output = Some(EdgeOutput::LoopBody);
            }
            _ => {} // 0 (already labeled) or 3+ (malformed) — leave alone
        }
    }

    // ── Phase 2: Fix EndLoop back-edges ──────────────────────────
    //
    // LLMs often route the last body node directly back to the Loop,
    // bypassing EndLoop. We detect this pattern and reroute through EndLoop.
    for node in nodes {
        let NodeType::EndLoop(params) = &node.node_type else {
            continue;
        };
        let endloop_id = node.id;
        let loop_id = params.loop_id;

        let has_back_edge = edges
            .iter()
            .any(|e| e.from == endloop_id && e.to == loop_id);

        // Always reroute body→Loop edges through EndLoop, even when the
        // back-edge already exists. LLMs often emit both EndLoop→Loop AND
        // direct body→Loop edges; skipping rerouting leaves a cycle.
        let body_start = edges
            .iter()
            .find(|e| e.from == loop_id && e.output == Some(EdgeOutput::LoopBody))
            .map(|e| e.to);

        if let Some(start) = body_start {
            let adj: HashMap<Uuid, Vec<Uuid>> = {
                let mut m: HashMap<Uuid, Vec<Uuid>> = HashMap::new();
                for e in edges.iter() {
                    m.entry(e.from).or_default().push(e.to);
                }
                m
            };

            // Collect body nodes via DFS (stop at Loop and EndLoop boundaries)
            let mut body_set = HashSet::new();
            let mut stack = vec![start];
            while let Some(n) = stack.pop() {
                if n == loop_id || n == endloop_id || !body_set.insert(n) {
                    continue;
                }
                if let Some(nexts) = adj.get(&n) {
                    stack.extend(nexts);
                }
            }

            // Reroute body→Loop edges through EndLoop.
            // Any edge from a body node back to the Loop should go through
            // EndLoop, regardless of its output label (e.g. IfFalse).
            //
            // For regular (non-control-flow) source nodes, also clear the
            // output label — it's stale from the LLM (e.g. LoopBody on a
            // Click→Loop edge) and would prevent follow_single_edge from
            // finding the edge. Control-flow nodes (If, Switch) keep their
            // label because the executor uses follow_edge with that label.
            for edge in edges.iter_mut() {
                if edge.to == loop_id && edge.from != endloop_id && body_set.contains(&edge.from) {
                    edge.to = endloop_id;
                    if !cf_node_ids.contains(&edge.from) {
                        edge.output = None;
                    }
                }
            }
        }

        // Add EndLoop → Loop back-edge if not already present
        if !has_back_edge {
            edges.push(Edge {
                from: endloop_id,
                to: loop_id,
                output: None,
            });
        }

        // Convert EndLoop→NextNode forward-edges to Loop→NextNode (LoopDone).
        // In flat plans, sequential layout creates EndLoop→NextNode edges that
        // should instead be the Loop's "done" path.
        let has_loop_done = edges
            .iter()
            .any(|e| e.from == loop_id && e.output == Some(EdgeOutput::LoopDone));
        if !has_loop_done
            && let Some(idx) = edges
                .iter()
                .position(|e| e.from == endloop_id && e.to != loop_id)
        {
            let target = edges[idx].to;
            edges[idx] = Edge {
                from: loop_id,
                to: target,
                output: Some(EdgeOutput::LoopDone),
            };
        } else if has_loop_done {
            // LoopDone already exists — remove any stray EndLoop forward edges.
            // LLMs sometimes emit both a LoopDone on the Loop node AND a forward
            // edge from EndLoop to a post-loop node. The forward edge is invalid
            // (EndLoop must only point back to its paired Loop).
            edges.retain(|e| !(e.from == endloop_id && e.to != loop_id));
        }
    }

    // ── Phase 3: Remove LoopDone→EndLoop edges ──────────────────
    //
    // If LoopDone targets an EndLoop, following it would loop back
    // (EndLoop→Loop), creating an infinite loop. Remove such edges.
    let before = edges.len();
    edges.retain(|e| !(e.output == Some(EdgeOutput::LoopDone) && endloop_ids.contains(&e.to)));
    if edges.len() < before {
        warnings.push("Removed LoopDone edge targeting EndLoop (would cause infinite loop)".into());
    }

    // ── Phase 4: Label If outgoing edges ─────────────────────────
    for node in nodes {
        if !matches!(node.node_type, NodeType::If(_)) {
            continue;
        }
        let unlabeled: Vec<usize> = edges
            .iter()
            .enumerate()
            .filter(|(_, e)| e.from == node.id && e.output.is_none())
            .map(|(i, _)| i)
            .collect();

        if unlabeled.len() == 2 {
            edges[unlabeled[0]].output = Some(EdgeOutput::IfTrue);
            edges[unlabeled[1]].output = Some(EdgeOutput::IfFalse);
        }
    }
}

/// Build a Workflow from graph-based planner output.
pub(crate) fn build_workflow_from_graph(
    output: &PlannerGraphOutput,
    intent: &str,
    mcp_tools: &[Value],
    allow_ai_transforms: bool,
    allow_agent_steps: bool,
) -> anyhow::Result<PlanResult> {
    let mut id_map = HashMap::new();
    let mut next_id_counters = HashMap::new();
    let positions = layout_nodes(output.nodes.len());

    let (nodes, edges, warnings) = build_nodes_and_edges_from_graph(
        &output.nodes,
        &output.edges,
        &positions,
        &mut id_map,
        &mut next_id_counters,
        mcp_tools,
        allow_ai_transforms,
        allow_agent_steps,
    );

    if nodes.is_empty() {
        return Err(anyhow::anyhow!("No valid nodes produced from graph output"));
    }

    let workflow = Workflow {
        id: Uuid::new_v4(),
        name: parse::truncate_intent(intent),
        nodes,
        edges,
        groups: vec![],
        next_id_counters,
    };

    clickweave_core::validate_workflow(&workflow)
        .map_err(|e| anyhow::anyhow!("Generated workflow failed validation: {}", e))?;

    Ok(PlanResult { workflow, warnings })
}

// ── Auto-ID normalization ────────────────────────────────────────

/// Post-LLM normalization pass: assigns auto-IDs to nodes and translates
/// LLM temporary IDs to auto-IDs in all OutputRefs and conditions.
pub(crate) fn normalize_auto_ids(workflow: &mut Workflow) {
    // Pass 1: Assign auto-IDs, build LLM-temp-ID -> auto-ID mapping
    let mut counters = std::mem::take(&mut workflow.next_id_counters);
    let mut id_map: HashMap<String, String> = HashMap::new();

    for node in &mut workflow.nodes {
        if node.auto_id.is_empty() {
            let auto_id = clickweave_core::auto_id::assign_auto_id(&node.node_type, &mut counters);
            id_map.insert(node.name.clone(), auto_id.clone());
            node.auto_id = auto_id;
        }
    }
    workflow.next_id_counters = counters;

    // Pass 2: Translate OutputRefs in conditions and _ref params
    for node in &mut workflow.nodes {
        translate_node_refs(&id_map, node);
    }
}

fn translate_output_ref(id_map: &HashMap<String, String>, output_ref: &mut OutputRef) {
    if let Some(auto_id) = id_map.get(&output_ref.node) {
        output_ref.node = auto_id.clone();
    }
}

fn translate_optional_ref(id_map: &HashMap<String, String>, opt_ref: &mut Option<OutputRef>) {
    if let Some(r) = opt_ref {
        translate_output_ref(id_map, r);
    }
}

fn translate_condition_value(id_map: &HashMap<String, String>, cv: &mut ConditionValue) {
    if let ConditionValue::Ref(r) = cv {
        translate_output_ref(id_map, r);
    }
}

/// Translate OutputRefs in a single node's parameters from temp IDs to auto-IDs.
fn translate_node_refs(id_map: &HashMap<String, String>, node: &mut Node) {
    match &mut node.node_type {
        NodeType::If(p) => {
            translate_output_ref(id_map, &mut p.condition.left);
            translate_condition_value(id_map, &mut p.condition.right);
        }
        NodeType::Loop(p) => {
            translate_output_ref(id_map, &mut p.exit_condition.left);
            translate_condition_value(id_map, &mut p.exit_condition.right);
        }
        NodeType::Switch(p) => {
            for case in &mut p.cases {
                translate_output_ref(id_map, &mut case.condition.left);
                translate_condition_value(id_map, &mut case.condition.right);
            }
        }
        NodeType::Click(p) => translate_optional_ref(id_map, &mut p.target_ref),
        NodeType::Hover(p) => translate_optional_ref(id_map, &mut p.target_ref),
        NodeType::Drag(p) => {
            translate_optional_ref(id_map, &mut p.from_ref);
            translate_optional_ref(id_map, &mut p.to_ref);
        }
        NodeType::TypeText(p) => translate_optional_ref(id_map, &mut p.text_ref),
        NodeType::FocusWindow(p) => translate_optional_ref(id_map, &mut p.value_ref),
        NodeType::AiStep(p) => translate_optional_ref(id_map, &mut p.prompt_ref),
        NodeType::CdpFill(p) => translate_optional_ref(id_map, &mut p.value_ref),
        NodeType::CdpType(p) => translate_optional_ref(id_map, &mut p.text_ref),
        NodeType::CdpNavigate(p) => translate_optional_ref(id_map, &mut p.url_ref),
        NodeType::CdpNewPage(p) => translate_optional_ref(id_map, &mut p.url_ref),
        _ => {}
    }
}

// ── Re-exports ──────────────────────────────────────────────────

pub use assistant::{AssistantResult, assistant_chat, assistant_chat_with_backend};
pub use conversation_loop::{ConversationOutput, ToolCallRecord, conversation_loop};
pub use patch::{patch_workflow, patch_workflow_with_backend};
pub use plan::{plan_workflow, plan_workflow_with_backend};
pub use tool_use::{PlannerToolExecutor, ToolPermission};
