# Frontend Architecture (Reference)

Verified at commit: `d77b18d`

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
│   ├── NodePalette.tsx
│   ├── LogsDrawer.tsx
│   ├── FloatingToolbar.tsx
│   ├── Header.tsx
│   ├── VerdictBar.tsx
│   ├── VerdictModal.tsx
│   ├── SettingsModal.tsx
│   ├── SupervisionModal.tsx
│   ├── WalkthroughPanel.tsx
│   ├── RecordingBarView.tsx
│   ├── CdpAppSelectModal.tsx
│   ├── ImageLightbox.tsx
│   ├── IntentEmptyState.tsx
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
│       ├── historySlice.ts
│       ├── settingsSlice.ts
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
- `HistorySlice`
- `SettingsSlice`
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

The agent slice owns the live state of the observe-act agent loop. The loop emits `agent://*` events as it runs; the slice folds them into UI state.

- `agentStatus: "idle" | "running" | "complete" | "stopped" | "error"`
- `agentGoal: string`, `agentSteps: AgentStep[]`, `agentError: string | null`, `currentAgentStep: number`
- `pendingApproval: PendingApproval | null` — populated when the agent asks the user to approve the next tool invocation
- `agentRunId: string | null` — per-run generation ID used to drop stale events from a prior run
- actions: `startAgent(goal)`, `stopAgent`, `addAgentStep`, `addAgentNode`, `addAgentEdge`, `setPendingApproval`, `approveAction`, `rejectAction`, `setAgentStatus`, `setAgentError`, `setAgentRunId`, `resetAgent`

**SettingsSlice** (`settingsSlice.ts`)

- `plannerConfig`, `agentConfig`, `fastConfig`, `fastEnabled`, `maxRepairAttempts`, `hoverDwellThreshold`, `supervisionDelayMs`, `toolPermissions`
- persistence via `store/settings.ts` (`settings.json` through Tauri plugin-store)

The `plannerConfig` field name is a tombstone from the pre-agent architecture — the value it carries is now the **supervisor LLM** endpoint used for Test-mode step verdicts and walkthrough-enrichment VLM fallback. `agentConfig` drives the agent loop. `fastConfig` (enabled by `fastEnabled`) is the fast-VLM used for screenshot description before the supervisor runs its judge pass.

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

- `walkthroughStatus`, `walkthroughPanelOpen`, `walkthroughError`, `walkthroughEvents`, `walkthroughActions`, `walkthroughDraft`, `walkthroughWarnings`, `walkthroughCdpModalOpen`, `walkthroughCdpProgress`, `walkthroughAnnotations`, `walkthroughActionNodeMap`
- recording actions: `startWalkthrough(cdpApps?)`, `pauseWalkthrough`, `resumeWalkthrough`, `stopWalkthrough`, `cancelWalkthrough`
- review actions: annotation editing (`deleteNode`, `restoreNode`, `renameNode`, `overrideTarget`, `promoteToVariable`, etc.), `applyDraftToCanvas`
- manages recording bar overlay window lifecycle and CDP app selection modal
- `useWalkthrough` hook provides a focused selector for WalkthroughPanel

## App Event Wiring

`ui/src/hooks/useExecutorEvents.ts` is a thin composer mounted by `App.tsx` that delegates to domain-specific hooks in `ui/src/hooks/events/`:

- `executor://log`, `executor://state`, `executor://node_started`, `executor://node_completed`, `executor://node_failed`
- `executor://checks_completed`, `executor://workflow_completed`
- `executor://supervision_passed`, `executor://supervision_paused`
- `agent://started`, `agent://step`, `agent://complete`, `agent://stopped`, `agent://error`, `agent://warning`, `agent://node_added`, `agent://edge_added`, `agent://approval_required`, `agent://cdp_connected`, `agent://step_failed`, `agent://sub_action`
- `walkthrough://state`, `walkthrough://event`, `walkthrough://draft_ready`, `walkthrough://cdp-setup`
- `recording-bar://action`
- `menu://new`, `menu://open`, `menu://save`, `menu://toggle-sidebar`, `menu://toggle-logs`, `menu://run-workflow`, `menu://stop-workflow`

All `agent://*` payloads carry a `run_id` field. Events whose `run_id` does not match the active run are silently dropped so late-arriving events from a previous run cannot leak into the current UI state.

## Graph Editor (`GraphCanvas`)

`GraphCanvas.tsx` composes the following hooks:

- `useNodeSync` — RF node state, position tracking, selection sync
- `useEdgeSync` — RF edge filtering, change handling, connect
- `useAppGrouping` — auto-group nodes by target app
- `useUserGrouping` — user-created node groups

### Node type keys

Registered node types:

- `workflow` -> `WorkflowNode`
- `appGroup` -> `AppGroupNode` (auto-generated groups by app)
- `userGroup` -> `UserGroupNode` (user-created groups)

### Behavior

- Palette click adds a node (not drag-and-drop)
- Handle-to-handle connect creates edges
- Delete key removes selected nodes/edges (multi-select supported; independently selected edges are removed silently via `removeEdgesOnly` without a separate history entry)
- Node selection drives detail modal visibility

Workflows are persisted as a linear sequence of tool-call nodes — there are no control-flow nodes (If / Switch / Loop / EndLoop) and no conditional edge labels. Edges carry only their source/target, with `from` and `to` fields on `Edge`.

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
| `ui/src/store/slices/agentSlice.ts` | agent loop live state (status, steps, pending approval, run id) |
| `ui/src/store/slices/walkthroughSlice.ts` | walkthrough lifecycle state and CDP modal |
| `ui/src/hooks/useWalkthrough.ts` | focused walkthrough selector hook for WalkthroughPanel |
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
