# Frontend Architecture (Reference)

Verified at commit: `64f9cc2`

The UI is a React 19 + Vite app using Zustand for app state and React Flow for graph editing.

## Stack

| Layer | Technology |
|------|------------|
| Framework | React 19 |
| Build | Vite 6 |
| Styling | Tailwind CSS v4 |
| Graph Editor | `@xyflow/react` |
| State | Zustand (slice composition) |
| Desktop bridge | Tauri v2 (`@tauri-apps/api`) |
| Types/commands | generated `ui/src/bindings.ts` (Specta/tauri-specta) |
| Tests | Vitest + Testing Library |

## Directory Structure

```
ui/src/
├── App.tsx
├── main.tsx
├── bindings.ts
├── components/
│   ├── GraphCanvas.tsx
│   ├── WorkflowNode.tsx
│   ├── AppGroupNode.tsx
│   ├── UserGroupNode.tsx
│   ├── AgentRunGroupNode.tsx
│   ├── DataEdge.tsx
│   ├── NodePalette.tsx
│   ├── LogsDrawer.tsx
│   ├── FloatingToolbar.tsx
│   ├── shell/                    # AppShell, TitleBar, Sidebar, view router (see Phase 5 detail)
│   ├── VerdictBar.tsx
│   ├── VerdictModal.tsx
│   ├── SettingsModal.tsx
│   ├── SupervisionModal.tsx
│   ├── WalkthroughPanel.tsx
│   ├── RecordingBarView.tsx
│   ├── CdpAppSelectModal.tsx
│   ├── ImageLightbox.tsx
│   ├── IntentEmptyState.tsx
│   ├── AssistantPanel.tsx
│   ├── RunTraceView.tsx
│   ├── ExecutionTab.tsx
│   ├── skills/
│   │   ├── SkillsPanel.tsx
│   │   ├── SkillDetailView.tsx
│   │   ├── SkillRefinementForm.tsx
│   │   ├── SkillToolCallNode.tsx
│   │   ├── SkillSubSkillNode.tsx
│   │   └── SkillLoopNode.tsx
│   ├── PermissionsTab.tsx
│   ├── CreateGroupPopover.tsx
│   ├── GroupContextMenu.tsx
│   ├── InlineRenameInput.tsx
│   ├── Modal.tsx
│   └── node-detail/
│       ├── NodeDetailModal.tsx
│       ├── fields/
│       │   └── index.tsx
│       └── tabs/
│           ├── SetupTab.tsx
│           ├── TraceTab.tsx
│           ├── RunsTab.tsx
│           └── editors/
│               ├── ClickEditor.tsx
│               ├── FocusWindowEditor.tsx
│               └── ... (per-node-type editors)
├── hooks/
│   ├── useEscapeKey.ts
│   ├── useHorizontalResize.ts
│   ├── useUndoRedoKeyboard.ts
│   ├── useNodeSync.ts
│   ├── useEdgeSync.ts
│   ├── useWorkflowActions.ts
│   ├── useExecutorEvents.ts
│   ├── useWalkthrough.ts
│   ├── events/
│   │   ├── useExecutorNodeEvents.ts
│   │   ├── useSupervisionEvents.ts
│   │   ├── useAgentEvents.ts
│   │   ├── useWalkthroughEvents.ts
│   │   └── useMenuEvents.ts
│   ├── node-sync/
│   │   ├── nodeBuilders.ts
│   │   ├── useRfNodeBuilder.ts
│   │   └── useNodeChangeHandler.ts
│   └── test-helpers.ts
├── constants/
│   └── nodeMetadata.ts
├── store/
│   ├── useAppStore.ts
│   ├── useWorkflowMutations.ts
│   ├── state.ts
│   ├── settings.ts
│   └── slices/
│       ├── projectSlice.ts
│       ├── executionSlice.ts
│       ├── agentSlice.ts
│       ├── assistantSlice.ts
│       ├── historySlice.ts
│       ├── settingsSlice.ts
│       ├── skillsSlice.ts
│       ├── logSlice.ts
│       ├── verdictSlice.ts
│       ├── walkthroughSlice.ts
│       ├── uiSlice.ts
│       └── types.ts
└── utils/
    ├── appKind.ts
    ├── edgeHandles.ts
    ├── graphValidation.ts
    ├── walkthroughDraft.ts
    ├── walkthroughFormatting.ts
    └── walkthroughGrouping.ts
```

## State Model

`StoreState` is composed from these slices:

- `ProjectSlice`
- `ExecutionSlice`
- `AgentSlice`
- `AssistantSlice`
- `HistorySlice`
- `SettingsSlice`
- `SkillsSlice`
- `LogSlice`
- `VerdictSlice`
- `WalkthroughSlice`
- `UiSlice`

Type is defined in `ui/src/store/slices/types.ts` and store composition in `ui/src/store/useAppStore.ts`.

### Slice Summary

**ProjectSlice** (`projectSlice.ts`)

- `workflow`, `projectPath`, `isNewWorkflow`
- actions: `openProject`, `saveProject`, `newProject`, `setWorkflow`, `skipIntentEntry`

**ExecutionSlice** (`executionSlice.ts`)

- `executorState: "idle" | "running"`, `executionMode: ExecutionMode`, `supervisionPause: SupervisionPause | null`, `lastRunStatus: "completed" | "failed" | null`
- actions: `setExecutorState`, `setExecutionMode`, `setSupervisionPause`, `clearSupervisionPause`, `supervisionRespond`, `runWorkflow`, `stopWorkflow`, `setLastRunStatus`, `setIntent`

**AgentSlice** (`agentSlice.ts`)

The agent slice owns the live state of the state-spine agent loop. The backend `StateRunner` emits `agent://*` events as it runs; the slice folds them into UI state. `AgentStep` / `AgentCommand` remain the current projection rendered by existing panels while Spec 3 surfaces are landing.

- `agentStatus: "idle" | "running" | "complete" | "stopped" | "error"`
- `agentGoal: string`, `agentSteps: AgentStep[]`, `agentError: string | null`, `currentAgentStep: number`
- `pendingApproval: PendingApproval | null` — populated when the agent asks the user to approve the next tool invocation
- `completionDisagreement: CompletionDisagreement | null` — populated when the backend emits `agent://completion_disagreement`; holds the screenshot, VLM reasoning, and agent summary surfaced by the assistant panel's disagreement card
- `agentRunId: string | null` — per-run generation ID used to drop stale events from a prior run
- `pendingRunNodes: Record<string, Node[]>`, `pendingRunEdges: Record<string, Edge[]>` — per-run canvas buffers for agent-produced workflow materialization
- `agentRunCollapsed: Record<string, boolean>` — session-only collapse state for synthetic agent-run containers
- actions: `startAgent(goal)`, `stopAgent`, `addAgentStep`, `bufferAgentNode`, `bufferAgentEdge`, `commitRunBuffer`, `dropRunBuffer`, `toggleAgentRunCollapsed`, `setPendingApproval`, `approveAction`, `rejectAction`, `setCompletionDisagreement`, `confirmDisagreementAsComplete` (invokes `resolve_completion_disagreement` with `"confirm"` — backend writes the durable record and emits `agent://complete`), `cancelDisagreement` (invokes with `"cancel"` — backend emits `agent://stopped { reason: "user_cancelled_disagreement" }`), `setAgentStatus`, `setAgentError`, `setAgentRunId`, `resetAgent`

The state-spine events (`agent://task_state_changed`, `agent://world_model_changed`, `agent://boundary_record_written`) are emitted by `StateRunner` and carried through to `AssistantSlice.runTraces` for the live trace surface. `WorldModelDiff` (payload of `world_model_changed`) is a minimal `{ changed_fields: Vec<String> }` shape — a re-render hint, not a full snapshot.

The Spec 2 episodic-memory events (`agent://episodes_retrieved`, `agent://episode_written`, `agent://episode_promoted`) are emitted by the engine and the background `EpisodicWriter` task, fan out through the same `forward_agent_event` seam, and carry the active `run_id` so the stale-run filter drops late events from a previous run. The slice itself does not yet consume them — they currently support telemetry / future inspector surfaces only. The fields the UI threads into `AgentRunRequest` to control the layer (`episodic_enabled`, `retrieved_episodes_k`, `episodic_global_participation`) are sourced from the SettingsSlice fields documented below.

The Spec 3 skill events are also subscribed in `useAgentEvents`: `agent://skill_extracted` updates the Skills panel index, `agent://skill_confirmed` moves a draft into Confirmed even when confirmation happens outside an active run, and `agent://skill_invoked` records a log line for the active run.

**AssistantSlice** (`assistantSlice.ts`)

Owns the conversational surface. `messages` is the source of truth for continuation: each `user` + `assistant` pair shares a `runId` so the backend can build `prior_turns` for the next turn. `system` messages are deletion annotations (center-aligned, muted blue) with no `runId`.

- `messages: AssistantMessage[]` where `AssistantMessage.role` is `"user" | "assistant" | "system"` and `runId?: string` is present for user/assistant pairs
- `assistantOpen: boolean`, `assistantError: string | null`
- `runTraces: Record<string, RunTrace>` — live trace state keyed by agent `run_id`. `RunTrace` contains `{ runId, phase, activeSubgoal, steps, worldModelDeltas, milestones, terminalFrame }`, where steps carry tool name/body/failure state, deltas carry changed world-model field names, milestones mirror completed-subgoal/recovery boundaries, and terminal frames distinguish complete/stopped/error/disagreement-cancelled endings.
- actions: `setAssistantOpen`, `toggleAssistant`, `setAssistantError`, `pushAssistantMessage(role, content, runId?)`, `pushSystemAnnotation`, `clearConversation`, `clearConversationFlow`, `setMessages`, `mapMessagesByRunIds`, `dropTurnsByRunIds`, `applyTaskStateUpdate`, `applyWorldModelDelta`, `applyBoundary`, `pushTraceStep`, `setTerminalFrame`, `clearTrace`
- persisted per-workflow to `agent_chat.json`, hydrated on project open; saves are best-effort and gated on `storeTraces`
- opening the panel while a walkthrough is `Recording` or `Paused` cancels it; `Review`/`Processing` state is kept and just hidden behind the assistant

`RunTraceView.tsx` renders the active `RunTrace` inside `AssistantPanel` while an agent run is active. It shows the harness phase, active subgoal, per-step records, world-model delta hints, boundary milestones, and the terminal frame. Before the first trace event arrives it renders a small "Agent running..." fallback scoped to the trace component; `AssistantPanel` itself no longer owns a standalone spinner/status row.

**SkillsSlice** (`skillsSlice.ts`)

- `drafts`, `confirmed`, `promoted` — bucketed `SkillSummary` entries loaded from the backend panel index
- `selectedSkill` and `breadcrumb` — selection state plus deterministic sub-skill navigation
- actions: `loadSkillsForPanel`, `setSkillsList`, `setSelectedSkill`, `clearSelectedSkill`, `findSkill`, `applySkillExtracted`, `applySkillConfirmed`, breadcrumb push/pop helpers
- `loadSkillsForPanel` calls `list_skills_for_panel` for project-local skills and, when `skillsGlobalParticipation` is enabled, the global tier; the two result sets are bucketed client-side

**SettingsSlice** (`settingsSlice.ts`)

- `supervisorConfig`, `agentConfig`, `fastConfig`, `fastEnabled`, `maxRepairAttempts`, `hoverDwellThreshold`, `supervisionDelayMs`, `toolPermissions`, `traceRetentionDays`, `storeTraces`, `episodicEnabled`, `retrievedEpisodesK`, `episodicGlobalParticipation`, `skillsEnabled`, `applicableSkillsK`, `skillsGlobalParticipation`
- persistence via `store/settings.ts` (`settings.json` through Tauri plugin-store)

`supervisorConfig` is the supervisor LLM endpoint used for Test-mode step verdicts and walkthrough-enrichment VLM fallback. `agentConfig` drives the agent loop. `fastConfig` (enabled by `fastEnabled`) is the fast-VLM used for screenshot description before the supervisor runs its judge pass.

`traceRetentionDays` (default 30, `0` disables cleanup) drives the run-trace retention sweep at app startup; `storeTraces` (default on) is the privacy kill switch threaded into each run request — when off, agent and workflow runs execute entirely in memory and nothing is written under `.clickweave/runs/`.

The Spec 2 episodic-memory controls are surfaced under the Execution settings tab's "Agent Memory" subsection:

- `episodicEnabled` (default `true`) — master kill switch. When off, the engine builds an `EpisodicContext::disabled()` for the run regardless of the global-participation flag, and the runner skips every retrieval and write.
- `retrievedEpisodesK` (default `2`, range `[1, 10]`) — top-k episodes returned per retrieval trigger. Higher values surface more candidate recoveries at the cost of a longer `<retrieved_recoveries>` block in the user turn.
- `episodicGlobalParticipation` (default `false`, **privacy opt-in**) — when on, recovery episodes from this workflow may be promoted into the cross-workflow `<app_data_dir>/episodic.sqlite` store. Default off keeps every workflow's recoveries strictly isolated; the global path is only opened when this flag is true.

All three flow through `AgentRunRequest` (`episodic_enabled`, `retrieved_episodes_k`, `episodic_global_participation`) and are persisted to `settings.json` via `saveSetting` per-key.

The Spec 3 procedural-skills controls are surfaced under the Execution settings tab's "Agent Skills" subsection:

- `skillsEnabled` (default `true`) — master kill switch. When off, the backend receives `skills_enabled = false` and builds a disabled `SkillContext`; no extraction, retrieval, or replay runs for that agent execution.
- `applicableSkillsK` (default `2`, range `[1, 10]`) — top-k procedural skills rendered per `push_subgoal` retrieval trigger.
- `skillsGlobalParticipation` (default `false`, privacy opt-in) — when on, panel listing and agent runs can include the global skill tier; default off keeps project-local skills isolated.

All three flow through `AgentRunRequest` (`skills_enabled`, `applicable_skills_k`, `skills_global_participation`) and are persisted to `settings.json` via `saveSetting` per-key.

**UiSlice** (`uiSlice.ts`)

- selection/panel state (`selectedNode`, `detailTab`, drawer/modal flags)
- feature toggles: `allowAiTransforms`, `allowAgentSteps`
- node type metadata (`nodeTypes`) loaded from backend

**HistorySlice** (`historySlice.ts`)

- `past: HistoryEntry[]`, `future: HistoryEntry[]` — undo/redo stacks (max 50 entries)
- actions: `pushHistory`, `undo`, `redo`, `clearHistory`
- Workflow mutations push snapshots via `useWorkflowMutations` before each change

**LogSlice**

- log buffer used by Logs drawer

**VerdictSlice** (`verdictSlice.ts`)

- `verdicts: NodeVerdict[]`, `verdictStatus: VerdictStatus`, `verdictBarVisible`, `verdictModalOpen`
- `VerdictStatus`: `"none" | "passed" | "failed" | "warned" | "completed"`
- actions: `setVerdicts`, `dismissVerdictBar`, `clearVerdicts`, `openVerdictModal`, `closeVerdictModal`

**WalkthroughSlice** (`walkthroughSlice.ts`)

- `walkthroughStatus`, `walkthroughPanelOpen`, `walkthroughError`, `walkthroughSessionId`, `walkthroughEvents`, `walkthroughActions`, `walkthroughDraft`, `walkthroughWarnings`, `walkthroughCdpModalOpen`, `walkthroughCdpProgress`, `walkthroughAnnotations`, `walkthroughActionNodeMap`, `walkthroughExpandedAction`, `walkthroughNodeOrder`
- recording actions: `startWalkthrough(cdpApps?)`, `pauseWalkthrough`, `resumeWalkthrough`, `stopWalkthrough`, `cancelWalkthrough`
- review actions: annotation editing (`deleteNode`, `restoreNode`, `renameNode`, `overrideTarget`, `promoteToVariable`, etc.), `applyDraftToCanvas`
- manages recording bar overlay window lifecycle and CDP app selection modal
- `useWalkthrough` hook provides a focused selector for WalkthroughPanel
- `WalkthroughPanel` can call `save_walkthrough_as_skill` during Review using `walkthroughSessionId`, then refresh the Skills panel index

## App Event Wiring

`ui/src/hooks/useExecutorEvents.ts` is a thin composer mounted by `App.tsx` that delegates to domain-specific hooks in `ui/src/hooks/events/`:

- `executor://log`, `executor://state`, `executor://node_started`, `executor://node_completed`, `executor://node_failed`
- `executor://checks_completed`, `executor://workflow_completed`
- `executor://supervision_passed`, `executor://supervision_paused`
- `agent://started`, `agent://step`, `agent://complete`, `agent://completion_disagreement`, `agent://completion_disagreement_resolved`, `agent://stopped`, `agent://error`, `agent://warning`, `agent://node_added`, `agent://edge_added`, `agent://approval_required`, `agent://cdp_connected`, `agent://step_failed`, `agent://sub_action`
- State-spine additions (payloads carry `run_id` per D17, filtered by `isStaleRunId` alongside all other `agent://*` events):
  - `agent://task_state_changed` — full `TaskState` snapshot emitted after any turn that applied at least one mutation
  - `agent://world_model_changed` — emitted once per step after `observe`; payload carries a `WorldModelDiff { changed_fields: string[] }` re-render hint, not the full model
  - `agent://boundary_record_written` — emitted when the runner persists a `StepRecord`; payload `{ boundary_kind, step_index, milestone_text }` where `boundary_kind` is `"terminal" | "subgoal_completed" | "recovery_succeeded"` and `milestone_text` is present for completed-subgoal milestones
- Spec 2 episodic-memory additions (same stale-run filtering; payload shapes locked by D33):
  - `agent://episodes_retrieved` — payload `{ trigger: "run_start" | "recovering_entry", count, episode_ids: string[], scope_breakdown: { workflow, global } }`. Fired by the runner when a retrieval pass returned at least one candidate.
  - `agent://episode_written` — payload `{ outcome: "inserted" | "merged" | "dropped: <reason>", episode_id, scope: "workflow_local" | "global", occurrence_count }`. Fired by the background `EpisodicWriter` task after each successful insert / merge.
  - `agent://episode_promoted` — payload `{ promoted_episode_ids: string[], skipped_count }`. Fired once at run-terminal when the promotion pass copies eligible workflow-local episodes into the global cross-workflow store. IDs in `promoted_episode_ids` are the actual global-store row IDs (existing IDs on dedup-merge, freshly minted IDs on insert), so they always resolve in the global store.
- Spec 3 procedural-skill additions:
  - `agent://skill_extracted` — payload `{ run_id, event_run_id, skill_id, version, state, scope }`. Updates the Skills panel buckets without polling.
  - `agent://skill_confirmed` — payload `{ run_id, event_run_id, skill_id, version }`. Moves a draft into Confirmed; this event is not stale-run gated because panel-driven confirmation can happen outside an active run.
  - `agent://skill_invoked` — payload `{ run_id, event_run_id, skill_id, version, parameter_count }`. Stale-run gated and logged for the active run.
- `walkthrough://state`, `walkthrough://event`, `walkthrough://draft_ready`, `walkthrough://cdp-setup`
- `recording-bar://action`
- `menu://new`, `menu://open`, `menu://save`, `menu://toggle-sidebar`, `menu://toggle-logs`, `menu://run-workflow`, `menu://stop-workflow`

All `agent://*` payloads carry a `run_id` field. Events whose `run_id` does not match the active run are silently dropped (`isStaleRunId` in `useAgentEvents`) so late-arriving events from a previous run cannot leak into the current UI state. The three state-spine additions above are filtered through the same stale-run gate.

Agent canvas materialization is deferred by run. `agent://node_added` and `agent://edge_added` append to `pendingRunNodes[run_id]` and `pendingRunEdges[run_id]`. `agent://complete` commits the buffered nodes and edges into the workflow in one clean-terminal batch. `agent://stopped`, `agent://error`, `agent://consecutive_destructive_cap_hit`, disagreement cancellation, and Clear conversation drop the active buffer. Clean-terminal commit never creates or mutates `workflow.groups`; grouping for agent output is a React Flow projection.

## Graph Editor (`GraphCanvas`)

`GraphCanvas.tsx` composes the following hooks:

- `useNodeSync` — RF node state, position tracking, selection sync
- `useEdgeSync` — RF edge filtering, change handling, connect
- `useAppGrouping` — auto-group nodes by target app
- `useUserGrouping` — user-created node groups

### Node type keys

Workflow canvas node types:

- `workflow` -> `WorkflowNode`
- `appGroup` -> `AppGroupNode` (auto-generated groups by app)
- `userGroup` -> `UserGroupNode` (user-created groups)
- `agent_run_group` -> `AgentRunGroupNode` (synthetic, session-only container for nodes produced by one agent run)

Skill-detail canvas node types are selected when `GraphCanvas` receives a `skillSource` prop. The canvas is read-only and uses:

- `skillToolCall` -> `SkillToolCallNode`
- `skillSubSkill` -> `SkillSubSkillNode`
- `skillLoop` -> `SkillLoopNode`

### Behavior

- Palette click adds a node (not drag-and-drop)
- Handle-to-handle connect creates edges
- Delete key removes selected nodes/edges (multi-select supported; independently selected edges are removed silently via `removeEdgesOnly` without a separate history entry)
- Node selection drives detail modal visibility

Agent-run containers are projected in `useRfNodeBuilder` between app-group rendering and user-group rendering. The composition rules are:

- Workflow nodes with `source_run_id`, no app group, and no user group are parented directly under `agent-run-${run_id}`.
- App-group containers whose member workflow nodes all share the same `source_run_id` are parented under the matching agent-run container, preserving the inner app group.
- Mixed-source app groups are left unwrapped.
- User-created groups take precedence; members of a user group are not wrapped by the agent-run projection.

`AgentRunGroupNode` is synthetic only. Its React Flow id is `agent-run-${run_id}` and it is never persisted to `workflow.groups`. Deleting the synthetic container expands to the underlying workflow nodes for that `source_run_id`.

Workflows are persisted as a linear sequence of tool-call nodes — there are no control-flow nodes (If / Switch / Loop / EndLoop) and no conditional edge labels. Edges carry only their source/target, with `from` and `to` fields on `Edge`.

## Skills Panel

`ui/src/components/skills/SkillsPanel.tsx` is the left-rail index for procedural skills. It renders three buckets from `SkillsSlice`: Drafts, Confirmed, and Promoted. Each entry shows skill name plus version and writes `(skill_id, version)` into `selectedSkill` when clicked.

`SkillDetailView.tsx` renders the selected skill's metadata and projects its `action_sketch` into a read-only `GraphCanvas` `skillSource`. Tool calls, sub-skills, and loops use the skill-specific React Flow node registry. Clicking a sub-skill pushes the parent onto the breadcrumb stack and selects the pinned child `(skill_id, version)` so navigation is deterministic.

Draft skills can carry a sibling proposal payload. When present, `SkillDetailView` renders `SkillRefinementForm`, whose Confirm path calls `confirm_skill_proposal` with the edited parameter schema and binding corrections; Reject calls `reject_skill_proposal`.

Walkthrough Review exposes `Save as Skill`, which calls `save_walkthrough_as_skill` with the current `walkthroughSessionId`, workflow identity, and project path. On success it reloads `list_skills_for_panel` so the new draft appears in the Skills panel.

## Node Detail Modal

`NodeDetailModal` is rendered as a flex sidebar (not a floating overlay). It has 3 tabs:

- `Setup`: node params, enabled flag, timeout, settle delay, retries, trace level. For eligible node types (`FindText`, `FindImage`, `TakeScreenshot`): Verification role toggle + expected outcome field
- `Trace`: trace events + artifact preview/lightbox for selected run
- `Runs`: run history list (can jump to Trace tab)

## Settings Defaults

From `ui/src/store/state.ts` and `settings.ts`:

- endpoint default: `http://localhost:1234/v1`, model `local`, empty API key
- `fastEnabled`: `false`
- `maxRepairAttempts`: `3`
- `hoverDwellThreshold`: `2000`
- `supervisionDelayMs`: `500`

`mcpCommand` was removed — the MCP binary is now resolved automatically by the backend.

`maxRepairAttempts` is clamped to `0..10`, `hoverDwellThreshold` to `100..10000`, and `supervisionDelayMs` to `0..10000` in `settingsSlice.ts`.

## Generated Bindings

`ui/src/bindings.ts` is generated in debug mode from Rust command/type definitions.

Contains:

- `commands.*` typed Tauri wrappers (including `commands.supervisionRespond(action)` for resuming a paused supervision check)
- mirrored Rust types/unions
- command result wrappers

Notable types:

- `ExecutionMode` — `"Test" | "Run"`, selects whether the executor runs in supervised test mode or unattended run mode
- `SupervisionPause` — `{ nodeId, nodeName, finding, screenshot }`, defined in `executionSlice.ts`; represents a paused supervision check awaiting user decision
- `NodeRole` — `"Default" | "Verification"`
- `WalkthroughStatus`, `WalkthroughAction`, `WalkthroughAnnotations`, `WalkthroughDraftResponse`, `ActionNodeEntry` — walkthrough recording and review types
- `AgentRunRequest` — request payload for `run_agent` (goal, agent endpoint, project path, workflow name, workflow id)

Do not edit manually.

## Key Files

| File | Role |
|------|------|
| `ui/src/App.tsx` | top-level layout, menu event listeners, app kind map |
| `ui/src/components/GraphCanvas.tsx` | React Flow graph editor |
| `ui/src/components/WorkflowNode.tsx` | standard node renderer |
| `ui/src/components/NodePalette.tsx` | collapsible node palette (left sidebar) |
| `ui/src/components/WalkthroughPanel.tsx` | walkthrough recording review panel |
| `ui/src/components/VerdictModal.tsx` | verdict detail modal |
| `ui/src/components/node-detail/NodeDetailModal.tsx` | node detail sidebar |
| `ui/src/components/node-detail/tabs/TraceTab.tsx` | trace + artifact viewer |
| `ui/src/store/useAppStore.ts` | composed Zustand store hook |
| `ui/src/store/useWorkflowMutations.ts` | node/edge mutation helpers with history push (`removeEdgesOnly` for silent edge removal) |
| `ui/src/store/slices/types.ts` | `StoreState` composition |
| `ui/src/store/slices/agentSlice.ts` | agent loop live state (status, steps, pending approval, completion-disagreement card, run id) |
| `ui/src/store/slices/skillsSlice.ts` | procedural-skill panel buckets, selection, breadcrumb navigation, event reducers |
| `ui/src/store/slices/walkthroughSlice.ts` | walkthrough lifecycle state and CDP modal |
| `ui/src/hooks/useWalkthrough.ts` | focused walkthrough selector hook for WalkthroughPanel |
| `ui/src/components/skills/SkillsPanel.tsx` | left-rail procedural-skill index |
| `ui/src/components/skills/SkillDetailView.tsx` | selected skill metadata, read-only skill canvas, refinement proposal surface |
| `ui/src/components/skills/SkillRefinementForm.tsx` | draft-skill proposal review form |
| `ui/src/components/skills/Skill*Node.tsx` | skill-canvas node renderers for tool calls, sub-skills, and loops |
| `ui/src/store/slices/historySlice.ts` | undo/redo state and actions |
| `ui/src/store/settings.ts` | persisted settings I/O |
| `ui/src/components/SupervisionModal.tsx` | supervision pause modal (retry / skip / abort) |
| `ui/src/hooks/useNodeSync.ts` | RF node state, position tracking, selection sync |
| `ui/src/hooks/useEdgeSync.ts` | RF edge filtering, change handling |
| `ui/src/hooks/useAppGrouping.ts` | auto-group nodes by target app |
| `ui/src/hooks/useUserGrouping.ts` | user-created node groups |
| `ui/src/hooks/useWorkflowActions.ts` | workflow mutation dispatchers (wraps `useWorkflowMutations`) |
| `ui/src/hooks/useEscapeKey.ts` | global Escape key handler that closes panels in priority order |
| `ui/src/hooks/useHorizontalResize.ts` | horizontal panel resize drag handle |
| `ui/src/hooks/useExecutorEvents.ts` | Thin event composer (delegates to `events/*.ts` hooks) |
| `ui/src/hooks/events/useAgentEvents.ts` | Agent-loop event subscriber (`agent://*`) |
| `ui/src/hooks/events/*.ts` | Domain-specific event hooks (executor, supervision, agent, walkthrough, menu) |
| `ui/src/hooks/useUndoRedoKeyboard.ts` | Ctrl+Z / Ctrl+Shift+Z keyboard binding |
| `ui/src/components/CdpAppSelectModal.tsx` | CDP app selection modal for walkthrough recording |
| `ui/src/utils/appKind.ts` | App kind classification helpers |
| `ui/src/utils/walkthroughDraft.ts` | Walkthrough draft processing utilities |
| `ui/src/utils/walkthroughFormatting.ts` | Walkthrough event formatting for display |
| `ui/src/constants/nodeMetadata.ts` | Node type display metadata (icons, colors, labels) |
