# Workflow Execution (Reference)

Verified at commit: `64f9cc2`

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

The agent loop (`crates/clickweave-engine/src/agent/`) is a goal-driven state-centric ReAct loop. It is the primary LLM-driven execution path in Clickweave — the user types a natural-language goal and the agent drives toward it one LLM-authored turn at a time.

The runner is **state-centric** rather than transcript-centric: the harness owns a `WorldModel` (environment facts with per-field freshness) and a `TaskState` (subgoal stack, watch slots, harness-inferred phase). The LLM mutates the task state via typed pseudo-tools batched into the same turn as the chosen MCP action. A rendered `<world_model>` / `<task_state>` block at the top of every user turn keeps the state visible, so the system prompt stays stable and cacheable across runs.

This loop is the implementation of **Spec 1 of 3** of the agent redesign. The authoritative design (locked decisions D1..D19) and the broader three-spec roadmap live in the private Clickweave vault — see "Agent State Spine" and the "Stateful Task Controller Vision" design docs.

### Entry Point

Tauri command `run_agent` (`src-tauri/src/commands/agent.rs`) dispatches through `run_agent_workflow` (`crates/clickweave-engine/src/agent/mod.rs`) which builds a `StateRunner` and drives it. `AgentRunRequest { goal, agent, project_path, workflow_name, workflow_id }` carries the goal and the LLM endpoint used for decisions.

### Core types

The state-spine types live in focused modules under `crates/clickweave-engine/src/agent/`:

- `StateRunner` (`runner.rs`) — owns the loop state, collaborators, and control flow.
- `WorldModel` (`world_model.rs`) — harness-owned environment facts. Each field is `Option<Fresh<T>>`, where `Fresh<T> { value, written_at, source, ttl_steps }` tracks freshness. Fields: `focused_app`, `window_list`, `cdp_page`, `elements: Vec<ObservedElement>` (tagged union over CDP / AX / OCR sources, D16), `modal_present`, `dialog_present`, `last_screenshot` (small ref: `{ screenshot_id, captured_at_step }`), `last_native_ax_snapshot` (full body: `{ snapshot_id, element_count, captured_at_step, ax_tree_text }` — native `take_ax_snapshot` only, per D15), `uncertainty: UncertaintyScore` (harness-computed, D14).
- `TaskState` (`task_state.rs`) — `{ goal, subgoal_stack: Vec<Subgoal>, watch_slots: Vec<WatchSlot>, hypotheses, phase, milestones }`. The stack is flat (D4); watch-slot names are a fixed enum `{ PendingModal, PendingAuth, PendingFocusShift }` (D13).
- `Phase` (`phase.rs`) — `{ Exploring, Executing, Recovering }`. Derived from `PhaseSignals { stack_depth, consecutive_errors, last_replan_step, current_step }` via the pure `phase::infer` function. Precedence: `Recovering > Executing > Exploring`. Harness-inferred; the LLM never authors it (D5).
- `AgentTurn` (`runner.rs`) — the batched single-pass LLM output: `{ mutations: Vec<TaskStateMutation>, action: AgentAction }`. Mutations apply in order before the action dispatches (D7).
- `AgentAction` — `ToolCall { tool_name, arguments, tool_call_id } | InvokeSkill { skill_id, version, parameters } | AgentDone { summary } | AgentReplan { reason }`. `get_current_datetime` is a harness-local observation pseudo-tool represented as `ToolCall` and intercepted before MCP dispatch.
- `TaskStateMutation` — the typed pseudo-tools: `PushSubgoal`, `CompleteSubgoal`, `SetWatchSlot`, `ClearWatchSlot`, `RecordHypothesis`, `RefuteHypothesis` (D10). Never dispatched to MCP.
- `StepRecord` / `BoundaryKind` (`step_record.rs`) — boundary snapshots written to `events.jsonl` at terminal events, `CompleteSubgoal` mutations, and `Recovering → Executing` transitions (D8). Feeder for Spec 2's episodic memory layer.

### Control flow

The body of a step is, in order (`StateRunner::observe` + `StateRunner::run_turn` in `runner.rs`):

1. **Observe**: drain `pending_events` into `WorldModel::apply_events` so focus-changing / navigation / tool-failure events invalidate the right fields.
2. **Phase infer**: run `phase::infer` on the current signals; write the result into `task_state.phase`.
3. **Skill retrieval**: when the mutation batch pushes a new subgoal and skills are enabled for the run, the runner refreshes applicable procedural skills from the in-memory `SkillIndex` for the next rendered turn.
4. **Render**: `render::render_step_input(&world_model, &task_state, step_index, retrieved_skills, skill_frame)` builds the user-turn block — `<world_model>` + `<task_state>` at the top, above the observation (D6), with `<applicable_skills>` and `<skill_in_progress>` appended when present. The system prompt (`messages[0]`) stays stable across runs; the goal block, `prior_turns`, and `VariantIndex::as_context_text()` are inlined into `messages[1]` per D18 so the system prefix remains cacheable.
5. **Decide**: one LLM call returns an `AgentTurn` — 0..N `TaskStateMutation`s plus exactly one `AgentAction` (D7). A malformed `AgentTurn` gets one repair retry; a second failure counts as a step error.
6. **Apply mutations**: `StateRunner::apply_mutations` walks the batch in order. Invalid mutations (stack underflow, unknown watch slot, refute out of range) become warnings — subsequent mutations and the action still run (error-path table in the design doc).
7. **Dispatch**: run the action. `ToolCall` goes through the approval gate (observation-only tools and `Allow` permission policies bypass the prompt) and then `ToolExecutor::call_tool`. `InvokeSkill` expands a confirmed/promoted skill through the same dispatch helper and falls back to the LLM path on divergence. `AgentDone` triggers the VLM completion check when a vision backend is attached; `AgentReplan` sets `last_replan_step` so the next phase-infer returns `Recovering`.
8. **Continuity hooks**: on `ToolSuccess` the runner updates `WorldModel.last_screenshot` and `WorldModel.last_native_ax_snapshot` from the tool body. AX dispatch targets are rewritten through `StateRunner::enrich_ax_descriptor`, which reads the AX tree body directly from `WorldModel.last_native_ax_snapshot` — no transcript walking (D15).
9. **Invalidation**: on failure, queue an `InvalidationEvent::ToolFailed`; focus-changing and navigation tools queue their own events for the next observe.
10. **Boundary record + skill extraction**: `maybe_record_step_snapshot` writes one `StepRecord` per `BoundaryKind` hit in the step — `Terminal`, `SubgoalCompleted` (one per `CompleteSubgoal` mutation that appended a milestone), and `RecoverySucceeded` at the exact `Recovering → Executing` transition (D8). `SubgoalCompleted` boundaries also feed the procedural-skill extractor when `SkillContext` is enabled.
11. **Compact**: `context::compact` runs on the transcript. The state block is re-rendered each turn, so snapshot tool-result messages older than the current step are dropped entirely — continuity information lives in `WorldModel` (D12). `messages[0]` and `messages[1]` are never compacted; a recent-N window of assistant/tool pairs is kept verbatim; older pairs collapse to `{ step_index, action.kind, tool_or_kind, outcome.kind, brief }`.

The loop repeats until `AgentDone`, max steps, max consecutive errors, the destructive-tool cap, an approval rejection, or user cancellation. `AgentReplan` does **not** terminate — it records the reason and drives the next step into `Recovering`.

### Procedural Skills

Procedural skills live as markdown files with YAML frontmatter under `RunStorage::project_skills_dir()` for project-local memory, with an opt-in global tier for promoted skills. Saved projects use `<project>/.clickweave/skills/`; unsaved projects use the app-data skills directory keyed by workflow id and move into the project directory on first save.

`SkillContext` is the Tauri-to-engine boundary type. It carries `{ enabled, project_skills_dir, global_skills_dir, project_id }`. The Tauri command enables it only when `store_traces`, `skills_enabled`, and the per-run storage setup allow on-disk skill writes. `SkillContext::disabled()` is the no-op shape used when the privacy kill switch or settings disable the layer.

Each run builds an in-memory `SkillIndex` from the project-local directory and, when global participation is on, the global tier. Retrieval fires when a mutation batch pushes a new subgoal; the runner looks up the top `applicable_skills_k` confirmed/promoted skills and renders them into the next user turn's `<applicable_skills>` block. User-controlled text in that block is passed through `escape_capped`, so skill names, descriptions, parameter names, and app hints cannot inject XML-like tags or unbounded content into the prompt.

Extraction happens online at `CompleteSubgoal` boundaries. The runner keeps an in-memory `RecordedStep` slice for the active run; when a subgoal completes, the extractor folds the relevant steps into a draft skill, preserving `produced_node_ids` lineage so node deletion and Clear-conversation can prune only draft skills derived from agent-created nodes.

Replay is explicit: the LLM chooses `InvokeSkill { skill_id, version, parameters }` from the rendered applicable-skills block. The replay engine resolves the exact on-disk `(skill_id, version)`, validates parameters, emits `SkillInvoked`, then expands the skill inline through the same live dispatch helper used for normal `ToolCall` actions. Permission policy, operator approval, coordinate guards, destructive caps, and telemetry therefore stay on the same path. If required captures cannot be resolved or the expected world-model delta diverges, the skill frame is suspended and the next LLM turn receives `<skill_in_progress>` context for fallback.

Three Tauri events expose this layer to the UI:

- `agent://skill_extracted` — `{ run_id, event_run_id, skill_id, version, state, scope }`
- `agent://skill_confirmed` — `{ run_id, event_run_id, skill_id, version }`
- `agent://skill_invoked` — `{ run_id, event_run_id, skill_id, version, parameter_count }`

`agent://boundary_record_written` also includes `milestone_text` for the live trace surface.

### Conversational continuation

Each `run_agent` call carries an optional `anchor_node_id` and a `prior_turns` log. The runner seeds `last_node_id` from the anchor so the first emitted edge links into the existing workflow chain (Extend mode). Prior-turn log and `VariantIndex::as_context_text()` are both composed into the goal string and land in `messages[1]` (D18) — this is a deliberate move from the earlier convention of appending variant context to `messages[0]`; keeping the system prefix stable preserves prompt-cache hit rate across runs. Every node the runner produces is stamped with `source_run_id` so selective-delete and Clear-conversation can scope operations to agent-built nodes only.

### Agent UI Projection

`AgentState.steps: Vec<AgentStep>` and `AgentCommand` remain the current frontend projection while Spec 3 surfaces are landing. `StateRunner` writes `AgentStep` records alongside its native `StepRecord` / `AgentTurn` representations so existing UI panels can render the step timeline until the live trace surface owns that display path. This is an implementation bridge, not a backward-compatibility contract. Harness-local observations such as `get_current_datetime` are recorded as tool-call steps but remain observation-only and never become workflow graph nodes.

### Tool Exposure

The tool list passed to the LLM is stable across the lifetime of a run. All tools — including CDP tools (`cdp_click`, `cdp_find_elements`, `cdp_type_text`, etc.) — are exposed up-front regardless of whether a CDP connection has been established yet. Tools that require a connection return a clean "not connected" error when called pre-connection, and the agent recovers by picking a different action on the next step.

**Rationale.** Mid-conversation changes to the tool list invalidate every prior prompt-cache prefix. Exposing the superset up-front trades an occasional wasted tool-call turn for a stable prompt prefix and higher cache hit rates across the run. This matches how modern agent runtimes handle tool surfaces and pairs with D6 / D18 — the system prompt and the user-turn state block are both designed to keep the cacheable prefix stable.

**Implications for contributors.** Do not add code paths that mutate the tool list mid-run. New MCP-backed tools should be exposed at run start via `mcp.tools_as_openai()` in `agent/mod.rs`. Harness-local tools that must not depend on the external MCP server should be added as stable pseudo-tools and intercepted by the runner. If a new capability genuinely needs runtime activation, prefer a guard inside the tool handler over refreshing the list. The typed mutation pseudo-tools (`push_subgoal`, `complete_subgoal`, `set_watch_slot`, `clear_watch_slot`, `record_hypothesis`, `refute_hypothesis`) live in the `AgentTurn` output schema rather than the MCP tool list; action pseudo-tools such as `get_current_datetime`, `agent_done`, `agent_replan`, and `invoke_skill` are appended once at run start, preserving the stable-tool-surface invariant (D10).

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
- `agent://completion_disagreement` — `agent_done` fired but a post-run VLM screenshot check rejected the completion; payload carries the screenshot, VLM reasoning, and the agent's own summary so the UI can surface the disagreement for operator adjudication. The Tauri task holds the run open on a per-run oneshot (`AgentHandle::pending_disagreement_tx`) until the operator resolves the disagreement via `resolve_completion_disagreement` (or the Stop button, which `force_stop`s the oneshot with `Cancel`). The resolution is persisted to `events.jsonl` and `variant_index.jsonl`, then the task emits the definitive terminal event below.
- `agent://completion_disagreement_resolved` — ancillary event emitted after the operator's decision lands; payload `{ run_id, action: "confirm" | "cancel" }`. Logs-drawer-and-telemetry grade; not the definitive terminal event.
- `agent://stopped` — bounded exit (`max_steps_reached`, `max_errors_reached`, `approval_unavailable`, `cancelled`, `user_cancelled_disagreement`, `consecutive_destructive_cap`). The `user_cancelled_disagreement` variant is the terminal emission for the Cancel path of a VLM disagreement. The confirm path emits `agent://complete` instead.
- `agent://task_state_changed` — emitted after `apply_mutations` applies at least one mutation during a turn. Payload: `{ run_id, task_state }` (full snapshot — subgoal stack, watch slots, phase, milestones, hypotheses).
- `agent://world_model_changed` — emitted once per step after `observe` runs. Payload: `{ run_id, diff: WorldModelDiff }` where `WorldModelDiff.changed_fields` lists the `WorldModel` field names whose freshness-wrapped value changed during that step's observe phase (stable names: `focused_app`, `window_list`, `cdp_page`, `elements`, `modal_present`, `dialog_present`, `last_screenshot`, `last_native_ax_snapshot`, `uncertainty`).
- `agent://boundary_record_written` — emitted every time a boundary `StepRecord` is persisted to `events.jsonl`. Payload: `{ run_id, boundary_kind, step_index, milestone_text }` where `boundary_kind` is `"terminal" | "subgoal_completed" | "recovery_succeeded"` and `milestone_text` is present for completed-subgoal milestones.
- `agent://episodes_retrieved` — Spec 2 D33. Emitted by `StateRunner` when an episodic-retrieval pass returns at least one candidate. Triggered once per run at the run-start retrieval slot and on each `Exploring/Executing -> Recovering` phase transition. Payload: `{ run_id, trigger: "run_start" | "recovering_entry", count, episode_ids: string[], scope_breakdown: { workflow, global } }`.
- `agent://episode_written` — Spec 2 D33. Emitted by the background `EpisodicWriter` task after a recovery snapshot is persisted to a SQLite store. Payload: `{ run_id, outcome: "inserted" | "merged" | "dropped: <reason>", episode_id, scope: "workflow_local" | "global", occurrence_count }`.
- `agent://episode_promoted` — Spec 2 D33. Emitted by the run-terminal promotion pass after the writer copies one or more workflow-local episodes into the global cross-workflow store. Payload: `{ run_id, promoted_episode_ids: string[], skipped_count }`. On dedup-merge the existing global row's ID is reported (not a freshly minted ID), so the IDs always resolve in the global store.
- `agent://skill_extracted` — emitted when an online extractor pass writes or merges a draft skill. Payload: `{ run_id, event_run_id, skill_id, version, state, scope }`.
- `agent://skill_confirmed` — emitted when a draft skill is confirmed by the command layer. Payload: `{ run_id, event_run_id, skill_id, version }`.
- `agent://skill_invoked` — emitted when a run resolves and starts replaying a confirmed/promoted skill. Payload: `{ run_id, event_run_id, skill_id, version, parameter_count }`.

`agent://node_added` and `agent://edge_added` remain per-step engine emissions. The frontend buffers them by `run_id` and commits them to the canvas only on a clean `agent://complete` terminal event; stopped, errored, destructive-cap, and disagreement-cancelled runs drop the buffer. See the frontend reference for the canvas-materialization projection.

After `StepOutcome::Done`, the loop runs a VLM completion check when a vision backend is attached: it takes a screenshot via `take_screenshot`, sends it with the goal and agent summary, and parses YES/NO from the reply. YES lets the run complete normally (`Completed`). NO halts the run with `TerminalReason::CompletionDisagreement` and emits `agent://completion_disagreement`. Verification errors (no vision backend, screenshot failure, empty or failed VLM response) log a warning and fall through to the legacy `Completed` path — a broken verifier must not tank successful runs.

All payloads carry the `run_id` so stale events from a prior run can be filtered on the UI side.

### Operator Controls

- `stop_agent` — cancels the running loop; sends an explicit rejection through any pending approval so the engine returns `Ok(false)` instead of "approval unavailable". Also resolves a pending VLM-disagreement oneshot as `Cancel` so the run still records a truthful `DisagreementCancelled` terminal reason (instead of an ambiguous `unknown`).
- `approve_agent_action { approved: bool }` — responds to the current pending approval.
- `resolve_completion_disagreement { action: "confirm" | "cancel" }` — resolves a pending VLM completion disagreement. `confirm` records the run as successful with a `DisagreementConfirmed` terminal reason and emits `agent://complete`. `cancel` records it as failed with a `DisagreementCancelled` reason and emits `agent://stopped { reason: "user_cancelled_disagreement" }`. Both paths append a `CompletionDisagreementResolved` entry to `events.jsonl` and a `VariantEntry` with a distinct `divergence_summary`.

### Episodic Memory (Spec 2)

The engine maintains a two-tier episodic memory layer (`crates/clickweave-engine/src/agent/episodic/`) so the agent can recall how it recovered from similar stuck states in past runs. Episodic is a **derived view** over `events.jsonl` — it never owns ground truth — and runs entirely best-effort: every failure path is swallowed (D32) so an unhealthy SQLite store never tanks an agent run.

**Boundary type.** `EpisodicContext` is the engine-boundary type the Tauri layer constructs once per run. It carries `{ enabled, workflow_local_path, global_path: Option, workflow_hash }`. The disabled context (`EpisodicContext::disabled()`) is the no-op shape — the runner skips every retrieval and write when `enabled = false`. The Tauri command sets `enabled = false` whenever the privacy `store_traces` kill switch is off (D34) or the operator turned the master kill switch off in settings.

**Retrieval triggers.** The runner runs `try_retrieve_episodic` once per outer-loop iteration; the helper itself is the gate (D24). It fires retrieval only on:

1. **Run-start** — `step_index == 0`. Lets the agent surface relevant past recoveries before it commits to a strategy.
2. **`Recovering`-entry** — the harness-inferred `Phase` flips from `Exploring` / `Executing` to `Recovering`. Captured at the top of the iteration via `prev_phase_at_top` so the same call simultaneously emits `EpisodesRetrieved` and snapshots a `RecoveringEntrySnapshot` for the matching `Recovering -> Executing` exit (the eventual write hangs off Spec 1's existing `RecoverySucceeded` boundary).

Retrieved episodes render as a `<retrieved_recoveries>` block above the observation in the user-turn message (`render::render_retrieved_recoveries_block`), preserving D6's stable system-prompt invariant.

**Storage.** Each scope is a separate SQLite database (D26): `<workflow_dir>/episodic.sqlite` for workflow-local, `<app_data_dir>/episodic.sqlite` for global. The global file is opt-in per run — the Tauri command sets `EpisodicContext::global_path = Some(...)` only when the operator enabled "Share recoveries across workflows" (D35).

**Write path.** Async, fire-and-forget. The runner queues `WriteRequest::DeriveAndInsert` to a bounded `mpsc::channel<WriteRequest>` at the `RecoverySucceeded` guard; the consumer task derives an `EpisodeRecord` and inserts via the dedup-aware path. Channel back-pressure surfaces as `EpisodicError::Backpressure` and the runner drops the request silently — the agent loop never blocks on episodic.

**Promotion.** Run-terminal. The Tauri command queues a single `WriteRequest::PromotePass` after the agent loop returns, gated on a clean terminal (`TerminalReason::Completed` → `PromotionTerminalKind::Clean`; everything else, including `DisagreementConfirmed`, `CompletionDisagreement`, `LoopDetected`, and errored terminals → `SkipPromotion`) AND the operator's global-participation opt-in. Workflow-local rows are still written for non-clean terminals; only global-tier promotion is gated. The pure `should_promote(occurrence_count, global_has_match)` rule promotes a row when its workflow-local `occurrence_count >= 2` OR a row with the same `pre_state_signature` already exists in global (cross-workflow confirmation, D31).

**Source-of-truth invariant.** `events.jsonl` remains authoritative. Every episode row carries `step_record_refs: Vec<String>` pointing back to the `events.jsonl` line that fed it, so the trace-retention sweep in `src-tauri/src/privacy.rs` can sweep orphaned rows when their backing trace is deleted (D36).
