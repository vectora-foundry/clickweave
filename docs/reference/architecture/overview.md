# Architecture Overview (Reference)

Clickweave is a Tauri v2 desktop app with a Rust backend and a React frontend.

## Workspace Crates

```
crates/
├── clickweave-core/     # Project manifest, runtime state, storage, safety types
├── clickweave-engine/   # Skill runner, agent loop, trace graph, skill store
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
| `project.rs` | `ProjectManifest` — on-disk project envelope `{ id, name, intent, schema_version }` |
| `skill_run.rs` | `SkillRun`, `SectionOutcome` — skill execution record; run storage is skill-keyed |
| `runtime.rs` | `RuntimeContext` variable store |
| `storage/` | `RunStorage` — skill-keyed execution and event persistence, `cache_path()` for decision cache |
| `decision_cache.rs` | `DecisionCache` — persists LLM decisions for replay |
| `safety.rs` | `SafetyScope` discriminant for supervision and approval events |
| `cdp.rs` | CDP types: `CdpFindElementsResponse`, `CdpFindElementMatch`, `rand_ephemeral_port()` |
| `app_detection.rs` | App classification (Electron, Chrome, native) from bundle ID / path / PID |
| `walkthrough/` | Walkthrough recording types, event normalization, draft synthesis, session storage |
| `variant_index.rs` | Agent variant index for caching action outcomes |

`clickweave-core` does **not** export `Workflow`, `Node`, `Edge`, or `NodeType`. Those canvas-graph types were removed. The agent runner's accumulating trace graph lives in `clickweave-engine` and is engine-private.

### `clickweave-engine`

| Module | Purpose |
|--------|---------|
| `executor/skill_runner.rs` | Native skill runner — index-walk over `&[ActionSketchStep]` with first-class `Loop` |
| `executor/mod.rs` | Executor shared state, MCP lifecycle |
| `agent/trace_graph.rs` | `AgentTraceGraph`, `TraceNode`, `TraceEdge`, `TraceNodeKind` — engine-private accumulating trace (no specta derives, not exposed across IPC) |
| `agent/tool_mapping/` | `TraceNodeKind` ↔ MCP tool invocation mapping (engine-private) |
| `agent/runner/` | `StateRunner` — state-centric ReAct loop (observe / phase-infer / render / decide / dispatch / compact) |
| `agent/skills/` | `SkillStore`, `SkillIndex`, `SkillPatch`, patch application, journal protocol |
| `agent/world_model.rs` | `WorldModel` — harness-owned environment facts with per-field freshness |
| `agent/task_state.rs` | `TaskState` — subgoal stack, watch slots, harness-inferred phase |
| `agent/phase.rs` | `Phase` — `{ Exploring, Executing, Recovering }`, pure `phase::infer` |
| `agent/step_record.rs` | `StepRecord` / `BoundaryKind` — boundary snapshots written to `events.jsonl` |
| `agent/episodic/` | Two-tier episodic memory (workflow-local + global SQLite) |

See [Skill Execution](../engine/execution.md).

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
  -> run_agent_workflow builds a StateRunner + AgentTraceGraph
     - observe: drain pending InvalidationEvents into WorldModel, refresh stale fields
     - phase-infer: derive Phase { Exploring | Executing | Recovering } from signals
     - skill retrieval: refresh applicable skills after push_subgoal mutations
     - render: state block (<world_model> + <task_state> + optional <applicable_skills>) at top of user turn
     - decide: one LLM call -> AgentTurn { mutations, action }
     - apply mutations: TaskStateMutation batch (push/complete subgoal, watch slots, hypotheses)
     - dispatch: AgentAction::ToolCall via MCP, InvokeSkill expansion, or AgentDone / AgentReplan
     - continuity hooks: update WorldModel.last_screenshot / last_native_ax_snapshot
     - invalidation: queue InvalidationEvents for the next observe
     - boundary record: write StepRecord at Terminal / SubgoalCompleted / RecoverySucceeded
     - compact: drop snapshot tool-result messages older than current step
  -> emit agent://* events (including task_state_changed, world_model_changed,
     boundary_record_written) to UI
```

`AgentTraceGraph` (`clickweave-engine::agent::trace_graph`) accumulates `TraceNode` and `TraceEdge` entries as the agent loop runs. It is engine-private: no specta derives, never serialized across IPC. The UI receives structured events over the `agent://*` channel instead.

### Skill Execution

```
UI
  -> Tauri command: run_skill (skill_id, variables)
  -> skill_runner::run_skill_steps walks &[ActionSketchStep]
     - ToolCall steps: resolve target, call MCP tool, record trace events
     - Loop steps: evaluate LoopPredicate, iterate body steps
     - requires_approval gate: check SafetyScope::Skill { skill_id, section_id, step_id }
       - approval needed => emit executor://approval_required, wait for user
       - approved => continue
  -> persist SkillRun per section outcome
  -> emit executor://* events to UI
```

## IPC Commands

### Agent Commands
- `run_agent` — start an agent session with a goal
- `stop_agent` — cancel a running agent
- `approve_agent_action` — approve or reject a pending agent action
- `add_run_to_skill` — promote a completed agent run into a skill
- `save_run_as_skill` — save a run as a new skill draft
- `resolve_completion_disagreement` — resolve a pending VLM completion disagreement

### Executor Commands
- `run_skill` — execute a skill by `skill_id` with optional variable bindings
- `resume_skill_from_failure` — resume a failed skill run from a given section
- `stop_workflow` — cancel execution
- `supervision_respond` — respond to supervision pause (retry/skip/abort)

### Project Commands
- `ping`, `get_mcp_status` — health checks
- `open_project`, `save_project` — file I/O (`ProjectManifest` on disk)
- `pick_workflow_file`, `pick_save_file` — native open/save dialogs
- `import_asset` — pick an image and copy it into the project's `assets/` dir
- `confirmable_tools`, `check_endpoint`, `list_models` — settings helpers

### Skill Commands
- `list_skills_for_panel` — list skills by state bucket (draft/confirmed/promoted)
- `load_skill_full` — load full skill (sections, action_sketch, variables, replay)
- `confirm_skill_proposal` — confirm a draft skill proposal with edits
- `reject_skill_proposal` — reject a draft skill proposal
- `promote_skill_to_global` — move a skill to the global tier
- `fork_skill` — fork a skill into a new editable copy
- `delete_skill` — delete a skill and its associated files
- `apply_skill_patch` — apply a `SkillPatch` (four-layer atomic write via journal protocol)

### Walkthrough Commands
- `start_walkthrough`, `stop_walkthrough`, `pause_walkthrough`, `resume_walkthrough`, `cancel_walkthrough`
- `get_walkthrough_draft`, `apply_walkthrough_annotations`, `seed_walkthrough_cache`
- `save_walkthrough_as_skill` — convert a walkthrough session into a skill draft
- `detect_cdp_apps`, `validate_app_path`

### Chrome Profile Commands
- `list_chrome_profiles`, `create_chrome_profile`, `is_chrome_profile_configured`
- `get_chrome_profile_path`, `launch_chrome_for_setup`

### Run History
- `list_runs` — list runs for a skill; query keyed by `{ skill_id, run_id? }`
- `load_run_events` — load trace events for a skill run; keyed by `{ skill_id, run_id, section_id? }`
- `read_artifact_base64` — read an artifact from a skill run directory

## Safety Events

`SafetyScope` (`clickweave-core::safety`) is the discriminant carried in all supervision and approval events:

```rust
pub enum SafetyScope {
    Skill { skill_id: String, section_id: String, step_id: String },
    AdHoc { run_id: Uuid },
}
```

- `SafetyScope::Skill` — emitted by the skill runner. Carries the exact skill, section, and step position of the pause. The frontend routes these to an inline `SkillSectionApprovalOverlay` on the matching section card.
- `SafetyScope::AdHoc` — emitted by ad-hoc agent runs (no active skill). The frontend routes these to an `AssistantThread`-anchored approval card.

Both `SupervisionPaused` / `SupervisionPassed` and `ApprovalRequired` carry the `SafetyScope` field.
