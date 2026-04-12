# Workflow Execution (Reference)

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

Runtime resolution outcomes (`RuntimeResolution`):

- `Updated(patch)` / `Rewind { patch, first_node_id }` / `Rejected` behave as before.
- `Removed(patch)` applies the patch, then checks whether the current node still exists: if it does, rewind to it; otherwise follow its edges (or the entry point) to continue execution rather than blindly rewinding to the deleted id.

`StepOutcome` (private to `run_loop`) — now includes a `Cancelled` variant so a cancellation-token trip during a node is propagated explicitly instead of falling through as a generic failure.

Supervision is **fail-closed**: backend errors during verification are treated as `passed = false`. A broken verifier must not silently pass a bad step.

### Execution Modes

- **Test mode**: Interactive. Runs supervision verification, records decisions to cache, supports retry/skip/abort.
- **Run mode**: Headless replay. Reads cached decisions, skips supervision.

## Agent Loop

The agent loop (`crates/clickweave-engine/src/agent/`) is a goal-driven observe-act loop that builds workflows dynamically.

### Entry Point

Tauri command `run_agent` (`src-tauri/src/commands/agent.rs`) creates an `AgentRunner` and starts the loop.

### Loop Structure

1. **Observe**: Take screenshot/snapshot of current app state
2. **Plan**: LLM decides what action to take next based on goal and observations
3. **Act**: Execute the planned action via MCP tools
4. **Record**: Add a node to the workflow graph for the executed action
5. **Evaluate**: Check if the goal has been achieved
6. Repeat until goal is met or max steps reached

### Caching

The agent uses a variant index (`VariantIndex`) and action cache to avoid re-running identical actions. Cache keys are derived from app state and action parameters.

### Operator Controls

The operator can `stop_agent` to cancel a running agent, or approve/reject individual actions via `approve_agent_action` when the approval gate is active.
