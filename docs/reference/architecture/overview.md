# Architecture Overview (Reference)

Verified at commit: `d0fd809`

Clickweave is a Tauri v2 desktop app with a Rust backend and a React frontend.

## Workspace Crates

```
crates/
├── clickweave-core/     # Workflow model, validation, runtime state, storage, tool mapping
├── clickweave-engine/   # Workflow execution engine
├── clickweave-llm/      # LLM client, planning, patching, assistant
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
| `validation.rs` | `validate_workflow()` graph validation |
| `runtime.rs` | `RuntimeContext` variable store + condition evaluation + loop counters |
| `storage.rs` | `RunStorage` execution/run/event/artifact persistence, `cache_path()` for decision cache |
| `decision_cache.rs` | `DecisionCache` — persists LLM decisions (click disambiguation, element/app resolution, CDP port) as `decisions.json` for replay in Run mode |
| `tool_mapping.rs` | `NodeType` ↔ MCP tool invocation mapping |
| `cdp.rs` | Shared CDP (Chrome DevTools Protocol) utilities: server naming, snapshot search |
| `app_detection.rs` | App classification (Electron, Chrome, native) from bundle ID / path / PID |
| `walkthrough/` | Walkthrough recording types, event normalization, draft synthesis, session storage (submodules: `types.rs`, `synthesis.rs`, `storage.rs`) |

### `clickweave-engine`

| Module | Purpose |
|--------|---------|
| `executor/mod.rs` | `WorkflowExecutor`, events, caches |
| `executor/run_loop.rs` | Main run loop, retries, supervision wait |
| `executor/control_flow.rs` | `eval_control_flow` — If/Switch/Loop/EndLoop branch evaluation |
| `executor/graph_nav.rs` | `entry_points`, `follow_single_edge`, `follow_edge`, `follow_disabled_edge` |
| `executor/variables.rs` | `extract_and_store_variables` — post-execution variable extraction |
| `executor/deterministic.rs` | Deterministic node execution (`NodeType` → MCP tool call) |
| `executor/ai_step.rs` | Agentic `AiStep` tool loop |
| `executor/app_resolve.rs` | LLM app-name resolution + cache eviction |
| `executor/element_resolve.rs` | LLM element-name resolution + cache eviction |
| `executor/supervision.rs` | Step verification via VLM + supervision LLM; screenshot capture, description, judge-with-history |
| `executor/verdict.rs` | Inline verification verdicts |
| `executor/trace.rs` | Trace events, artifacts, run finalization |

See [Workflow Execution](../engine/execution.md).

### `clickweave-llm`

| Module | Purpose |
|--------|---------|
| `client.rs` | OpenAI-compatible chat client, AI-step prompts (`workflow_system_prompt`, `build_step_prompt`), VLM analysis (`analyze_images`) |
| `types.rs` | `ChatBackend`, message/response/tool-call types |
| `planner/prompt.rs` | Planner, patcher, and assistant system prompt builders |
| `planner/plan.rs` | `plan_workflow()` |
| `planner/patch.rs` | `patch_workflow()` |
| `planner/assistant.rs` | `assistant_chat()` with patch validation retry |
| `planner/repair.rs` | one-shot repair retry (`chat_with_repair`) |
| `planner/mod.rs` | lenient parsing, patch building, control-flow edge inference |
| `planner/parse.rs` | JSON extraction + layout helpers |
| `planner/mapping.rs` | `PlanStep` → `NodeType` mapping |
| `planner/conversation.rs` | Conversation session windowing |
| `planner/summarize.rs` | Overflow summarization |

See [Planning & LLM Retry Logic](../llm/planning-retries.md).

### `clickweave-mcp`

| Module | Purpose |
|--------|---------|
| `client.rs` | `McpClient` subprocess lifecycle + tool calls |
| `protocol.rs` | JSON-RPC and MCP payload types |

See [MCP Integration](../mcp/integration.md).

## Data Flow

### Planning

```
UI
  -> Tauri command: plan_workflow / patch_workflow / assistant_chat
  -> spawn MCP briefly to fetch tools/list
  -> LLM call (planner/assistant)
  -> parse + infer edges + validate
  -> Workflow/Patch + warnings back to UI
```

### Execution

`RunRequest` carries `workflow`, `agent`, `vlm`, `planner` (supervision LLM), `mcp_command`, and `execution_mode` (`ExecutionMode::Test` or `ExecutionMode::Run`).

```
UI
  -> Tauri command: run_workflow (with ExecutionMode)
  -> WorkflowExecutor::new() loads DecisionCache from decisions.json
  -> WorkflowExecutor::run()
  -> spawn MCP server for run lifetime
  -> walk graph node-by-node
     - deterministic nodes => MCP tools/call
     - AiStep => LLM + MCP tool loop
     - control-flow => evaluate RuntimeContext + follow labeled edge
     - [Test mode] after each step => supervision verification
       - passed => emit executor://supervision_passed
       - failed => emit executor://supervision_paused, wait for user command
       - user sends supervision_respond (retry / skip / abort)
  -> persist DecisionCache to decisions.json (Run mode replays cached decisions)
  -> emit executor://* events to UI
```

## IPC Commands

Commands are registered in `src-tauri/src/main.rs` and implemented under `src-tauri/src/commands/`.

### Commands Directory

```
src-tauri/src/commands/
├── mod.rs                    # Re-exports all public commands and handles
├── types.rs                  # IPC request/response payloads, shared helpers (resolve_storage, project_dir)
├── planner.rs                # plan_workflow, patch_workflow, fetch_mcp_tool_schemas
├── assistant.rs              # assistant_chat, cancel_assistant_chat (AssistantHandle with AbortHandle)
├── executor.rs               # run_workflow, stop_workflow, supervision_respond (ExecutorHandle with cancel token + command channel)
├── project.rs                # open/save/validate, node_type_defaults, import_asset, pick_*_file, conversation I/O, ping
├── runs.rs                   # list_runs, load_run_events, read_artifact_base64
├── walkthrough.rs            # start/pause/resume/stop/cancel_walkthrough, get/apply/seed walkthrough, detect_cdp_apps, validate_app_path
├── walkthrough_session.rs    # WalkthroughHandle, event processing loop, CDP setup, MCP helpers
└── walkthrough_enrichment.rs # VLM click-target resolution and accessibility enrichment
```

### Managed State

Three `Mutex`-wrapped handles are registered as Tauri managed state:

| Handle | State | Purpose |
|--------|-------|---------|
| `ExecutorHandle` | `cancel_token: Option<CancellationToken>`, `cmd_tx: Option<Sender<ExecutorCommand>>`, `task_handle: Option<JoinHandle<()>>` | Cancels the executor task via token (graceful) then abort (forceful); `cmd_tx` sends `Resume`/`Skip`/`Abort` commands |
| `AssistantHandle` | `Option<AbortHandle>` | Cancels in-flight assistant LLM call |
| `WalkthroughHandle` | `session`, `session_dir`, `storage`, `mcp_command`, `event_tap`, `processing_task`, `cancel_tx` | Manages walkthrough recording session lifecycle, event capture, and cancellation |

### Command Summary

| Command | File | Purpose |
|---------|------|---------|
| `plan_workflow` | `planner.rs` | Generate workflow from intent |
| `patch_workflow` | `planner.rs` | Generate workflow patch |
| `assistant_chat` | `assistant.rs` | Conversational assistant + optional patch |
| `cancel_assistant_chat` | `assistant.rs` | Cancel in-flight assistant request |
| `run_workflow` | `executor.rs` | Execute workflow |
| `stop_workflow` | `executor.rs` | Stop active execution |
| `supervision_respond` | `executor.rs` | Send supervision action (`retry`/`skip`/`abort`) to paused executor |
| `validate` | `project.rs` | Validate workflow |
| `open_project` / `save_project` | `project.rs` | Project I/O |
| `save_conversation` / `load_conversation` | `project.rs` | Assistant conversation persistence |
| `pick_workflow_file` / `pick_save_file` | `project.rs` | Native file dialogs |
| `node_type_defaults` | `project.rs` | Return default node configs |
| `import_asset` | `project.rs` | Copy image asset into project |
| `list_runs` / `load_run_events` | `runs.rs` | Run history + trace events |
| `read_artifact_base64` | `runs.rs` | Load artifact contents |
| `start_walkthrough` | `walkthrough.rs` | Begin recording walkthrough session |
| `pause_walkthrough` | `walkthrough.rs` | Pause recording |
| `resume_walkthrough` | `walkthrough.rs` | Resume recording |
| `stop_walkthrough` | `walkthrough.rs` | Stop recording and process events (optional LLM generalization) |
| `cancel_walkthrough` | `walkthrough.rs` | Cancel and discard walkthrough |
| `get_walkthrough_draft` | `walkthrough.rs` | Return draft workflow from recorded walkthrough |
| `apply_walkthrough_annotations` | `walkthrough.rs` | Apply user annotations (renames, deletions, target overrides) to draft |
| `seed_walkthrough_cache` | `walkthrough.rs` | Populate decision cache from walkthrough recording data |
| `detect_cdp_apps` | `walkthrough.rs` | Detect running Electron/Chrome apps for CDP walkthrough |
| `validate_app_path` | `walkthrough.rs` | Validate a user-selected app binary path for CDP support |
| `ping` | `project.rs` | Health check |

## Event Contract

Emitted from `src-tauri/src/commands/executor.rs` and `src-tauri/src/commands/assistant.rs`; consumed in `ui/src/App.tsx`.

| Event | Payload |
|-------|---------|
| `executor://log` | `{ message: string }` |
| `executor://state` | `{ state: "idle" | "running" }` |
| `executor://node_started` | `{ node_id: string }` |
| `executor://node_completed` | `{ node_id: string }` |
| `executor://node_failed` | `{ node_id: string, error: string }` |
| `executor://workflow_completed` | `()` |
| `executor://checks_completed` | `NodeVerdict[]` |
| `executor://supervision_passed` | `{ node_id: string, node_name: string, summary: string }` |
| `executor://supervision_paused` | `{ node_id: string, node_name: string, finding: string, screenshot: string? }` |
| `assistant://repairing` | `[attempt: number, max: number]` |
| `walkthrough://state` | `{ status: WalkthroughStatus }` |
| `walkthrough://event` | `{ event: WalkthroughEvent }` |
| `walkthrough://draft_ready` | `{ actions, draft, warnings, action_node_map }` |
| `walkthrough://cdp-setup` | `CdpSetupProgress` |
| `recording-bar://action` | `{ action: string }` |

Notes:
- `ExecutorEvent::RunCreated` is internal and not emitted to UI.
- `ExecutorEvent::Error` is forwarded as `executor://log`.

## Type Bridge

TypeScript bindings are generated via Specta + tauri-specta:

1. Rust types derive `specta::Type` (enabled by crate features)
2. Tauri commands are registered with `tauri_specta::Builder`
3. In debug builds, bindings are exported to `ui/src/bindings.ts`
4. UI uses typed `commands.*` wrappers and generated TS types

## Key Files

| File | Role |
|------|------|
| `Cargo.toml` | Workspace crates and shared deps |
| `src-tauri/src/main.rs` | Tauri setup, command registration, Specta export |
| `src-tauri/src/commands/mod.rs` | Command exports |
| `src-tauri/src/commands/types.rs` | IPC request/response payloads |
| `ui/src/bindings.ts` | Generated TS commands + types |
| `ui/src/store/useAppStore.ts` | Main composed Zustand store hook |
| `ui/src/App.tsx` | Root wiring and event listeners |
