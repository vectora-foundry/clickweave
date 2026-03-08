# Frontend Architecture (Reference)

Verified at commit: `d0fd809`

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
│   ├── LoopGroupNode.tsx
│   ├── NodePalette.tsx
│   ├── AssistantPanel.tsx
│   ├── ChatMessage.tsx
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
│   ├── useLoopGrouping.ts
│   ├── useNodeSync.ts
│   ├── useEdgeSync.ts
│   ├── useWorkflowActions.ts
│   ├── useExecutorEvents.ts
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
│       ├── assistantSlice.ts
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
    ├── loopMembers.ts
    ├── walkthroughDraft.ts
    └── walkthroughFormatting.ts
```

## State Model

`StoreState` is the intersection of 9 slices:

- `ProjectSlice`
- `ExecutionSlice`
- `AssistantSlice`
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
- actions: `setExecutorState`, `setExecutionMode`, `setSupervisionPause`, `clearSupervisionPause`, `supervisionRespond`, `runWorkflow`, `stopWorkflow`, `setLastRunStatus`

**AssistantSlice** (`assistantSlice.ts`)

- `conversation`, `assistantOpen`, `assistantLoading`, `assistantRetrying`, `assistantError`
- `pendingPatch`, `pendingPatchWarnings`
- actions: `sendAssistantMessage`, `resendMessage`, `applyPendingPatch`, `discardPendingPatch`, `cancelAssistantChat`, `clearConversation`

**SettingsSlice** (`settingsSlice.ts`)

- `plannerConfig`, `agentConfig`, `vlmConfig`, `vlmEnabled`, `mcpCommand`, `maxRepairAttempts`
- persistence via `store/settings.ts` (`settings.json` through Tauri plugin-store)

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

- `walkthroughStatus`, `walkthroughPanelOpen`, `walkthroughError`, `walkthroughEvents`, `walkthroughActions`, `walkthroughDraft`, `walkthroughWarnings`, `walkthroughAnnotations`, `walkthroughActionNodeMap`, `walkthroughCdpModalOpen`, `walkthroughCdpProgress`
- actions: `startWalkthrough(cdpApps?)`, `pauseWalkthrough`, `resumeWalkthrough`, `stopWalkthrough`, `cancelWalkthrough`, `fetchWalkthroughDraft`, `applyDraftToCanvas`, `discardDraft`, `openCdpModal`, `closeCdpModal`, `pushCdpProgress`, annotation editing actions (`deleteNode`, `restoreNode`, `renameNode`, `overrideTarget`, `promoteToVariable`, etc.)
- manages recording bar overlay window lifecycle and CDP app selection modal

## App Event Wiring

`ui/src/hooks/useExecutorEvents.ts` subscribes to backend events (called from `App.tsx`):

- `executor://log`
- `executor://state`
- `executor://node_started`
- `executor://node_completed`
- `executor://node_failed`
- `executor://checks_completed`
- `executor://workflow_completed`
- `executor://supervision_passed`
- `executor://supervision_paused`
- `assistant://repairing`
- `walkthrough://state`
- `walkthrough://event`
- `walkthrough://draft_ready`
- `walkthrough://cdp-setup`
- `recording-bar://action`

`App.tsx` also listens to menu events (`menu://new`, `menu://open`, etc.) and maps them to store actions.

## Graph Editor (`GraphCanvas`)

`GraphCanvas.tsx` is a thin composition shell that delegates to three hooks:

- `useLoopGrouping` — loop collapse state, hidden node tracking
- `useNodeSync` — RF node state, position tracking, selection sync
- `useEdgeSync` — RF edge filtering, change handling, connect

### Node type keys

Registered node types:

- `workflow` -> `WorkflowNode`
- `loopGroup` -> `LoopGroupNode`

### Behavior

- Palette click adds a node (not drag-and-drop)
- Handle-to-handle connect creates edges
- Delete key removes selected nodes/edges (multi-select supported; independently selected edges are removed silently via `removeEdgesOnly` without a separate history entry)
- Node selection drives detail modal visibility
- Loop groups support collapsed/expanded rendering and child containment

Control-flow edge labels shown in canvas:

- `IfTrue`, `IfFalse`
- `SwitchCase(name)`, `SwitchDefault`
- `LoopBody`, `LoopDone`

## Node Detail Modal

`NodeDetailModal` is rendered as a flex sidebar (not a floating overlay). It has 3 tabs:

- `Setup`: node params, enabled flag, timeout, settle delay, retries, trace level. For eligible node types (`FindText`, `FindImage`, `TakeScreenshot`, `ListWindows`): Verification role toggle + expected outcome field
- `Trace`: trace events + artifact preview/lightbox for selected run
- `Runs`: run history list (can jump to Trace tab)

## Settings Defaults

From `ui/src/store/state.ts` and `settings.ts`:

- endpoint default: `http://localhost:1234/v1`, model `local`, empty API key
- `vlmEnabled`: `false`
- `mcpCommand`: `"npx"`
- `maxRepairAttempts`: `3`

`maxRepairAttempts` is clamped to `0..10` in `settingsSlice.ts`.

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

Do not edit manually.

## Key Files

| File | Role |
|------|------|
| `ui/src/App.tsx` | top-level layout, menu event listeners, app kind map |
| `ui/src/components/GraphCanvas.tsx` | React Flow graph editor |
| `ui/src/components/WorkflowNode.tsx` | standard node renderer |
| `ui/src/components/LoopGroupNode.tsx` | expanded loop group renderer |
| `ui/src/components/NodePalette.tsx` | collapsible node palette (left sidebar) |
| `ui/src/components/WalkthroughPanel.tsx` | walkthrough recording review panel |
| `ui/src/components/VerdictModal.tsx` | verdict detail modal |
| `ui/src/components/node-detail/NodeDetailModal.tsx` | node detail sidebar |
| `ui/src/components/node-detail/tabs/TraceTab.tsx` | trace + artifact viewer |
| `ui/src/store/useAppStore.ts` | composed Zustand store hook |
| `ui/src/store/useWorkflowMutations.ts` | node/edge mutation helpers with history push (`removeEdgesOnly` for silent edge removal) |
| `ui/src/store/slices/types.ts` | `StoreState` composition |
| `ui/src/store/slices/walkthroughSlice.ts` | walkthrough recording state and actions |
| `ui/src/store/slices/historySlice.ts` | undo/redo state and actions |
| `ui/src/store/settings.ts` | persisted settings I/O |
| `ui/src/components/SupervisionModal.tsx` | supervision pause modal (retry / skip / abort) |
| `ui/src/hooks/useLoopGrouping.ts` | loop collapse state, hidden node tracking |
| `ui/src/hooks/useNodeSync.ts` | RF node state, position tracking, selection sync |
| `ui/src/hooks/useEdgeSync.ts` | RF edge filtering, change handling |
| `ui/src/hooks/useWorkflowActions.ts` | workflow mutation dispatchers (wraps `useWorkflowMutations`) |
| `ui/src/hooks/useEscapeKey.ts` | global Escape key handler that closes panels in priority order |
| `ui/src/hooks/useHorizontalResize.ts` | horizontal panel resize drag handle |
| `ui/src/hooks/useExecutorEvents.ts` | Backend event subscriptions (executor, walkthrough, assistant) |
| `ui/src/hooks/useUndoRedoKeyboard.ts` | Ctrl+Z / Ctrl+Shift+Z keyboard binding |
| `ui/src/components/CdpAppSelectModal.tsx` | CDP app selection modal for walkthrough recording |
| `ui/src/utils/appKind.ts` | App kind classification helpers |
| `ui/src/utils/walkthroughDraft.ts` | Walkthrough draft processing utilities |
| `ui/src/utils/walkthroughFormatting.ts` | Walkthrough event formatting for display |
| `ui/src/constants/nodeMetadata.ts` | Node type display metadata (icons, colors, labels) |
