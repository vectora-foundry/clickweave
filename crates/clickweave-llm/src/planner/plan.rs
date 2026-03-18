use super::mapping::step_to_node_type;
use super::parse::{extract_json, layout_nodes, step_rejected_reason, truncate_intent};
use super::prompt::planner_system_prompt;
use super::repair::chat_with_repair;
use super::{PlanResult, PlanStep, PlannerOutput};
use crate::{ChatBackend, LlmClient, LlmConfig, Message};
use anyhow::{Context, Result, anyhow};
use clickweave_core::{Edge, Node, NodeRole, Workflow, validate_workflow};
use serde::Deserialize;
use serde_json::Value;
use tracing::{debug, info};
use uuid::Uuid;

/// A flat plan step with optional role/expected_outcome metadata.
/// Unlike `PlanNode`, this doesn't require an `id` field (flat plans are sequential).
#[derive(Debug, Deserialize)]
pub(crate) struct FlatPlanStep {
    #[serde(flatten)]
    pub step: PlanStep,
    #[serde(default)]
    pub role: Option<String>,
    #[serde(default)]
    pub expected_outcome: Option<String>,
}

/// Plan a workflow from an intent using the planner LLM.
pub async fn plan_workflow(
    intent: &str,
    planner_config: LlmConfig,
    mcp_tools_openai: &[Value],
    allow_ai_transforms: bool,
    allow_agent_steps: bool,
) -> Result<PlanResult> {
    let planner = LlmClient::new(planner_config);
    plan_workflow_with_backend(
        &planner,
        intent,
        mcp_tools_openai,
        allow_ai_transforms,
        allow_agent_steps,
        None,
    )
    .await
}

/// Plan a workflow using a given ChatBackend (for testability).
/// On parse or validation failure, retries once with the error message appended.
///
/// When `prompt_template` is `Some`, uses that string as the prompt template
/// instead of the compiled-in default.
pub async fn plan_workflow_with_backend(
    backend: &impl ChatBackend,
    intent: &str,
    mcp_tools_openai: &[Value],
    allow_ai_transforms: bool,
    allow_agent_steps: bool,
    prompt_template: Option<&str>,
) -> Result<PlanResult> {
    let system = planner_system_prompt(
        mcp_tools_openai,
        allow_ai_transforms,
        allow_agent_steps,
        prompt_template,
    );
    let user_msg = format!("Plan a workflow for: {}", intent);

    info!("Planning workflow for intent: {}", intent);
    debug!("Planner system prompt length: {} chars", system.len());

    let messages = vec![Message::system(&system), Message::user(&user_msg)];

    chat_with_repair(backend, "Planner", messages, |content| {
        parse_and_build_workflow(
            content,
            intent,
            mcp_tools_openai,
            allow_ai_transforms,
            allow_agent_steps,
        )
    })
    .await
}

/// Parse planner output JSON and build a workflow.
fn parse_and_build_workflow(
    content: &str,
    intent: &str,
    mcp_tools_openai: &[Value],
    allow_ai_transforms: bool,
    allow_agent_steps: bool,
) -> Result<PlanResult> {
    let json_str = extract_json(content);

    // Try graph format first (has "nodes" key)
    if let Ok(graph_output) = serde_json::from_str::<super::PlannerGraphOutput>(json_str)
        && !graph_output.nodes.is_empty()
    {
        return super::build_workflow_from_graph(
            &graph_output,
            intent,
            mcp_tools_openai,
            allow_ai_transforms,
            allow_agent_steps,
        );
    }

    // Fall back to flat steps format
    let mut warnings = Vec::new();

    let planner_output: PlannerOutput =
        serde_json::from_str(json_str).context("Failed to parse planner output as JSON")?;

    if planner_output.steps.is_empty() {
        return Err(anyhow!("Planner returned no steps"));
    }

    // Parse steps leniently — skip malformed ones with warnings
    let (parsed_steps, step_warnings) = super::parse_lenient::<FlatPlanStep>(&planner_output.steps);
    warnings.extend(step_warnings);

    if parsed_steps.is_empty() {
        return Err(anyhow!("No valid steps (all were malformed)"));
    }

    // Filter out rejected steps and collect warnings in a single pass
    let mut steps = Vec::new();
    for flat in &parsed_steps {
        if let Some(reason) =
            step_rejected_reason(&flat.step, allow_ai_transforms, allow_agent_steps)
        {
            warnings.push(format!("Planner step removed: {}", reason));
            continue;
        }
        steps.push(flat);
    }

    if steps.is_empty() {
        return Err(anyhow!(
            "No valid steps after filtering (all were rejected by feature flags)"
        ));
    }

    // Map steps to nodes
    let positions = layout_nodes(steps.len());
    let mut nodes = Vec::new();

    for (i, flat) in steps.iter().enumerate() {
        match step_to_node_type(&flat.step, mcp_tools_openai) {
            Ok((node_type, display_name)) => {
                let mut node = Node::new(node_type, positions[i], display_name);
                if flat.role.as_deref() == Some("Verification") {
                    node.role = NodeRole::Verification;
                }
                node.expected_outcome = flat.expected_outcome.clone();
                nodes.push(node);
            }
            Err(e) => {
                warnings.push(format!("Step {} skipped: {}", i, e));
            }
        }
    }

    if nodes.is_empty() {
        return Err(anyhow!("No valid nodes produced from planner output"));
    }

    // Build linear edges
    let mut edges: Vec<Edge> = nodes
        .windows(2)
        .map(|pair| Edge {
            from: pair[0].id,
            to: pair[1].id,
            output: None,
        })
        .collect();

    // Flat plans don't carry explicit Loop→EndLoop ID links — pair them
    // by nesting order, then infer control flow edge labels and back-edges.
    super::pair_endloop_with_loop(&mut nodes, &mut warnings);
    super::infer_control_flow_edges(&nodes, &mut edges, &mut warnings);

    let workflow = Workflow {
        id: Uuid::new_v4(),
        name: truncate_intent(intent),
        nodes,
        edges,
        groups: vec![],
    };

    // Validate
    validate_workflow(&workflow).context("Generated workflow failed validation")?;

    info!(
        "Planned workflow: {} nodes, {} edges, {} warnings",
        workflow.nodes.len(),
        workflow.edges.len(),
        warnings.len()
    );

    Ok(PlanResult { workflow, warnings })
}
