use clickweave_core::{NodeType, Workflow, chrome_profiles::ChromeProfile, tool_mapping};
use serde_json::Value;

/// Build the "Context Gathering" section for the planner prompt.
///
/// When `has_planning_tools` is true, includes instructions about using
/// planning tools. When false, the section is empty (no context gathering).
pub(crate) fn context_gathering_section(has_planning_tools: bool) -> String {
    if !has_planning_tools {
        return String::new();
    }

    r#"## Context Gathering

Before generating the workflow, call planning tools to understand the target apps.
These tools are for gathering context only — do NOT include them in the workflow JSON.

Available planning tools:
- **probe_app(app_name)** — classify an app as Native, ElectronApp, or ChromeBrowser. **Always call this first** for any app in the user's request.
- **take_ax_snapshot(app_name)** — see visible UI elements (names, roles) for a running native app. Returns interactive elements only, capped at 150 items.
- **cdp_connect(app_name)** — restart an Electron/Chrome app with a debug port and connect CDP. Requires user confirmation (the app will be restarted). After connecting, CDP inspection tools become available.
- **cdp_find_elements(query, role?, max_results?)** — search the CDP-connected page for interactive elements matching a text query. Returns a page element overview (all interactive elements grouped by role) plus a compact hit list (uid, role, label, parent context) for up to 10 matches by default. Only interactive elements (buttons, links, inputs, etc.) are returned. **Available after `cdp_connect`.** Use this instead of `cdp_take_snapshot` — it gives you focused results without flooding context. If your search returns no matches, use the element overview to pick a better query.
- **cdp_take_snapshot** — take a full DOM snapshot of the CDP-connected page. Prefer `cdp_find_elements` for targeted lookups; only use this if you need the full DOM structure.
- **cdp_list_pages** / **cdp_select_page** — list and switch between pages (tabs) in a CDP-connected app.

**CRITICAL — after probe_app returns:**
- If `kind` is **ElectronApp** or **ChromeBrowser**: call `cdp_connect(app_name)` to restart with debug port, then use `cdp_find_elements` to discover UI elements. Use the element names as text targets for `cdp_click` (the executor resolves them at runtime). Generate CDP tool names: `cdp_click`, `cdp_type_text`, `cdp_press_key`, `fill`, `wait_for`, `navigate_page`.
- If `kind` is **Native**: use `take_ax_snapshot` to see UI elements, then generate native tools (`find_text`, `click`, `type_text`, etc.).
- Do NOT use native `click`/`type_text`/`press_key` for Electron/Chrome apps — use `cdp_click`/`cdp_type_text`/`cdp_press_key` instead.
- Do NOT call `take_ax_snapshot` for Electron/Chrome apps — it returns accessibility data, not DOM. Use `cdp_find_elements` after `cdp_connect` instead.

**Recommended sequences:**
- **Native apps:** `probe_app` → `take_ax_snapshot` → generate workflow with native tools
- **Electron/Chrome apps:** `probe_app` → `cdp_connect` (user confirms restart) → `cdp_list_pages` → find the main UI page (skip `background.html`, service workers, devtools pages) → `cdp_select_page` if needed → `cdp_find_elements` to discover elements → generate workflow with CDP tools using text targets from the search results

**Element targeting in workflows:**
- For `cdp_click`: use **text targets** (the element's label text). The executor resolves these to UIDs at runtime from fresh snapshots. Example: `{"target": "Note to Self"}`.
- For `cdp_type_text`: pass **the text to type** — it types into the currently focused element. No target resolution. Example: `{"text": "hello"}`. Click the target input first with `cdp_click`.
- For `cdp_press_key`: pass **the key name** — it sends the keypress to the currently focused element. Example: `{"key": "Enter"}`. Use DOM key names: `Enter` (not `Return`), `Tab`, `Escape`, `ArrowUp`, `ArrowDown`, `Backspace`, `Delete`, or single characters.
- For `fill`: use **UIDs from `cdp_find_elements`**. The `fill` tool requires a literal UID because it targets a specific input field by DOM identity. Example: search with `cdp_find_elements(query: "search", role: "textbox")`, then use `{"uid": "<uid>", "value": "search term"}` in the workflow.
- Do NOT bake UIDs into `cdp_click` arguments — UIDs change between sessions. Always use text targets for click.

**Page selection:** If you called `cdp_select_page` during planning to reach the right page, include the same `cdp_select_page` step in the workflow after `launch_app` so the runtime reaches the same page.

**Important:**
- Electron apps often have a `background.html` page (main process) that contains no UI. Always call `cdp_list_pages` after `cdp_connect` and select the page with the actual application UI before searching.
- The generated workflow must always start with `launch_app` for the target app, even if the app was already started during context gathering. The executor needs `launch_app` to set up the CDP connection at runtime.

Call as many tools as you need, then output the workflow JSON.
For simple tasks on well-known native apps (e.g., Calculator), you may skip probing.

"#.to_string()
}

/// Build the planner system prompt.
///
/// When `template_override` is `Some`, uses that string as the template
/// instead of the compiled-in default. Used by the eval tool.
pub(crate) fn planner_system_prompt(
    tools_json: &[Value],
    allow_ai_transforms: bool,
    allow_agent_steps: bool,
    template_override: Option<&str>,
    chrome_profiles: Option<&[ChromeProfile]>,
    has_planning_tools: bool,
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
  "left": {"node": "<auto_id>", "field": "<field>"},
  "operator": "<op>",
  "right": {"type": "Literal", "value": {"type": "Bool", "value": true}}
}
```
Operators: Equals, NotEquals, GreaterThan, LessThan, GreaterThanOrEqual, LessThanOrEqual, Contains, NotContains, IsEmpty, IsNotEmpty.

Literal types: `{"type": "String", "value": "text"}`, `{"type": "Number", "value": 42}`, `{"type": "Bool", "value": true}`.

**Variable names** follow `<auto_id>.<field>`. The auto_id is assigned automatically from the node type (e.g. `find_text_1`, `click_1`, `find_image_2`). The variable name in conditions MUST use the node's `auto_id`. Fields per tool:
- find_text: `.found` (bool), `.count`, `.text`, `.coordinates` (object with x/y)
- find_image: `.found` (bool), `.count`, `.coordinates` (object with x/y), `.confidence`
- find_app / list_apps: `.found` (bool), `.name`, `.pid`
- click, type_text, press_key, scroll, focus_window: `.success` (bool)
- take_screenshot: `.result`
- Any tool: `.result` (raw JSON response)

## Verification role

Any read-only Tool step (find_text, find_image, list_apps, take_screenshot) can be marked as a **verification** by adding `"role": "Verification"` to the node. This makes the node's result count as a test assertion:

- **find_text / find_image / find_app**: Pass if matches are found, fail otherwise. No LLM call needed.
- **take_screenshot**: Requires `"expected_outcome": "<description>"`. A VLM evaluates whether the screenshot shows the expected result.

Verification failures stop the workflow immediately (fail-fast).

Use `"role": "Verification"` when the user asks to **verify**, **check**, **confirm**, or **assert** a result. Do NOT use it for navigation lookups (e.g., finding a button to click)."#);

    let chrome_profiles_section = match chrome_profiles {
        Some(profiles) if profiles.len() > 1 => {
            let mut lines = String::from(
                "\n## Chrome profiles\n\nAvailable Chrome profiles for browser sessions:\n",
            );
            for p in profiles {
                match &p.google_email {
                    Some(email) => {
                        lines.push_str(&format!("- \"{}\" (signed in as {})\n", p.name, email))
                    }
                    None => lines.push_str(&format!("- \"{}\"\n", p.name)),
                }
            }
            lines.push_str(
                "\nWhen the user specifies which Chrome profile to use, add `\"chrome_profile\": \"<name>\"` to the launch_app arguments alongside `\"app_kind\": \"ChromeBrowser\"`. Use the exact profile name shown in quotes above (do NOT include the email). Only include this when explicitly requested.",
            );
            lines
        }
        _ => String::new(),
    };

    let template = template_override.unwrap_or(include_str!("../../prompts/planner.md"));

    let context_gathering = context_gathering_section(has_planning_tools);

    template
        .replace("{{context_gathering}}", &context_gathering)
        .replace("{{tool_list}}", &tool_list)
        .replace("{{step_types}}", &step_types)
        .replace("{{chrome_profiles}}", &chrome_profiles_section)
}

/// Build the patcher system prompt.
pub(crate) fn patcher_system_prompt(
    workflow: &Workflow,
    tools_json: &[Value],
    allow_ai_transforms: bool,
    allow_agent_steps: bool,
    has_planning_tools: bool,
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
                            args["target"] = Value::String(target.text().to_string());
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

    let context_gathering = context_gathering_section(has_planning_tools);

    format!(
        r#"You are a workflow editor for UI automation. Given an existing workflow and a user's modification request, produce a JSON patch.

{context_gathering}Current workflow nodes:
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
    chrome_profiles: Option<&[ChromeProfile]>,
    has_planning_tools: bool,
) -> String {
    use super::tool_use::is_planning_only_tool;

    // When planning tools are available, filter them out of the workflow catalog
    // so the LLM prompt only shows tools valid as workflow nodes.
    let workflow_tools: Vec<Value> = if has_planning_tools {
        tools_json
            .iter()
            .filter(|tool| {
                let name = tool
                    .get("function")
                    .and_then(|f| f.get("name"))
                    .and_then(|n| n.as_str())
                    .unwrap_or("");
                !is_planning_only_tool(name)
            })
            .cloned()
            .collect()
    } else {
        tools_json.to_vec()
    };

    if workflow.nodes.is_empty() {
        let base = planner_system_prompt(
            &workflow_tools,
            allow_ai_transforms,
            allow_agent_steps,
            None,
            chrome_profiles,
            has_planning_tools,
        );
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
        let base = patcher_system_prompt(
            workflow,
            &workflow_tools,
            allow_ai_transforms,
            allow_agent_steps,
            has_planning_tools,
        );
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
