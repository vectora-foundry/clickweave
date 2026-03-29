use clickweave_core::Workflow;

/// Build the system prompt for runtime resolution queries.
///
/// Teaches the LLM the restricted patch grammar (update/insert/remove)
/// and the constraints on what can be changed at runtime.
pub fn resolution_system_prompt(workflow: &Workflow) -> String {
    let workflow_summary = build_workflow_summary(workflow);

    format!(
        r#"You are a workflow assistant helping fix a runtime resolution failure.

A workflow step failed to find its target element on screen. You have the full planning context from earlier in this conversation. Use it to propose a minimal fix.

## Current Workflow
{workflow_summary}

## Response Format

Return a JSON object with optional fields:
```json
{{
  "reasoning": "Brief explanation of the fix",
  "update": [{{"node_id": "<uuid>", ...changed_fields}}],
  "add_nodes": [{{"name": "...", "tool_name": "...", "arguments": {{}}, "insert_before": "<failing_node_uuid>"}}],
  "remove_node_ids": ["<uuid>", ...]
}}
```

### Update (field corrections on the failing node)
- `name` — node display name
- `tool_name` + `arguments` — change the tool or its parameters. Always provide both `tool_name` and `arguments` together, even if only the arguments change. Examples:
  - Change click target: `"tool_name": "cdp_click", "arguments": {{"target": "OK button"}}` (use the exact label from the element inventory)
  - Change typed text: `"tool_name": "cdp_type_text", "arguments": {{"text": "new text"}}`
  - Change key: `"tool_name": "cdp_press_key", "arguments": {{"key": "Tab"}}`

Available tool names: click, press_key, type_text, move_mouse, focus_window, scroll, find_text, launch_app, cdp_click, cdp_type_text, cdp_press_key, cdp_hover, cdp_fill, cdp_select_page, cdp_navigate, cdp_new_page, cdp_close_page, cdp_wait_for, cdp_handle_dialog

### Insert (new steps before the failing node)
- Use any tool from the Available tool names list above
- Use `insert_before` with the failing node's ID — do NOT emit edges
- NOT allowed: Loop, EndLoop, If, Switch, AiStep

### Remove (redundant steps ahead of the failing node)
- Sequential action nodes that are now redundant
- NOT: control-flow nodes, the failing node, already-completed nodes

## Constraints
- Minimal patch — only changes needed to unblock the current step
- Do NOT add/remove loops, conditionals, or switch branches
- Do NOT restructure control-flow edges
- Do NOT modify already-completed nodes
- Prefer changing the arguments (e.g. target) over changing the tool type
- For `cdp_click` targets, preserve the exact element label from the error context — do not paraphrase or invent labels
"#
    )
}

fn build_workflow_summary(workflow: &Workflow) -> String {
    let mut lines = Vec::new();
    for node in &workflow.nodes {
        lines.push(format!(
            "- [{}] {} ({})",
            node.id,
            node.name,
            node.node_type.display_name()
        ));
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolution_prompt_includes_workflow_nodes() {
        let wf = Workflow::default();
        let prompt = resolution_system_prompt(&wf);
        assert!(prompt.contains("Current Workflow"));
        assert!(prompt.contains("insert_before"));
        assert!(prompt.contains("Minimal patch"));
    }
}
