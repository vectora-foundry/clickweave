use clickweave_core::{NodeType, Workflow, tool_mapping};
use serde_json::Value;

/// Build the planner system prompt.
///
/// When `template_override` is `Some`, uses that string as the template
/// instead of the compiled-in default. Used by the eval tool.
pub(crate) fn planner_system_prompt(
    tools_json: &[Value],
    allow_ai_transforms: bool,
    allow_agent_steps: bool,
    template_override: Option<&str>,
) -> String {
    let tool_list = serde_json::to_string_pretty(tools_json).unwrap_or_default();

    let mut step_types = r#"Available step types:

1. **Tool** — calls exactly one MCP tool:
   ```json
   {"step_type": "Tool", "tool_name": "<name>", "arguments": {...}, "name": "optional label"}
   ```
   The arguments must be valid according to the tool's input schema."#
        .to_string();

    if allow_ai_transforms {
        step_types.push_str(
            r#"

2. **AiTransform** — bounded AI operation (summarize, extract, classify) with no tool access:
   ```json
   {"step_type": "AiTransform", "kind": "summarize|extract|classify", "input_ref": "<step_name>", "output_schema": {...}, "name": "optional label"}
   ```"#,
        );
    }

    if allow_agent_steps {
        step_types.push_str(
            r#"

3. **AiStep** — agentic loop with tool access (use sparingly, only when the task genuinely requires dynamic decision-making):
   ```json
   {"step_type": "AiStep", "prompt": "<what to accomplish>", "allowed_tools": ["tool1", "tool2"], "max_tool_calls": 10, "name": "optional label"}
   ```"#,
        );
    }

    step_types.push_str(r#"

4. **Loop** — repeat a body of steps until an exit condition is met (do-while: body runs at least once). Define the body steps ONCE — the runtime repeats them each iteration, just like a `while` loop in code:
   ```json
   {"id": "<id>", "step_type": "Loop", "exit_condition": <Condition>, "max_iterations": 20, "name": "optional label"}
   ```

5. **EndLoop** — marks the end of a loop body (execution jumps back to the paired Loop node):
   ```json
   {"id": "<id>", "step_type": "EndLoop", "loop_id": "<loop_node_id>", "name": "optional label"}
   ```

6. **If** — conditional branch. MUST have exactly 2 outgoing edges in the graph: one with `{"type": "IfTrue"}` and one with `{"type": "IfFalse"}`. Both branches must connect to downstream nodes (no dangling edges):
   ```json
   {"id": "<id>", "step_type": "If", "condition": <Condition>, "name": "optional label"}
   ```

**Condition** objects compare a runtime variable to a value:
```json
{
  "left": {"type": "Variable", "name": "<sanitized_node_name>.<field>"},
  "operator": "<op>",
  "right": {"type": "Literal", "value": {"type": "Bool", "value": true}}
}
```
Operators: Equals, NotEquals, GreaterThan, LessThan, GreaterThanOrEqual, LessThanOrEqual, Contains, NotContains, IsEmpty, IsNotEmpty.

Literal types: `{"type": "String", "value": "text"}`, `{"type": "Number", "value": 42}`, `{"type": "Bool", "value": true}`.

**Variable names** follow `<sanitized_node_name>.<field>`. The sanitized name is derived from the node's `"name"` field: lowercase the entire name, then replace every non-alphanumeric character (spaces, punctuation, symbols) with `_`. Examples: `"Check result"` → `check_result`, `"Check if result is 128"` → `check_if_result_is_128`, `"Click +"` → `click___`. The variable name in conditions MUST match the exact sanitized form of the referenced node's name. Fields per tool:
- find_text: `.found` (bool), `.text`, `.x`, `.y`, `.count`, `.matches`
- find_image: `.found` (bool), `.x`, `.y`, `.score`, `.count`, `.matches`
- list_windows: `.found` (bool), `.count`, `.windows`
- click, type_text, press_key, scroll, focus_window: `.success` (bool)
- take_screenshot: `.result`
- Any tool: `.result` (raw JSON response)

## Verification role

Any read-only Tool step (find_text, find_image, list_windows, take_screenshot) can be marked as a **verification** by adding `"role": "Verification"` to the node. This makes the node's result count as a test assertion:

- **find_text / find_image / list_windows**: Pass if matches are found, fail otherwise. No LLM call needed.
- **take_screenshot**: Requires `"expected_outcome": "<description>"`. A VLM evaluates whether the screenshot shows the expected result.

Verification failures stop the workflow immediately (fail-fast).

Use `"role": "Verification"` when the user asks to **verify**, **check**, **confirm**, or **assert** a result. Do NOT use it for navigation lookups (e.g., finding a button to click)."#);

    let template = template_override.unwrap_or(include_str!("../../prompts/planner.md"));

    template
        .replace("{{tool_list}}", &tool_list)
        .replace("{{step_types}}", &step_types)
}

/// Build the patcher system prompt.
pub(crate) fn patcher_system_prompt(
    workflow: &Workflow,
    tools_json: &[Value],
    allow_ai_transforms: bool,
    allow_agent_steps: bool,
) -> String {
    let tool_list = serde_json::to_string_pretty(tools_json).unwrap_or_default();

    let nodes_summary: Vec<Value> = workflow
        .nodes
        .iter()
        .map(|n| {
            let mut summary = serde_json::json!({
                "id": n.id.to_string(),
                "name": n.name,
            });
            match tool_mapping::node_type_to_tool_invocation(&n.node_type) {
                Ok(inv) => {
                    summary["tool_name"] = inv.name.into();
                    let mut args = inv.arguments;
                    // Click `target` is internal (not sent to MCP) but the LLM
                    // needs it to know what text the click resolves against.
                    if let NodeType::Click(p) = &n.node_type {
                        if let Some(target) = &p.target {
                            args["target"] = Value::String(target.clone());
                        } else if p.template_image.is_some() {
                            args["has_template_image"] = Value::Bool(true);
                        }
                    }
                    summary["arguments"] = args;
                }
                Err(_) => {
                    // AiStep / AppDebugKitOp — show the raw node_type
                    if let Ok(v) = serde_json::to_value(&n.node_type) {
                        summary["node_type"] = v;
                    }
                }
            }
            summary
        })
        .collect();
    let nodes_json = serde_json::to_string_pretty(&nodes_summary).unwrap_or_default();

    let edges_summary: Vec<Value> = workflow
        .edges
        .iter()
        .map(|e| serde_json::json!({"from": e.from.to_string(), "to": e.to.to_string()}))
        .collect();
    let edges_json = serde_json::to_string_pretty(&edges_summary).unwrap_or_default();

    let mut step_types = String::from("Step types for 'add': same as planning (Tool, ");
    if allow_ai_transforms {
        step_types.push_str("AiTransform, ");
    }
    if allow_agent_steps {
        step_types.push_str("AiStep, ");
    }
    step_types.push_str("see the tool schemas below).");
    step_types.push_str(" For control flow nodes (Loop, EndLoop, If), use \"add_nodes\" + \"add_edges\" instead of \"add\".");

    format!(
        r#"You are a workflow editor for UI automation. Given an existing workflow and a user's modification request, produce a JSON patch.

Current workflow nodes:
{nodes_json}

Current edges:
{edges_json}

Available MCP tools:
{tool_list}

{step_types}

Output ONLY a JSON object with these optional fields:
{{
  "add": [<steps to add, same format as planning>],
  "add_nodes": [<nodes with "id" fields, for control flow>],
  "add_edges": [{{"from": "<id>", "to": "<id>", "output": {{"type": "LoopBody"}}}}],
  "remove_node_ids": ["<id1>", "<id2>"],
  "update": [{{"node_id": "<id>", "name": "new name", "node_type": <step as Tool/AiStep/AiTransform>}}]
}}

Rules:
- **Minimal patch.** Only modify, add, or remove nodes directly affected by the user's request. Leave all unrelated nodes and edges untouched. If the user says "change X", only touch the nodes for X — do not rebuild or remove unrelated parts of the workflow.
- **Identify affected nodes by their arguments, not just names.** When the user refers to a specific part (e.g. "change Alice to Michael"), find nodes whose tool arguments reference "Alice" specifically. Do not touch nodes that reference other entities (e.g. "Bob"), even if they use the same tool types.
- **Use the right operation for each change:**
  - **Replacing** an action with an equivalent one (e.g. "click Alice" → "click Michael"): use "update" to change the node's arguments in-place. This preserves position and edges.
  - **Adding** genuinely new steps (e.g. "also type the result into TextEdit"): use "add" to append new nodes. These are new actions that don't replace anything.
  - **Inserting before** an existing step: remove that step with "remove_node_ids", then include the new steps followed by the removed step in "add". This ensures correct ordering. Example: to insert "type result into TextEdit" before "take screenshot" (id: xyz) → remove xyz, add [launch TextEdit, type result, take screenshot].
  - **Removing** steps the user no longer wants: use "remove_node_ids".
  - Never use "remove+add" to **replace** a node with an equivalent one — use "update" instead. DO use "remove+add" when you need to **reorder** (insert before an existing step).
- Only include fields that have changes (omit empty arrays).
- For "add", use the same step format as planning (step_type: Tool/AiTransform/AiStep). New nodes from "add" are appended after the last existing node.
- For "remove_node_ids", use the exact node IDs from the current workflow.
- For "update", include "node_type" whenever tool arguments need to change (e.g. different search text, click target, key). Changing only the "name" does NOT change what the node actually does at runtime.
- For "add_nodes" + "add_edges", use short IDs (e.g. "n1", "n2") for new nodes. You can reference existing workflow node UUIDs in "add_edges" to connect new nodes to existing ones.
- Keep the workflow functional — don't remove nodes without replacement.
- **Loop structure — think like code.** Setup steps go BEFORE the loop. Only repeating steps go in the body. Verification/cleanup goes AFTER (LoopDone). Example: "multiply by 2 until > 128" → setup: click "2" | body: click "×", click "2", click "=" | after: verify result.

Example — redirect (user says "send to Michael instead of Alice"):
Current nodes: [Launch Signal, Find Alice (id: abc1), Click Alice (id: abc2), Type hello (id: abc3), Send (id: abc4), Find Bob, Click Bob, Type yo, Send to Bob]
User: "Don't send to Alice, send to Michael instead"
Correct patch — use "update" to change Alice nodes in-place:
{{"update": [{{"node_id": "abc1", "name": "Find Michael", "node_type": {{"step_type": "Tool", "tool_name": "find_text", "arguments": {{"text": "Michael"}}}}}}, {{"node_id": "abc2", "name": "Click Michael", "node_type": {{"step_type": "Tool", "tool_name": "click", "arguments": {{"target": "Michael"}}}}}}]}}
This preserves node ordering and edges. Bob's nodes stay untouched.
Wrong: removing Alice nodes + adding Michael nodes at the end (breaks edge ordering).

Example — insert before (user says "before the screenshot, type the result into TextEdit"):
Current nodes: [Launch Calc, Click 5, Click ×, Click 6, Click = , Take screenshot (id: xyz1)]
User: "Before the screenshot, type the result into TextEdit"
Correct patch — remove the screenshot, add new steps then re-add the screenshot at the end:
{{"remove_node_ids": ["xyz1"], "add": [{{"step_type": "Tool", "tool_name": "launch_app", "arguments": {{"app_name": "TextEdit"}}}}, {{"step_type": "Tool", "tool_name": "type_text", "arguments": {{"text": "30"}}}}, {{"step_type": "Tool", "tool_name": "take_screenshot", "arguments": {{}}}}]}}
The removed node is re-added after the new steps, so the final order is: …Click = → Launch TextEdit → Type 30 → Take screenshot.
Wrong: using only "add" without removing the screenshot (appends TextEdit AFTER the screenshot, violating "before")."#,
    )
}

/// Build the unified assistant system prompt.
///
/// Handles both planning (empty workflow) and patching (existing workflow).
pub(crate) fn assistant_system_prompt(
    workflow: &Workflow,
    tools_json: &[Value],
    allow_ai_transforms: bool,
    allow_agent_steps: bool,
    run_context: Option<&str>,
) -> String {
    if workflow.nodes.is_empty() {
        let base = planner_system_prompt(tools_json, allow_ai_transforms, allow_agent_steps, None);
        let mut prompt = format!(
            "You are a conversational workflow assistant for UI automation. \
             You help users create and modify workflows through natural dialogue.\n\n\
             The workflow is currently empty. When the user describes what they want to automate, \
             generate a workflow plan.\n\n{base}"
        );
        if let Some(ctx) = run_context {
            prompt.push_str(&format!("\n\nLatest execution results:\n{ctx}"));
        }
        prompt
    } else {
        let base =
            patcher_system_prompt(workflow, tools_json, allow_ai_transforms, allow_agent_steps);
        let mut prompt = format!(
            "You are a conversational workflow assistant for UI automation. \
             You help users modify their existing workflow through natural dialogue.\n\n\
             When the user asks to modify the workflow, output the JSON patch as specified below. \
             When the user asks a question or makes a comment that doesn't require workflow changes, \
             respond conversationally WITHOUT any JSON output.\n\n{base}"
        );
        if let Some(ctx) = run_context {
            prompt.push_str(&format!("\n\nLatest execution results:\n{ctx}"));
        }
        prompt
    }
}
