# Architecture Overview (Reference)

Clickweave is a Tauri v2 desktop app with a Rust backend and a React frontend.

## Workspace Crates

```
crates/
├── clickweave-core/     # Workflow model, runtime state, storage, tool mapping
├── clickweave-engine/   # Workflow executor + agent loop
├── clickweave-llm/      # LLM client, image prep, chat types
└── clickweave-mcp/      # MCP JSON-RPC client
src-tauri/               # Tauri app shell + IPC commands
ui/                      # React frontend
```

### Dependency Graph

```
clickweave-engine
├── clickweave-core
├── clickweave-llm
│   └── clickweave-core
└── clickweave-mcp

src-tauri
├── clickweave-core
├── clickweave-engine
├── clickweave-llm
└── clickweave-mcp
```

## Crate Responsibilities

### `clickweave-core`

| Module | Purpose |
|--------|---------|
| `workflow.rs` | Core types: `Workflow`, `Node`, `Edge`, `NodeType`, `ExecutionMode`, `NodeRole` |
| `node_params.rs` | Parameter structs for all node types (re-exported via `pub use`) |
| `runtime.rs` | `RuntimeContext` variable store |
| `storage.rs` | `RunStorage` execution/run/event/artifact persistence, `cache_path()` for decision cache |
| `decision_cache.rs` | `DecisionCache` — persists LLM decisions for replay in Run mode |
| `tool_mapping.rs` | `NodeType` <-> MCP tool invocation mapping |
| `cdp.rs` | CDP types: `CdpFindElementsResponse`, `CdpFindElementMatch`, `rand_ephemeral_port()` |
| `app_detection.rs` | App classification (Electron, Chrome, native) from bundle ID / path / PID |
| `walkthrough/` | Walkthrough recording types, event normalization, draft synthesis, session storage |
| `variant_index.rs` | Agent variant index for caching action outcomes |

### `clickweave-engine`

| Module | Purpose |
|--------|---------|
| `executor/mod.rs` | `WorkflowExecutor`, events, caches |
| `executor/run_loop.rs` | Main run loop, retries, supervision wait |
| `executor/graph_nav.rs` | `entry_points`, `follow_single_edge`, `find_predecessor` |
| `executor/variables.rs` | `extract_and_store_variables` — post-execution variable extraction |
| `executor/deterministic/` | Deterministic node execution (`NodeType` -> MCP tool call), CDP connection management |
| `executor/ai_step.rs` | Agentic `AiStep` tool loop |
| `executor/app_resolve.rs` | LLM app-name resolution + cache eviction |
| `executor/element_resolve.rs` | LLM element-name resolution + cache eviction |
| `executor/supervision.rs` | Step verification via VLM + supervision LLM |
| `executor/verdict.rs` | Inline verification verdicts |
| `executor/trace.rs` | Trace events, artifacts, run finalization |
| `agent/` | Agent observe-act loop, prompt building, action cache, transition logic |

See [Workflow Execution](../engine/execution.md).

### `clickweave-llm`

| Module | Purpose |
|--------|---------|
| `client.rs` | OpenAI-compatible chat client, health check, AI-step prompts |
| `types.rs` | `ChatBackend`, message/response/tool-call types |
| `image_prep.rs` | Image resizing for VLM input |

### `clickweave-mcp`

| Module | Purpose |
|--------|---------|
| `client.rs` | `McpClient` subprocess lifecycle + tool calls |
| `protocol.rs` | JSON-RPC and MCP payload types |

See [MCP Integration](../mcp/integration.md).

## Data Flow

### Agent Execution

```
UI
  -> Tauri command: run_agent (goal, endpoint config)
  -> AgentRunner observe-act loop
     - observe: take snapshot/screenshot via MCP
     - plan: LLM decides next action
     - act: execute via MCP tools
     - record: add node to workflow
     - evaluate: check goal completion
  -> emit agent://* events to UI
```

### Workflow Execution

```
UI
  -> Tauri command: run_workflow (with ExecutionMode)
  -> WorkflowExecutor::new() loads DecisionCache
  -> WorkflowExecutor::run()
  -> spawn MCP server for run lifetime
  -> walk graph node-by-node
     - deterministic nodes => MCP tools/call
     - AiStep => LLM + MCP tool loop
     - [Test mode] after each step => supervision verification
       - passed => emit executor://supervision_passed
       - failed => emit executor://supervision_paused, wait for user command
  -> persist DecisionCache (Run mode replays cached decisions)
  -> emit executor://* events to UI
```

## IPC Commands

### Agent Commands
- `run_agent` — start an agent session with a goal
- `stop_agent` — cancel a running agent
- `approve_agent_action` — approve or reject a pending agent action

### Executor Commands
- `run_workflow` — execute a workflow graph
- `stop_workflow` — cancel execution
- `supervision_respond` — respond to supervision pause (retry/skip/abort)

### Project Commands
- `ping`, `get_mcp_status` — health checks
- `open_project`, `save_project` — file I/O
- `validate` — workflow validation
- `node_type_defaults`, `generate_auto_id` — node catalog
- `confirmable_tools`, `check_endpoint` — settings helpers

### Walkthrough Commands
- `start_walkthrough`, `stop_walkthrough`, `pause_walkthrough`, `resume_walkthrough`
- `get_walkthrough_draft`, `apply_walkthrough_annotations`, `seed_walkthrough_cache`
- `detect_cdp_apps`, `validate_app_path`

### Chrome Profile Commands
- `list_chrome_profiles`, `create_chrome_profile`, `is_chrome_profile_configured`
- `get_chrome_profile_path`, `launch_chrome_for_setup`

### Run History
- `list_runs`, `load_run_events`, `read_artifact_base64`
