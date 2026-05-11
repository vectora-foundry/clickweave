# Skill Execution (Reference)

The engine provides two execution paths: the **native skill runner** (deterministic replay of `ActionSketchStep` sequences) and the **agent loop** (goal-driven autonomous execution). Both paths share the MCP client and `RunStorage` for event persistence.

## Native Skill Runner

`clickweave-engine::executor::skill_runner` walks a `&[ActionSketchStep]` slice directly. There is no graph navigation layer. `Loop` is a first-class step kind; the runner recurses into the loop body without any `ProjectionError::UnsupportedLoop` fallback.

### Entry Points

Execution starts at Tauri command `run_skill` (`src-tauri/src/commands/executor.rs`). The command loads the skill, resolves variable bindings, and calls `run_skill_steps`. Failed runs can be resumed section-by-section via `resume_skill_from_failure`.

### Execution Flow

1. Emit `StateChanged(Running)`
2. Spawn MCP client subprocess
3. Walk `&[ActionSketchStep]` from index 0 (or from the resume section):
   - `ActionSketchStep::ToolCall` — resolve target, call MCP tool, record trace events
   - `ActionSketchStep::Loop { predicate, body }` — evaluate `LoopPredicate`, iterate over `body: &[ActionSketchStep]` until the predicate is satisfied
4. For each step: run the approval gate (`should_gate_step`), then dispatch
5. Emit `StateChanged(Idle)` when complete, cancelled, or failed

### Key Structures

- `SkillRunContext` — owns the MCP client reference and the variable `HashMap` for the run
- `SkillRun` (`clickweave-core::skill_run`) — persisted run record; tracks `SectionOutcome` per section
- `RunStorage` — skill-keyed storage: `create_skill_run`, `save_skill_run`, `find_skill_run`, `load_runs_for_skill`, `append_skill_event`
- `DecisionCache` — persisted LLM decisions replayed across runs

### Safety Events

The safety gate in `skill_runner::should_gate_step` checks `requires_approval` on each `ActionSketchStep::ToolCall`. When approval is required, the runner emits an `ApprovalRequired` event carrying `SafetyScope::Skill { skill_id, section_id, step_id }` and waits for operator response before proceeding.

`SafetyScope` (`clickweave-core::safety`) is the discriminant shared by all supervision and approval events:

```rust
pub enum SafetyScope {
    Skill { skill_id: String, section_id: String, step_id: String },
    AdHoc { run_id: Uuid },
}
```

The frontend `useSafetyEventRouter` hook reads the `kind` field and routes accordingly:

- `kind: "skill"` → inline `SkillSectionApprovalOverlay` on the matching `SkillSectionCard`
- `kind: "ad_hoc"` → `AssistantThread`-anchored approval card

Both `SupervisionPaused` / `SupervisionPassed` and `ApprovalRequired` carry this scope. `supervision_respond` and `approve_agent_action` are the corresponding Tauri commands.

### Phase 1 Approval Fallback

The `phase1_static_approvals` Cargo feature gates how `should_gate_step(tool_name, explicit, annotations)` decides whether a step needs operator approval:

- **Feature on:** if `explicit: Option<bool>` is set on the step, that value wins; otherwise fall through to `ToolAnnotations.destructive_hint`, then to the supplemental static `CONFIRMABLE_TOOLS` list.
- **Feature off:** only `explicit.unwrap_or(false)` is checked — the static list and annotation fallback are inactive.

When the feature is disabled, steps without an explicit `requires_approval: true` annotation never trigger the approval gate.

## Four-Layer SkillPatch and Journal Protocol

Skills persist across four layers under `<skill_dir>/<skill_id>/`:

| Layer | File | Purpose |
|-------|------|---------|
| markdown | `SKILL.md` | Human-readable section prose and frontmatter |
| action_sketch | embedded in `SKILL.md` frontmatter | Executable step sequence (`ActionSketchStep[]`) |
| variables | embedded in `SKILL.md` frontmatter | Variable declarations and default bindings |
| replay | `replay.json` | Concrete recorded arguments for replay |

A `SkillPatch` (`clickweave-engine::agent::skills::patch`) is an atomic change request that may touch any combination of these layers. The `SkillPatchPrimitive` discriminant declares the semantic intent:

- `Rebind` — change the target binding of one `action_sketch` step (clears stale signals)
- `Reorder` — reorder both the section prose and its `action_sketch` steps together
- `Promote` — promote a literal argument to a named variable in the `variables` layer and insert a `$ref` in `action_sketch`
- `FreeFormProse` — update narrative prose only

Three named pseudo-tools allow the LLM to author patches during a run: `skill_patch_rebind_target`, `skill_patch_reorder_sections`, `skill_patch_promote_to_variable`. Each is intercepted by `parse_agent_turn` and dispatched to `apply_skill_patch`.

### Journal Protocol

`SkillStore` (`clickweave-engine::agent::skills::store`) writes patches atomically via a crash-safe journal:

1. Stage every new file content under `<skill_id>/.tx/pending/<basename>.new`
2. Write a manifest of the pending files
3. Create `<skill_id>/.tx/commit` via `OpenOptions::create_new` — this is the **single atomic boundary**; if the process dies before this rename, there is no commit
4. Past the `commit` marker: rename each staged file over its live target
5. Remove the `commit` marker (best-effort; a leftover `.tx/` is harmless)

`SkillStore::recover_atomic_writes` runs on `load_all` and is idempotent:

- If `<skill_id>/.tx/commit` exists and a manifest is present: replay the renames (completes a partial commit)
- If pending state exists without the `commit` marker: drop the staged files (rolls back)

Recovery ensures that no partial write is ever visible to skill readers.

## Agent Loop

The agent loop (`crates/clickweave-engine/src/agent/`) is a goal-driven state-centric ReAct loop. The user types a natural-language goal and the agent drives toward it one LLM-authored turn at a time.

The runner is **state-centric**: the harness owns a `WorldModel` (environment facts with per-field freshness) and a `TaskState` (subgoal stack, watch slots, harness-inferred phase). The LLM mutates the task state via typed pseudo-tools batched into the same turn as the chosen MCP action. A rendered `<world_model>` / `<task_state>` block at the top of every user turn keeps the state visible, so the system prompt stays stable and cacheable across runs.

### Entry Point

Tauri command `run_agent` (`src-tauri/src/commands/agent/commands.rs`) dispatches through `run_agent_workflow` (`crates/clickweave-engine/src/agent/mod.rs`) which builds a `StateRunner` and an `AgentTraceGraph` and drives the loop. `AgentRunRequest { goal, agent, project_path }` carries the goal and the LLM endpoint.

### Trace Graph

`AgentTraceGraph` (`clickweave-engine::agent::trace_graph`) accumulates the running trace as an in-memory directed graph of `TraceNode` / `TraceEdge` entries. It is engine-private: no specta derives, never serialized across IPC. `TraceNodeKind` is the engine's renamed successor to the deleted `clickweave-core::NodeType`; it carries the same node-type semantics without the canvas-coupling or specta derives.

### Core Types

The state-spine types live in focused modules under `crates/clickweave-engine/src/agent/`:

- `StateRunner` (`runner/`) — owns the loop state, collaborators, and control flow
- `WorldModel` (`world_model.rs`) — harness-owned environment facts; each field is `Option<Fresh<T>>` tracking freshness, value, written-at, source, and TTL
- `TaskState` (`task_state.rs`) — `{ goal, subgoal_stack, watch_slots, hypotheses, phase, milestones }`
- `Phase` (`phase.rs`) — `{ Exploring, Executing, Recovering }` derived by pure `phase::infer`
- `AgentTurn` (`runner/`) — batched single-pass LLM output: `{ mutations: Vec<TaskStateMutation>, action: AgentAction }`
- `AgentAction` — `ToolCall | InvokeSkill | AgentDone | AgentReplan`
- `TaskStateMutation` — typed pseudo-tools: `PushSubgoal`, `CompleteSubgoal`, `SetWatchSlot`, `ClearWatchSlot`, `RecordHypothesis`, `RefuteHypothesis`
- `StepRecord` / `BoundaryKind` (`step_record.rs`) — boundary snapshots written to `events.jsonl`

### Control Flow

Each step runs, in order (`StateRunner::observe` + `StateRunner::run_turn`):

1. **Observe**: drain `pending_events` into `WorldModel::apply_events`
2. **Phase infer**: run `phase::infer`; write result into `task_state.phase`
3. **Skill retrieval**: refresh applicable skills from `SkillIndex` when a new subgoal is pushed
4. **Render**: build user-turn block — `<world_model>` + `<task_state>`, with `<applicable_skills>` and `<skill_in_progress>` appended when present
5. **Decide**: one LLM call → `AgentTurn`; one repair retry on malformed output
6. **Apply mutations**: walk the batch; invalid mutations become warnings without aborting
7. **Dispatch**: `ToolCall` through the approval gate then `ToolExecutor::call_tool`; `InvokeSkill` expands through the same dispatch path; `AgentDone` triggers VLM completion check; `AgentReplan` records reason and drives next step into `Recovering`
8. **Continuity hooks**: update `WorldModel.last_screenshot` and `WorldModel.last_native_ax_snapshot` from tool body on success
9. **Invalidation**: queue `InvalidationEvent::ToolFailed` on failure; focus-changing and navigation tools queue their own events
10. **Boundary record**: write `StepRecord` per `BoundaryKind` hit — `Terminal`, `SubgoalCompleted`, `RecoverySucceeded`
11. **Compact**: re-render state block each turn; drop snapshot tool-result messages older than current step; keep a recent-N window of assistant/tool pairs verbatim; collapse older pairs

The loop repeats until `AgentDone`, max steps, max consecutive errors, the destructive-tool cap, an approval rejection, or user cancellation.

### Procedural Skills

Skills live as markdown files with YAML frontmatter at `<skill_dir>/<skill_id>/SKILL.md`. `replay.json` lives alongside as a sidecar.

`SkillContext` is the Tauri-to-engine boundary type: `{ enabled, project_skills_dir, global_skills_dir, project_id }`. The runner skips every extraction, retrieval, and replay when `enabled = false`.

Each run builds an in-memory `SkillIndex` from the project-local directory and, when global participation is on, the global tier. Retrieval fires when a mutation batch pushes a new subgoal; the runner renders the top `applicable_skills_k` confirmed/promoted skills into `<applicable_skills>`.

Extraction happens online at `CompleteSubgoal` boundaries. Replay is explicit: the LLM chooses `InvokeSkill { skill_id, version, parameters }`. The replay engine resolves the exact on-disk `(skill_id, version)`, validates parameters, emits `SkillInvoked`, then expands the skill inline through the same live dispatch path used for normal `ToolCall` actions.

The three `SkillPatch` pseudo-tools (`skill_patch_rebind_target`, `skill_patch_reorder_sections`, `skill_patch_promote_to_variable`) are appended to the tool list at run start and intercepted by `parse_agent_turn`; they never reach MCP dispatch.

### Events

The loop emits events through an `AgentChannels` mpsc channel, forwarded as Tauri events by `commands/agent.rs`:

- `agent://started` — run started; carries the generation `run_id`
- `agent://step` — tool call completed successfully
- `agent://step_failed` — tool call returned an error
- `agent://approval_required` — approval gate is waiting on the UI; carries `SafetyScope`
- `agent://cdp_connected` — CDP auto-connect succeeded
- `agent://sub_action` — automatic pre/post-tool hook ran
- `agent://warning` / `agent://error`
- `agent://complete` — goal achieved; summary in payload
- `agent://completion_disagreement` — `agent_done` fired but VLM screenshot check rejected completion
- `agent://completion_disagreement_resolved` — operator decision landed; `{ action: "confirm" | "cancel" }`
- `agent://stopped` — bounded exit (`max_steps_reached`, `max_errors_reached`, `approval_unavailable`, `cancelled`, `user_cancelled_disagreement`, `consecutive_destructive_cap`)
- `agent://task_state_changed` — full `TaskState` snapshot after any turn that applied at least one mutation
- `agent://world_model_changed` — `WorldModelDiff { changed_fields: string[] }` re-render hint, emitted once per step after observe
- `agent://boundary_record_written` — emitted every time a `StepRecord` is persisted; `{ boundary_kind, step_index, milestone_text }`
- `agent://skill_extracted` — `{ run_id, event_run_id, skill_id, version, state, scope }`
- `agent://skill_confirmed` — `{ run_id, event_run_id, skill_id, version }`
- `agent://skill_invoked` — `{ run_id, event_run_id, skill_id, version, parameter_count }`
- Spec 2 episodic events: `agent://episodes_retrieved`, `agent://episode_written`, `agent://episode_promoted`

All payloads carry `run_id` so stale events from a prior run can be filtered on the UI side.

### Operator Controls

- `stop_agent` — cancels the running loop; resolves any pending approval and any pending VLM-disagreement oneshot
- `approve_agent_action { approved: bool }` — responds to the current pending approval
- `resolve_completion_disagreement { action: "confirm" | "cancel" }` — resolves a pending VLM completion disagreement

### Episodic Memory (Spec 2)

The engine maintains a two-tier episodic memory layer (`crates/clickweave-engine/src/agent/episodic/`) so the agent can recall how it recovered from similar stuck states in past runs. Episodic is a **derived view** over `events.jsonl` — it never owns ground truth — and runs entirely best-effort: every failure path is swallowed so an unhealthy SQLite store never tanks an agent run.

`EpisodicContext` is the engine-boundary type: `{ enabled, workflow_local_path, global_path: Option, workflow_hash }`. When `enabled = false`, the runner skips every retrieval and write.

Retrieval triggers: run-start (`step_index == 0`) and `Recovering`-entry phase transitions. Retrieved episodes render as a `<retrieved_recoveries>` block in the user-turn message.

Each scope is a separate SQLite database: `<workflow_dir>/episodic.sqlite` for workflow-local, `<app_data_dir>/episodic.sqlite` for global. Write path is async, fire-and-forget via a bounded mpsc channel.
