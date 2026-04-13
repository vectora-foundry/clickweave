# Workflow Execution (Reference)

Verified at commit: `4425c6c`

The engine provides two execution modes: **workflow executor** (deterministic replay of node graphs) and **agent loop** (goal-driven autonomous execution).

## Workflow Executor

The deterministic executor runs a pre-built workflow graph sequentially, dispatching each node to MCP tools.

### Entry Point

Execution starts at Tauri command `run_workflow` (`src-tauri/src/commands/executor.rs`), which creates `WorkflowExecutor` and calls `run()`.

### Execution Flow

1. Emit `StateChanged(Running)`
2. Spawn MCP client subprocess
3. Walk the graph from entry point, executing each node in sequence
4. For each node: resolve target, call MCP tool, record trace events
5. In Test mode: run supervision verification after action nodes
6. Emit `StateChanged(Idle)` when complete or cancelled

### Key Structures

- `WorkflowExecutor` — owns the workflow graph, MCP client, LLM backends, and execution state
- `RetryContext` — per-run transient state (supervision hints, retry tracking, verdicts)
- `DecisionCache` — persisted LLM decisions from Test mode, replayed in Run mode
- `RunStorage` — manages trace event files and artifacts per execution

### State & Contracts

Executor-owned state relevant for CDP and focus bookkeeping:

- `cdp_connected_app: Option<(String, i32)>` — name and PID of the app the CDP session is currently bound to. Comparing both fields (not name only) prevents the CDP connection from silently targeting a different instance of a same-name browser.
- `focused_app: RwLock<Option<(String, AppKind, i32)>>` — last-known focused app with its kind classification and PID. Used by deterministic dispatch to pick the CDP path for Electron/Chrome apps.

`RetryContext` (per-run, transient):

- `completed_node_ids: Vec<(Uuid, String)>` — each entry pairs the node id with its sanitized auto-id prefix, so rollback can also remove any variables the node produced.
- `force_resolve: bool` — skip the persistent decision cache on the next resolution (set after an eviction so retry doesn't replay a stale decision); reset when a node succeeds.
- `focus_dirty: bool` — set when an AI step calls a focus-changing MCP tool (`launch_app`, `focus_window`, `quit_app`); consumed by post-step logic to refresh `focused_app`.

`StepOutcome` (private to `run_loop`) — includes a `Cancelled` variant so a cancellation-token trip during a node is propagated explicitly instead of falling through as a generic failure.

Supervision is **fail-closed**: backend errors during verification are treated as `passed = false`. A broken verifier must not silently pass a bad step.

### Execution Modes

- **Test mode**: Interactive. Runs supervision verification, records decisions to cache, supports retry/skip/abort.
- **Run mode**: Headless replay. Reads cached decisions, skips supervision.

## Agent Loop

The agent loop (`crates/clickweave-engine/src/agent/`) is a goal-driven observe-act loop. It is the primary LLM-driven execution path in Clickweave — the user types a natural-language goal and the agent emits one MCP tool call per step until the goal is reached.

### Entry Point

Tauri command `run_agent` (`src-tauri/src/commands/agent.rs`) creates an `AgentRunner` and starts the loop. The command accepts an `AgentRunRequest { goal, agent, project_path, workflow_name, workflow_id }` where `agent` is the LLM endpoint used for decisions.

### Loop Structure

Implemented in `crates/clickweave-engine/src/agent/loop_runner.rs`:

1. **Observe**: Gather page elements from the current app (via CDP when available; native tools otherwise).
2. **Cache check**: Look up the observation in the per-run decision cache. On a hit the cached tool call is replayed after re-approval.
3. **Decide**: The agent LLM receives the conversation so far — system prompt, goal, prior steps and tool results, plus the full MCP tool list (augmented with `agent_done` and `agent_replan`) — and returns exactly one tool call.
4. **Approve** (optional): If the approval gate is attached, the UI can approve or reject each tool call before dispatch. Observation-only tools (see `OBSERVATION_TOOLS` in `loop_runner.rs` — e.g., `take_screenshot`, `take_ax_snapshot`, `find_text`, `cdp_take_snapshot`) bypass the approval prompt because they do not change state.
5. **Act**: Dispatch the chosen MCP tool and record the result.
6. **Append**: Persist the emitted tool call as a workflow node and an edge from the previous step, so the run materializes as a linear workflow.
7. **Compact**: Run context compaction on the transcript, including snapshot supersession — older AX/DOM snapshots collapse to short placeholders while the most recent snapshot per tool stays at full fidelity (see `crates/clickweave-engine/src/agent/context.rs`). Step summaries are collapsed once the transcript exceeds the token budget.
8. Repeat until `agent_done`, max steps, max consecutive errors, or user cancellation. `agent_replan` does **not** terminate the run — it records the replan reason as the previous tool result and the loop continues, giving the model a chance to re-observe and pick a different action.

### Caching

Decisions are cached in an `AgentCache` keyed by goal + observed element signature. Entries are persisted at `RunStorage::agent_cache_path()` — `agent_cache.json` at the **workflow** level (sibling to per-execution directories, not inside one), so the cache is shared across every run of that workflow and survives individual executions. Future runs against the same app state replay the decision without an LLM round-trip. Approval-gated tools are re-approved on replay. Observation-only tools (e.g., `take_screenshot`, `take_ax_snapshot`) are never cached.

### Events

The loop emits events through an `AgentChannels` mpsc channel, forwarded as Tauri events by `commands/agent.rs`:

- `agent://started` — run started; carries the generation `run_id`
- `agent://step` — tool call completed successfully
- `agent://step_failed` — tool call returned an error
- `agent://node_added` / `agent://edge_added` — workflow persistence
- `agent://approval_required` — approval gate is waiting on the UI
- `agent://cdp_connected` — CDP auto-connect succeeded
- `agent://sub_action` — automatic pre/post-tool hook ran (e.g., auto CDP connect)
- `agent://warning` / `agent://error`
- `agent://complete` — goal achieved; summary in payload
- `agent://stopped` — bounded exit (max_steps, max_errors_reached, approval_unavailable, cancelled)

All payloads carry the `run_id` so stale events from a prior run can be filtered on the UI side.

### Operator Controls

- `stop_agent` — cancels the running loop; sends an explicit rejection through any pending approval so the engine returns `Ok(false)` instead of "approval unavailable".
- `approve_agent_action { approved: bool }` — responds to the current pending approval.

