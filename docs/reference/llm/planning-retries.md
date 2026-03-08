# Planning & LLM Retry Logic (Reference)

Verified at commit: `d0fd809`

Planner/assistant flows layer retries and parsing tolerance to handle malformed LLM output.

## Retry Layers

| Layer | Scope | Trigger | Limit |
|------|-------|---------|-------|
| JSON repair (`chat_with_repair`) | planner + patcher | parse/build/validation failure in processing closure | 1 retry |
| Assistant validation retry | assistant chat | patch merges to invalid workflow | configurable (`0..10`, default 3) |
| Lenient parsing (`parse_lenient`) | planner + patcher + assistant parsing paths | malformed individual items | skip bad items, keep processing |

## 1. JSON Repair (`planner/repair.rs`)

`chat_with_repair()` wraps an LLM call and retries once with error feedback.

Flow:

1. Call LLM
2. Run caller `process(content)`
3. On failure, append assistant output + corrective user message
4. Call LLM again
5. Re-run processing and return success/failure

Used by:

- `plan_workflow_with_backend()`
- `patch_workflow_with_backend()`

## 2. Assistant Validation Retry (`planner/assistant.rs`)

Assistant path retries only when a patch is produced and merged workflow fails `validate_workflow()`.

Flow:

1. Build messages and call LLM
2. Parse assistant response (conversation/patch/plan)
3. If patch exists and `max_repair_attempts > 0`:
   - build candidate via `merge_patch_into_workflow()`
   - validate candidate
   - if invalid and attempts remain, fire `on_repair_attempt` callback (if provided), append validation error feedback and retry
   - feedback message includes a `Reminder:` paragraph about EndLoop edge wiring rules (must have exactly 1 outgoing edge back to paired Loop, no forward edges)
   - if invalid and exhausted, return patch as-is
4. Return assistant result

`max_repair_attempts` semantics:

- `0`: skip validation
- `1`: validate, no retry
- `N >= 2`: validate + up to `N-1` retries

UI setting is persisted as `maxRepairAttempts` in `settings.json`.

## 3. Lenient Parsing (`planner/mod.rs`)

### `parse_lenient<T>(raw: &[Value])`

- deserializes each item independently
- malformed items are skipped with warnings
- prevents whole response from failing because of one bad entry

### Unknown step handling

`PlanStep` includes `#[serde(other)] Unknown`, allowing unknown `step_type` values to deserialize and later be filtered.

### Feature-flag filtering

`step_rejected_reason()` drops `AiStep`/`AiTransform` based on enabled flags, with warnings.

## Control-Flow Edge Inference

`infer_control_flow_edges()` (in `planner/mod.rs`) repairs common LLM graph issues:

1. Label unlabeled `Loop` edges as `LoopBody`/`LoopDone` (Phase 1)
2. Reroute body-to-loop back edges through `EndLoop` (Phase 2); clears stale `output` labels on rerouted edges from regular (non-control-flow) source nodes so `follow_single_edge` can find them
3. Add missing `EndLoop -> Loop` back-edge
4. Convert `EndLoop -> Next` forward edge into `LoopDone` when needed
5. Remove stray `EndLoop` forward edges when `LoopDone` already exists on the Loop node (LLMs sometimes emit both)
6. Remove `LoopDone -> EndLoop` edges that would create infinite loops (Phase 3)
7. Label unlabeled `If` edges as `IfTrue`/`IfFalse` (Phase 4)

For flat plans, `pair_endloop_with_loop()` pairs EndLoop/Loop by nesting order before inference.

## Prompt Structure

Prompt builders live in `crates/clickweave-llm/src/planner/prompt.rs`. The planner prompt body is loaded from a Markdown template at `crates/clickweave-llm/prompts/planner.md` (compiled in via `include_str!`, overridable at runtime via the `template_override` parameter). The AI-step runtime prompt lives in `crates/clickweave-llm/src/client.rs`.

### Planner Prompt (`planner_system_prompt`)

Signature: `planner_system_prompt(tools_json, allow_ai_transforms, allow_agent_steps, template_override: Option<&str>)`

Composed for `plan_workflow`. The step type catalog and condition/variable reference are built in `prompt.rs`, then substituted into the template (`{{tool_list}}` and `{{step_types}}` placeholders). When `template_override` is `Some`, that string is used instead of the compiled-in `prompts/planner.md` default (used by the eval tool).

Step type catalog (built in `prompt.rs`, conditionally includes AiTransform / AiStep based on feature flags):

```
1. Tool         — single MCP tool call
2. AiTransform  — bounded AI op, no tool access (if allow_ai_transforms)
3. AiStep       — agentic LLM+tool loop (if allow_agent_steps)
4. Loop         — do-while with exit condition; body runs at least once, defined once (runtime repeats)
5. EndLoop      — marks loop body end (execution jumps back to paired Loop)
6. If           — 2-branch conditional; MUST have exactly 2 outgoing edges (IfTrue + IfFalse), both connected
```

The catalog also includes a **Verification role** section: any read-only Tool step (`find_text`, `find_image`, `list_windows`, `take_screenshot`) can be marked `"role": "Verification"` to act as a test assertion. `take_screenshot` additionally requires `"expected_outcome"` for VLM evaluation. Verification failures are fail-fast.

Condition / Variable / Operator reference (also built in `prompt.rs`):
- Variable names follow `<sanitized_node_name>.<field>`: lowercase the name, replace every non-alphanumeric character with `_`
- Examples: `"Check result"` -> `check_result`, `"Click +"` -> `click___`
- Operators: Equals, NotEquals, GreaterThan, LessThan, GreaterThanOrEqual, LessThanOrEqual, Contains, NotContains, IsEmpty, IsNotEmpty
- Literal types: String, Number, Bool

Template structure (`prompts/planner.md`):

```
Role: "You are a workflow planner for UI automation."
  ↓
MCP tool schemas ({{tool_list}} — pretty-printed JSON array from tools/list)
  ↓
{{step_types}} (catalog + condition/variable/operator reference)
  ↓
Output format rules:
  - Simple workflows: {"steps": [...]}
  - Control-flow workflows: {"nodes": [...], "edges": [...]}
  ↓
Graph connectivity rule (exactly one entry point)
  ↓
Loop wiring rules + If wiring rules
  ↓
Conciseness rules (5-8 nodes target, no duplicate actions)
  ↓
Behavioral rules (click with target, launch_app if needed, prefer Tool over AiStep, etc.)
  ↓
Conditional example + Loop example
```

User message: `"Plan a workflow for: <intent>"`

### Patcher Prompt (`patcher_system_prompt`)

Composed for `patch_workflow`. Structure:

```
Role: "You are a workflow editor for UI automation."
  ↓
Current workflow snapshot:
  - Nodes: [{id, name, tool_name, arguments}] (Click nodes include target field; nodes with a template image include has_template_image)
  - Edges: [{from, to}]
  ↓
MCP tool schemas
  ↓
Step types summary (references planning format)
  ↓
Output format: JSON patch object with optional fields:
  - add: [<steps>]
  - add_nodes: [<nodes with id>] (for control flow)
  - add_edges: [{from, to, output}]
  - remove_node_ids: [<ids>]
  - update: [{node_id, name, node_type}]
  ↓
Patch rules (only changed fields, valid IDs, keep flow functional)
  ↓
"Loop structure — think like code" rule (setup BEFORE loop, only repeating steps in body, verification/cleanup AFTER via LoopDone)
```

User message: `"Modify the workflow: <user_prompt>"`

### Assistant Prompt (`assistant_system_prompt`)

Delegates to planner or patcher prompt based on workflow state:

- **Empty workflow** → wraps `planner_system_prompt` with conversational preamble
- **Non-empty workflow** → wraps `patcher_system_prompt` with conversational preamble + instruction to respond conversationally when no changes are needed

Both variants append `run_context` (execution results summary) when available.

Message assembly in `assistant_chat_with_backend`:

```
1. System prompt (planner or patcher variant)
2. Summary context (if available): injected as user + assistant exchange
3. Recent conversation window (last 5 exchanges = 10 messages)
4. New user message
```

### AI-Step Runtime Prompt (`workflow_system_prompt` + `build_step_prompt`)

Used at execution time for `AiStep` nodes. Lives in `client.rs`.

System prompt:
```
Role: "You are a UI automation assistant executing an AI Step node."
  ↓
Available MCP tool descriptions (abbreviated)
  ↓
VLM_IMAGE_SUMMARY format documentation
  ↓
Strategy guidance (screenshot → find → act → verify)
  ↓
Completion signal: JSON `{"step_complete": true, "summary": "..."}` (or `{"step_complete": false, "error": "..."}`)
```

User message built by `build_step_prompt`:
```
<prompt text>
[Button to find: "<button_text>"]     (optional)
[Image to find: <template_path>]      (optional)
```

## Planner Pipeline (`plan_workflow`)

`plan_workflow_with_backend(backend, intent, mcp_tools_openai, allow_ai_transforms, allow_agent_steps, prompt_template: Option<&str>)` — the `prompt_template` parameter is forwarded to `planner_system_prompt` as `template_override`.

1. Build planner prompt (optionally using custom template)
2. LLM call via `chat_with_repair`
3. `extract_json()` (`planner/parse.rs`)
4. Parse graph or flat output
5. `parse_lenient<FlatPlanStep>` + feature filtering (`FlatPlanStep` wraps `PlanStep` with optional `role` and `expected_outcome`)
6. `PlanStep -> NodeType` mapping; propagate `role: "Verification"` → `NodeRole::Verification` and `expected_outcome` to `Node`
7. Edge build + control-flow inference
8. `validate_workflow()`
9. Return workflow + warnings

## Patcher Pipeline (`patch_workflow`)

1. Build patcher prompt
2. LLM call via `chat_with_repair`
3. Parse `PatcherOutput`
4. Build patch via lenient add/update/remove parsing
5. Return patch + warnings

## Assistant Pipeline (`assistant_chat`)

Both `assistant_chat` and `assistant_chat_with_backend` accept an `on_repair_attempt: Option<&(dyn Fn(usize, usize) + Send + Sync)>` callback parameter. It is called with `(attempt, max_repair_attempts)` each time a validation retry is triggered, allowing the caller (e.g. Tauri command layer) to emit progress events.

1. Build conversation messages (summary + recent window + new user message)
2. Call LLM
3. Parse response to patch/plan/conversation
4. If patch and validation enabled: merge + validate + retry loop (fires `on_repair_attempt` callback on each retry)
5. Return assistant text, optional patch, warnings, optional summary update

## Key Files

| File | Role |
|------|------|
| `crates/clickweave-llm/src/planner/prompt.rs` | prompt builders (planner, patcher, assistant); builds step type catalog and substitutes into template |
| `crates/clickweave-llm/prompts/planner.md` | planner prompt template (Markdown with `{{tool_list}}` and `{{step_types}}` placeholders); compiled in via `include_str!`, overridable at runtime |
| `crates/clickweave-llm/src/client.rs` | AI-step runtime prompt (`workflow_system_prompt`, `build_step_prompt`) |
| `crates/clickweave-llm/src/planner/repair.rs` | one-shot repair retry wrapper |
| `crates/clickweave-llm/src/planner/assistant.rs` | assistant retry loop + patch merge validation |
| `crates/clickweave-llm/src/planner/plan.rs` | planner entrypoint and workflow build |
| `crates/clickweave-llm/src/planner/patch.rs` | patcher entrypoint |
| `crates/clickweave-llm/src/planner/mod.rs` | lenient parsing, patch build, control-flow inference |
| `crates/clickweave-llm/src/planner/parse.rs` | JSON extraction and layout helpers |
| `crates/clickweave-core/src/validation.rs` | workflow structural validation |
| `ui/src/store/settings.ts` | settings persistence (`maxRepairAttempts`) |
