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
