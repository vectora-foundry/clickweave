# Workflow Execution (Reference)

Verified at commit: `cdabe41`

The engine executes a workflow graph sequentially, evaluating control-flow nodes in place and dispatching execution nodes to MCP tools or an AI-step tool loop.

## Entry Point

Execution starts at Tauri command `run_workflow` (`src-tauri/src/commands/executor.rs`), which creates `WorkflowExecutor` and calls `run()`.

`WorkflowExecutor::new()` takes `agent_config`, `vlm_config`, `supervision_config`, `mcp_configs`, `execution_mode`, `project_path`, `event_tx`, `storage`, and `cancel_token`. Key fields on the executor:

| Field | Type | Purpose |
|-------|------|---------|
| `agent` | `C: ChatBackend` | Primary LLM (small/fast) |
| `vlm` | `Option<C>` | Vision-language model for image analysis |
| `supervision` | `Option<C>` | Planner-class LLM for supervision verification (Test mode) |
| `execution_mode` | `ExecutionMode` | `Test` (interactive supervision, records decisions) or `Run` (replays cached decisions) |
| `decision_cache` | `RwLock<DecisionCache>` | Persisted LLM decisions from Test mode, replayed in Run mode |
| `verdict_vlm` | `Option<LlmClient>` | Dedicated VLM for screenshot verification with low max_tokens and thinking disabled |
| `cdp_connected_app` | `Option<String>` | The app currently connected via CDP (one connection at a time) |
| `cancel_token` | `CancellationToken` | Graceful cancellation signal (replaces the removed `ExecutorCommand::Stop`) |
| `resolution_tx` | `Option<mpsc::Sender<RuntimeQuery>>` | Channel to send resolution queries to Tauri listener (Test mode only) |

Per-run transient state (supervision hints, retry tracking, loop exits, verdicts, completed nodes, rejected resolutions) lives in `RetryContext` (`executor/retry_context.rs`), created fresh for each `run_with_mcp()` call and threaded through execution methods.

High-level flow in `run()`:

1. Emit `StateChanged(Running)`
2. Log agent/VLM model info
3. Spawn MCP server via `McpClient::spawn(path, &[])`
4. `RunStorage::begin_execution()`
5. Find entry points
6. Walk graph (with inline verification for Verification-role nodes and per-step supervision in Test mode)
7. Emit accumulated `runtime_verdicts` via `ChecksCompleted` (if any)
8. Save decision cache (Test mode only)
9. Emit `WorkflowCompleted` when completed normally or when a verification failure stopped the walk
10. Emit `StateChanged(Idle)`

## Graph Walk

Main state machine (in `executor/run_loop.rs`):

1. Cancellation check (`cancel_token.is_cancelled()`)
2. Skip disabled nodes (`follow_disabled_edge`)
3. For control-flow nodes, evaluate branch and jump
4. If a loop just exited (Test mode): run `verify_loop_exit` supervision check
5. For execution nodes, run with retries
6. If node has `role == Verification`: run `evaluate_verification()` inline (fail-fast — a failed verdict breaks the walk immediately)
7. If Test mode and not inside a loop: run `verify_step` supervision check
8. If node execution fails with `ElementResolution`, `ClickTarget`, or `Cdp` error in Test mode: attempt runtime resolution callback (see below)

## Runtime Resolution Callback

When element/target resolution fails in Test mode and a `resolution_tx` channel is available, the executor:

1. Takes a screenshot and builds an element inventory from the error
2. Sends a `RuntimeQuery` to the Tauri resolution listener via the mpsc channel
3. The listener calls `resolution_chat_with_backend` with the planning conversation context
4. If the LLM proposes a patch:
   - **Auto-approve off (default):** emits `executor://resolution_proposed` to the frontend and waits for user approval via `resolution_respond` command
   - **Auto-approve on:** emits `executor://resolution_auto_approved` (observational, for logging/counting) and `executor://patch_applied`, then returns immediately without waiting for user input. The `auto_approve` flag is snapshotted from `RunRequest.auto_approve_resolutions` at run start.
5. Returns one of:
   - `RuntimeResolution::Updated(patch)` — apply patch, cancel current node, re-enter same node
   - `RuntimeResolution::Rewind { patch, first_node_id }` — apply patch, cancel current node, jump to inserted node
   - `RuntimeResolution::Removed(patch)` — apply patch, cancel current node, follow updated edges
   - `RuntimeResolution::Rejected` — record in `rejected_resolutions`, fall through to normal error

Error variants: `ExecutorError::Rewind(Uuid)` propagates through the supervision loop. `RunStatus::Cancelled` and `ExecutorEvent::NodeCancelled` are used when a resolution cancels the current node attempt.
8. On supervision failure: pause and wait for user command (`Resume`, `Skip`, `Abort`)
9. Follow next edge (`follow_single_edge`)

### Executor Commands

| Command | Purpose |
|---------|---------|
| `Resume` | Continue after a supervision pause (re-executes the current node) |
| `Skip` | Skip past a supervision failure and continue to the next node |
| `Abort` | Abort execution after a supervision failure |

### Executor Events

| Event | Payload | Purpose |
|-------|---------|---------|
| `Log(String)` | message | General log message |
| `StateChanged(ExecutorState)` | `Idle` or `Running` | Execution state transition |
| `NodeStarted(Uuid)` | node id | Node execution began |
| `NodeCompleted(Uuid)` | node id | Node execution succeeded |
| `NodeFailed(Uuid, String)` | node id, error | Node execution failed |
| `RunCreated(Uuid, NodeRun)` | node id, run metadata | Node run directory created |
| `WorkflowCompleted` | none | Graph walk completed normally or stopped by a verification failure |
| `ChecksCompleted(Vec<NodeVerdict>)` | verdicts | Inline verification verdicts from Verification-role nodes |
| `Error(String)` | message | Fatal error |
| `SupervisionPassed` | `node_id`, `node_name`, `summary` | Step passed verification (Test mode) |
| `SupervisionPaused` | `node_id`, `node_name`, `finding`, `screenshot?` | Step failed verification, awaiting user action (Test mode) |

### Entry Points

Entry points are nodes with no incoming edges, excluding EndLoop back-edges (so loop cycles do not invalidate start-node detection).

### Edge Helpers

| Method | Purpose |
|--------|---------|
| `follow_single_edge(node_id)` | Regular unlabeled edge |
| `follow_edge(node_id, output)` | Labeled edge (`IfTrue`, `LoopBody`, etc.) |
| `follow_disabled_edge(node_id, node_type)` | Disabled control-flow fallback |

## Control Flow Semantics

### If

Evaluates condition in `RuntimeContext` and takes `IfTrue` or `IfFalse` edge.

### Switch

Evaluates cases in order, takes first matching `SwitchCase(name)`, else `SwitchDefault`.

### Loop

Uses do-while semantics: first visit always takes `LoopBody`.

Iteration logic:

1. If `iteration >= max_iterations`, exit via `LoopDone`
2. Else if `iteration > 0` and exit condition is true, exit via `LoopDone`
3. Else increment counter and continue via `LoopBody`

Loop counters are stored in `RuntimeContext.loop_counters` keyed by Loop node id.

### EndLoop

`EndLoop { loop_id }` jumps directly back to the paired Loop node.

## Node Execution

Non-control-flow nodes run through `execute_node_with_retries()`.

### Deterministic Path

For most nodes:

`NodeType -> node_type_to_tool_invocation() -> mcp.call_tool(name, args)`

Special handling:

- `Click` with `target` and no coordinates: resolve via `find_text` first
- `find_text` auto-scoped to `focused_app` when no explicit `app_name` is set on the node
- `find_text` fallback: if no matches and `available_elements` exists, resolve element name with LLM and retry
- `find_text` element resolution skipped inside loops: `FindText` nodes in loops act as condition checks where accurate found/not-found results are needed for exit conditions; element resolution would mask the fact that the target is not yet on screen
- Click disambiguation: when `find_text` returns multiple matches for a click target, the LLM picks the best match based on text, role, and position (see `disambiguate_click_matches`)
- `FocusWindow` by app name: resolve app to pid via `list_apps` + LLM
- `TakeScreenshot(Window)` with target app name: same app-resolution path
- `launch_app` implicitly sets `focused_app` to the launched app name
- `Click` with `template_image` and no coordinates: resolve via `find_image` using the template
- CDP click path: for apps with `AppKind` that `uses_cdp()`, attempt CDP-based click (snapshot + uid click via `cdp_click`) before falling back to native `find_text`

App resolution and element resolution use `reasoning_backend()` priority: supervision LLM (planner-class) -> VLM -> agent. The small agent model often has insufficient context for these prompts.

### AI Step Path

`AiStep` runs an LLM/tool loop:

1. Build system + user prompts
2. Filter tools by `allowed_tools` if set
3. Repeatedly call LLM
4. Execute returned tool calls via MCP
5. Save tool result images as artifacts
6. If images exist:
   - with vision backend (`vision_backend()`: prefers VLM, falls back to supervision LLM): summarize via `analyze_images()`
   - without vision backend: attach images directly to next LLM turn
7. Stop on no tool calls, timeout, max tool calls, or user stop

## Supervision (Test Mode)

In `ExecutionMode::Test`, each execution node is verified after it completes:

1. Capture a screenshot of the focused app (waits 500ms for UI animations to settle)
2. Ask the vision backend (`vision_backend()`: prefers VLM, falls back to supervision LLM) to describe the current screen state
3. Ask the supervision LLM (with persistent conversation history) whether the step achieved its intended effect
4. If passed: emit `SupervisionPassed` and continue
5. If failed: emit `SupervisionPaused` with finding + screenshot, then wait for user command:
   - `Resume` -> re-execute the node from scratch
   - `Skip` -> accept the result and continue
   - `Abort` -> stop execution

### Loop Supervision

Per-step supervision is skipped for nodes inside loops (detected via non-empty `loop_counters`). Individual steps like clicks, keypresses, and condition checks are verified in aggregate when the loop exits.

After a loop exits (`LoopDone` edge), `verify_loop_exit` runs a deferred visual verification:

1. The loop exit is stored in `pending_loop_exit` by `eval_control_flow`
2. After `eval_control_flow` returns, the main loop consumes `pending_loop_exit`
3. A screenshot is taken and the supervision LLM is asked whether the loop achieved its goal
4. The same `SupervisionPassed`/`SupervisionPaused` flow applies

Read-only nodes (`FindText`, `FindImage`, `TakeScreenshot`, `ListWindows`) skip verification entirely — checked via `node_type.is_read_only()`.

## Retry Behavior

### Node Retries

Each node has `retries` (0-10). On failure before final attempt:

1. Evict relevant caches (`app_cache`, `element_cache`, `focused_app` when applicable)
2. Record `retry` trace event
3. Re-run the node

If retries are exhausted, execution fails and graph walk stops.

### Supervision Retries

Each node has `supervision_retries` (default 2). When VLM supervision detects a failed step:

1. If auto-retries remain: evict caches, set `supervision_hint` with failure reasoning, re-execute
2. The hint is threaded into disambiguation prompts so the LLM picks a different match
3. If auto-retries exhausted: fall through to manual `SupervisionPaused` (Resume/Skip/Abort)
4. The hint is cleared on supervision pass or after exhausting auto-retries

This is orthogonal to node retries (which trigger on tool execution errors). Supervision retries trigger when the tool succeeds but the VLM determines the action didn't have the intended effect.

### Planning/Assistant Retries

See [Planning & LLM Retry Logic](../llm/planning-retries.md).

## Variable Extraction

After each successful execution node, results are written into `RuntimeContext` using sanitized node name prefixes.

Always set:

- `<node>.success = true`
- `<node>.result` (raw parsed result, or empty string for null)

Object result:

- each top-level field -> `<node>.<field>`

Array result:

- `<node>.found` (bool)
- `<node>.count` (number)
- first element fields -> `<node>.<field>`
- typed aliases:
  - `ListWindows` -> `<node>.windows`
  - `FindText` -> `<node>.matches`
  - `FindImage` -> `<node>.matches`

String/number/bool result:

- `<node>.result`

## Runtime Caches

| Cache | Key | Value | Used By |
|-------|-----|-------|---------|
| `app_cache` | user app text | `{name, pid}` | FocusWindow, TakeScreenshot |
| `element_cache` | `(target, app_name?)` | resolved element name | Click, FindText |
| `focused_app` | none | `(app_name, AppKind)` | scoped find-text, resolution, and CDP routing |
| `decision_cache` | varies by type (see below) | `AppResolution`, `ElementResolution`, `ClickDisambiguation` | App resolution, element resolution, click disambiguation |

### Decision Cache

The `DecisionCache` (`clickweave-core/src/decision_cache.rs`) persists LLM decisions made during Test mode so they can be replayed in Run mode without repeating LLM calls. Stored as `decisions.json` alongside the workflow's run directory.

Types stored:

| Type | Key Format | Fields | Purpose |
|------|-----------|--------|---------|
| `AppResolution` | `"node_id\0user_input"` | `user_input`, `resolved_name` | Maps user app text to resolved app name (not PID, since PIDs change between runs) |
| `ElementResolution` | `"node_id\0target\0app_name"` | `target`, `resolved_name` | Maps UI element text to accessibility element name |
| `ClickDisambiguation` | `"node_id\0target\0app_name"` | `target`, `app_name?`, `chosen_text`, `chosen_role` | Records which match was chosen when multiple `find_text` results exist |
| `CdpPort` | `app_name` | `port` | Persists the CDP debugging port for an app between Test and Run modes |

Lifecycle:

1. On `WorkflowExecutor::new()`: load from `storage.cache_path()` (falls back to empty cache)
2. During execution: in-memory cache is checked first, then decision cache, then LLM
3. In Test mode: LLM decisions are recorded into the cache after each resolution
4. After graph walk completes (Test mode only): saved to `storage.cache_path()`

## Run Storage Layout

Saved project path:

```
<project>/.clickweave/runs/<workflow>/<execution_dir>/
```

Unsaved project fallback path:

```
<app_data>/runs/<workflow>_<short_workflow_id>/<execution_dir>/
```

Per-node run directory:

```
<execution_dir>/<sanitized_node_name>/
├── run.json
├── events.jsonl
└── artifacts/
```

Execution-level events are also stored in:

```
<execution_dir>/events.jsonl
```

## Trace Events

Common event types recorded in trace files:

- `node_started`
- `tool_call`
- `tool_result`
- `vision_summary`
- `branch_evaluated`
- `loop_iteration`
- `loop_exited`
- `variable_set`
- `retry`
- `target_resolved`
- `app_resolved`
- `element_resolved`
- `match_disambiguated`
- `cdp_click`
- `cdp_connected`
- `supervision_retry`

## Inline Verification Verdicts

Nodes with `role: Verification` produce verdicts inline during execution (fail-fast). If a verification fails, the walk stops immediately. Verdicts are accumulated in `runtime_verdicts` and emitted as `ChecksCompleted` after the graph walk completes.

Verdict types (in `executor/verdict.rs`):

- `deterministic_verdict()` — for `FindText`, `FindImage`, `ListWindows`: pass if matches found, fail otherwise
- `screenshot_verdict()` — for `TakeScreenshot` with `expected_outcome`: sends screenshot + expected outcome to VLM, parses `{verdict, reasoning}` response
- `missing_outcome_verdict()` — warn verdict for `TakeScreenshot` without `expected_outcome`

See [Verification Nodes](../../verification/node-checks.md).

## Key Files

| File | Role |
|------|------|
| `crates/clickweave-engine/src/executor/mod.rs` | Executor struct and events |
| `crates/clickweave-engine/src/executor/run_loop.rs` | Graph walk, retries, supervision wait |
| `crates/clickweave-engine/src/executor/control_flow.rs` | `eval_control_flow` — If/Switch/Loop/EndLoop |
| `crates/clickweave-engine/src/executor/graph_nav.rs` | Entry points, edge followers |
| `crates/clickweave-engine/src/executor/variables.rs` | Post-execution variable extraction |
| `crates/clickweave-engine/src/executor/deterministic.rs` | Deterministic node execution |
| `crates/clickweave-engine/src/executor/ai_step.rs` | AI-step tool loop |
| `crates/clickweave-engine/src/executor/app_resolve.rs` | App resolution + cache eviction |
| `crates/clickweave-engine/src/executor/element_resolve.rs` | Element resolution, click disambiguation, cache eviction |
| `crates/clickweave-engine/src/executor/supervision.rs` | Per-step and loop-exit verification (Test mode) |
| `crates/clickweave-engine/src/executor/verdict.rs` | Inline verification verdicts |
| `crates/clickweave-core/src/decision_cache.rs` | Decision cache types and save/load |
| `crates/clickweave-core/src/runtime.rs` | Runtime context and condition evaluation |
| `crates/clickweave-engine/src/executor/error.rs` | `ExecutorError` and `ExecutorResult<T>` typed error handling |
| `crates/clickweave-engine/src/executor/trace.rs` | Event emission, logging, image saving, run finalization, cancellation check |
| `crates/clickweave-core/src/storage.rs` | Run/event/artifact persistence |
