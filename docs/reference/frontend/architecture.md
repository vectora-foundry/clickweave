# Frontend Architecture (Reference)

The UI is a React 19 + Vite app using Zustand for app state and `react-window` for virtualized skill lists.

## Stack

| Layer | Technology |
|------|------------|
| Framework | React 19 |
| Build | Vite 6 |
| Styling | Tailwind CSS v4 |
| Virtualization | `react-window` (FixedSizeList + ResizeObserver) |
| State | Zustand 5 (slice composition) |
| Desktop bridge | Tauri v2 (`@tauri-apps/api`) |
| Types/commands | generated `ui/src/bindings.ts` (Specta/tauri-specta) |
| Tests | Vitest 4 + Testing Library |

`@xyflow/react` has been removed. There is no canvas or graph rendering surface in the frontend.

## Directory Structure

```
ui/src/
├── App.tsx
├── main.tsx
├── bindings.ts
├── components/
│   ├── skill/                     # SkillView and SkillSectionCard surface
│   │   ├── SkillView.tsx          # virtualized vertical list of SkillSectionCard via react-window
│   │   ├── SkillSectionCard.tsx   # one card per section: run-state badge, fidelity dot, approval overlay, Resume button
│   │   ├── SkillFidelityDot.tsx   # fidelity indicator dot
│   │   ├── SkillSelectionContext.tsx # selection model and edit/inspect mode
│   │   ├── SkillSectionApprovalOverlay.tsx # inline approval card; triggered by SafetyScope::Skill
│   │   ├── SkillPatchDiffPreview.tsx # read-only four-pane diff (markdown / action_sketch / variables / replay.json) + Allow/Cancel
│   │   └── RunWithValuesForm.tsx  # variable input form before kicking off a skill run
│   ├── skills/                    # Skills panel (panel index, detail, refinement)
│   │   ├── SkillsPanel.tsx
│   │   ├── SkillDetailView.tsx
│   │   └── SkillRefinementForm.tsx
│   ├── shell/                     # App shell and view composition
│   │   ├── AppShell.tsx           # root orchestrator, view router, global overlay mounts
│   │   ├── TitleBar.tsx           # in-app bar below OS chrome; wordmark + settings + save
│   │   ├── Sidebar.tsx            # nav rail
│   │   ├── WorkflowRow.tsx        # project-name pencil-edit row
│   │   ├── StatsStrip.tsx         # skills-lifecycle cards + Skills Manager pill
│   │   ├── OverviewView.tsx       # Overview composition; mounts SkillView + WalkthroughSaveSheet
│   │   ├── LiveRuntimeCard.tsx    # phase chip, Step N, elapsed, active tool, run-status pill
│   │   ├── OverviewAssistantCard.tsx # Overview chrome around AssistantThread
│   │   ├── AssistantThread.tsx    # chat-thread body (shared by drawer + Overview card)
│   │   └── LogsBar.tsx            # collapsible chrome: title, count, search, copy, clear
│   ├── AssistantPanel.tsx
│   ├── VerdictBar.tsx
│   ├── VerdictModal.tsx
│   ├── SettingsModal.tsx
│   ├── RecordingBarView.tsx
│   ├── CdpAppSelectModal.tsx
│   ├── ImageLightbox.tsx
│   ├── IntentEmptyState.tsx
│   ├── RunTraceView.tsx
│   ├── ExecutionTab.tsx
│   ├── WalkthroughSaveSheet.tsx   # small overlay anchored to the recording-stop affordance
│   ├── AmbiguityResolutionModal.tsx
│   ├── ConfirmClearConversationModal.tsx
│   ├── Modal.tsx
│   └── PermissionsTab.tsx
├── hooks/
│   ├── useEscapeKey.ts
│   ├── useHorizontalResize.ts
│   ├── useSafetyEventRouter.ts    # routes SafetyScope::Skill → inline overlay; SafetyScope::AdHoc → thread card
│   ├── useExecutorEvents.ts
│   ├── useWalkthrough.ts
│   └── events/
│       ├── index.ts
│       ├── useAgentEvents.ts
│       ├── useExecutorNodeEvents.ts
│       ├── useSupervisionEvents.ts
│       ├── useWalkthroughEvents.ts
│       └── useMenuEvents.ts
├── store/
│   ├── useAppStore.ts
│   ├── state.ts
│   ├── settings.ts
│   └── slices/
│       ├── projectSlice.ts
│       ├── executionSlice.ts
│       ├── agentSlice.ts
│       ├── assistantSlice.ts
│       ├── settingsSlice.ts
│       ├── skillsSlice.ts
│       ├── logSlice.ts
│       ├── verdictSlice.ts
│       ├── walkthroughSlice.ts
│       ├── uiSlice.ts
│       └── types.ts
└── utils/
    ├── appKind.ts
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
- `SettingsSlice`
- `SkillsSlice`
- `LogSlice`
- `VerdictSlice`
- `WalkthroughSlice`
- `UiSlice`

Type is defined in `ui/src/store/slices/types.ts` and store composition in `ui/src/store/useAppStore.ts`.

### Slice Summary

**ProjectSlice** (`projectSlice.ts`)

- `workflow`, `projectPath`, `isNewWorkflow` — the `workflow` field mirrors the `ProjectManifest` fields from the backend (`id`, `name`, `intent`, `schema_version`). `open_project` returns `ProjectData { path, manifest: ProjectManifest }` and the slice hydrates both.
- actions: `openProject`, `saveProject`, `newProject`, `setWorkflow`, `skipIntentEntry`

**ExecutionSlice** (`executionSlice.ts`)

- `executorState: "idle" | "running"`, `executionMode`, `supervisionPause: SupervisionPause | null`, `lastRunStatus: "completed" | "failed" | null`
- actions: `setExecutorState`, `setExecutionMode`, `setSupervisionPause`, `clearSupervisionPause`, `supervisionRespond`, `runSkill`, `stopWorkflow`, `resumeSkillFromFailure`, `setLastRunStatus`, `setIntent`

**AgentSlice** (`agentSlice.ts`)

The agent slice owns the live state of the state-spine agent loop. The backend `StateRunner` emits `agent://*` events as it runs; the slice folds them into UI state.

- `agentStatus: "idle" | "running" | "complete" | "stopped" | "error"`
- `agentGoal: string`, `agentSteps: AgentStep[]`, `agentError: string | null`, `currentAgentStep: number`
- `skillCreationIntent: boolean` — when true, a completed agent run is materialized as a skill draft rather than a standalone run record
- `pendingApproval: PendingApproval | null` — populated when the agent asks the user to approve the next tool invocation
- `completionDisagreement: CompletionDisagreement | null` — populated when the backend emits `agent://completion_disagreement`
- `agentRunId: string | null` — per-run generation ID used to drop stale events
- actions: `startAgent(goal)`, `stopAgent`, `addAgentStep`, `setPendingApproval`, `approveAction`, `rejectAction`, `setCompletionDisagreement`, `confirmDisagreementAsComplete`, `cancelDisagreement`, `setSkillCreationIntent`, `setAgentStatus`, `setAgentError`, `setAgentRunId`, `resetAgent`

State-spine events (`agent://task_state_changed`, `agent://world_model_changed`, `agent://boundary_record_written`) are carried through to `AssistantSlice.runTraces` for the live trace surface.

**AssistantSlice** (`assistantSlice.ts`)

Owns the conversational surface. `messages` is the source of truth for continuation.

- `messages: AssistantMessage[]` where `AssistantMessage.role` is `"user" | "assistant" | "system"` and `runId?: string` is present for user/assistant pairs
- `runTraces: Record<string, RunTrace>` — live trace state keyed by agent `run_id`. `RunTrace` contains `{ runId, phase, activeSubgoal, steps, worldModelDeltas, milestones, terminalFrame }`
- actions: `setAssistantOpen`, `toggleAssistant`, `pushAssistantMessage`, `pushSystemAnnotation`, `clearConversation`, `applyTaskStateUpdate`, `applyWorldModelDelta`, `applyBoundary`, `pushTraceStep`, `setTerminalFrame`, `clearTrace`
- persisted per-project to `agent_chat.json`, hydrated on project open

**SkillsSlice** (`skillsSlice.ts`)

- `drafts`, `confirmed`, `promoted` — bucketed `SkillSummary` entries
- `selectedSkill: Skill | null` — full skill shape loaded via `loadSkillFull`; includes sections, action_sketch, variables
- `sectionRunState: Record<string, SectionRunState>` — per-section execution state for the selected skill
- `failedSectionId: string | null`, `failedSectionError: string | null` — set when a section fails; drives Resume button and error pre-fill in `RunWithValuesForm`
- `breadcrumb` — deterministic sub-skill navigation stack
- actions: `loadSkillsForPanel`, `setSkillsList`, `setSelectedSkill`, `clearSelectedSkill`, `loadSkillFull`, `findSkill`, `applySkillExtracted`, `applySkillConfirmed`, `setSectionRunState`, `markSectionFailed`, `clearSectionRunState`, breadcrumb push/pop helpers

**SettingsSlice** (`settingsSlice.ts`)

- `supervisorConfig`, `agentConfig`, `fastConfig`, `fastEnabled`, `maxRepairAttempts`, `hoverDwellThreshold`, `supervisionDelayMs`, `toolPermissions`, `traceRetentionDays`, `storeTraces`, `episodicEnabled`, `retrievedEpisodesK`, `episodicGlobalParticipation`, `skillsEnabled`, `applicableSkillsK`, `skillsGlobalParticipation`
- persistence via `store/settings.ts` (`settings.json` through Tauri plugin-store)

**UiSlice** (`uiSlice.ts`)

- selection/panel state (`detailTab`, drawer/modal flags)
- `skillFrozen: boolean` — set when the executor emits a running state for the selected skill; cleared on idle. While frozen, the skill editing surface is locked
- feature toggles: `allowAiTransforms`, `allowAgentSteps`

**LogSlice**

- log buffer used by LogsBar

**VerdictSlice** (`verdictSlice.ts`)

- `verdicts: NodeVerdict[]`, `verdictStatus: VerdictStatus`, `verdictBarVisible`, `verdictModalOpen`
- actions: `setVerdicts`, `dismissVerdictBar`, `clearVerdicts`, `openVerdictModal`, `closeVerdictModal`

**WalkthroughSlice** (`walkthroughSlice.ts`)

- `walkthroughStatus`, `walkthroughSaveSheetOpen`, `walkthroughError`, `walkthroughSessionId`, `walkthroughEvents`, `walkthroughActions`, `walkthroughDraft`, `walkthroughWarnings`, `walkthroughCdpModalOpen`, `walkthroughCdpProgress`, `walkthroughAnnotations`, `walkthroughActionNodeMap`, `walkthroughExpandedAction`, `walkthroughNodeOrder`
- recording actions: `startWalkthrough(cdpApps?)`, `pauseWalkthrough`, `resumeWalkthrough`, `stopWalkthrough`, `cancelWalkthrough`
- `save_walkthrough_as_skill` converts a walkthrough session into a skill draft and refreshes the Skills panel index

## SkillView Surface

`ui/src/components/skill/SkillView.tsx` is the primary execution surface. It renders the selected skill's sections as a virtualized vertical scrolling list of `SkillSectionCard` components via `react-window` `FixedSizeList`. A `ResizeObserver` adjusts the list height when the container resizes.

### SkillSectionCard

One card per section. Renders:

- Section title and prose (truncated with expand)
- Run-state badge: `idle | running | succeeded | failed | skipped`
- `SkillFidelityDot` — fidelity confidence indicator
- Inline `SkillSectionApprovalOverlay` when the section has a pending `SafetyScope::Skill` event
- Resume button when `failedSectionId` matches the card's section and the user may resume from that point

### SkillSectionApprovalOverlay

Inline approval card rendered inside the matching `SkillSectionCard`. Replaces the deleted `SupervisionModal`. Receives the pending approval payload routed by `useSafetyEventRouter` and exposes Allow / Cancel controls that call `approve_agent_action`.

### SkillPatchDiffPreview

Read-only four-pane diff preview showing the pending `SkillPatch` changes across all four layers:

| Pane | Layer |
|------|-------|
| markdown | Section prose changes in `SKILL.md` |
| action_sketch | Step sequence changes |
| variables | Variable declaration changes |
| replay.json | Recorded argument changes |

Exposes Allow (calls `apply_skill_patch`) and Cancel controls. Mounted as a modal overlay when a patch is staged for operator review.

### RunWithValuesForm

Variable input form shown before kicking off a skill run. Pre-fills variable defaults from the skill's `variables` layer; pre-fills the failed section's last-known values when resuming after failure. Calls `run_skill` or `resumeSkillFromFailure` based on the entry point.

### WalkthroughSaveSheet

Small overlay anchored to the recording-stop affordance. Prompts the user to name and save the walkthrough as a skill draft. Calls `save_walkthrough_as_skill` with the current `walkthroughSessionId` on confirm.

## SkillPatch Layer

The frontend models a `SkillPatch` as a staged four-layer change to a skill:

| Layer | File | Displayed In |
|-------|------|--------------|
| markdown | `SKILL.md` prose | Pane 1 of `SkillPatchDiffPreview` |
| action_sketch | frontmatter in `SKILL.md` | Pane 2 |
| variables | frontmatter in `SKILL.md` | Pane 3 |
| replay.json | sidecar file | Pane 4 |

`apply_skill_patch` is the Tauri command that commits the patch atomically via the journal protocol described in the engine reference. The frontend never writes skill files directly.

## Safety Event Routing

`useSafetyEventRouter` is mounted once at `AppShell` root. It subscribes to `executor://approval_required` and `executor://supervision_paused` events and dispatches based on the `SafetyScope` discriminant:

- `kind: "skill"` → looks up the `SkillSectionCard` for `section_id` in the currently active skill; sets the card's inline `SkillSectionApprovalOverlay` state
- `kind: "ad_hoc"` → routes to an `AssistantThread`-anchored approval card

Skill identity is frozen at execution start: the router trusts the `skill_id` in the event payload and does not re-derive it from UI selection state.

## App Event Wiring

`ui/src/hooks/useExecutorEvents.ts` is a thin composer mounted by `App.tsx` that delegates to domain-specific hooks in `ui/src/hooks/events/`:

- `executor://log`, `executor://state`, `executor://node_started`, `executor://node_completed`, `executor://node_failed`
- `executor://checks_completed`, `executor://workflow_completed`, `executor://ambiguity_resolved`
- `executor://supervision_passed`, `executor://supervision_paused`
- `agent://started`, `agent://step`, `agent://complete`, `agent://completion_disagreement`, `agent://completion_disagreement_resolved`, `agent://stopped`, `agent://error`, `agent://warning`, `agent://approval_required`, `agent://cdp_connected`, `agent://step_failed`, `agent://sub_action`
- State-spine additions (filtered by `isStaleRunId`): `agent://task_state_changed`, `agent://world_model_changed`, `agent://boundary_record_written`
- Spec 2 episodic additions: `agent://episodes_retrieved`, `agent://episode_written`, `agent://episode_promoted`
- Spec 3 skill additions: `agent://skill_extracted`, `agent://skill_confirmed`, `agent://skill_invoked`
- `walkthrough://state`, `walkthrough://event`, `walkthrough://draft_ready`, `walkthrough://cdp-setup`
- `recording-bar://action`
- `menu://new`, `menu://open`, `menu://save`, `menu://toggle-sidebar`, `menu://toggle-logs`, `menu://run-workflow`, `menu://stop-workflow`

All `agent://*` payloads carry `run_id`. Events whose `run_id` does not match the active run are silently dropped via `isStaleRunId` in `useAgentEvents`.

## Shell Components (`components/shell/`)

`AppShell` is the root orchestrator. It mounts `TitleBar`, `Sidebar`, and a view router that renders `OverviewView` based on `uiSlice.currentView`. True global overlays (`SettingsModal`, `VerdictModal`, `AmbiguityResolutionModal`, `ConfirmClearConversationModal`, `CdpAppSelectModal`) mount at `AppShell` root.

`OverviewView` composes the assistant surface (`OverviewAssistantCard`), the live runtime card (`LiveRuntimeCard`), and `SkillView`. It also mounts `WalkthroughSaveSheet` when `walkthroughSaveSheetOpen` is true.

`LogsBar` (in `AppShell`) provides title + count, search (client-side substring filter), copy, and clear. Row coloring follows substring rules.

`AssistantPanel` is a thin drawer wrapper (resize handle + close button) that delegates its body to `AssistantThread`. `AssistantThread` is also embedded in `OverviewAssistantCard` so the same thread renders in both surfaces without duplication.

## Skills Panel (`components/skills/`)

`SkillsPanel.tsx` is the left-rail index for procedural skills. It renders three buckets from `SkillsSlice`: Drafts, Confirmed, and Promoted. Each entry writes `(skill_id, version)` into `selectedSkill` when clicked.

`SkillDetailView.tsx` renders the selected skill's metadata, parameters, and sections in detail. Draft skills can carry a sibling proposal payload; when present, `SkillDetailView` renders `SkillRefinementForm` whose Confirm path calls `confirm_skill_proposal` and Reject calls `reject_skill_proposal`.

## Settings Defaults

From `ui/src/store/state.ts` and `settings.ts`:

- endpoint default: `http://localhost:1234/v1`, model `local`, empty API key
- `fastEnabled`: `false`
- `maxRepairAttempts`: `3`
- `hoverDwellThreshold`: `2000`
- `supervisionDelayMs`: `500`

`mcpCommand` was removed — the MCP binary is resolved automatically by the backend.

## Generated Bindings

`ui/src/bindings.ts` is generated in debug mode from Rust command/type definitions.

Contains:

- `commands.*` typed Tauri wrappers
- mirrored Rust types/unions
- command result wrappers

Notable types:

- `ProjectManifest` — `{ id, name, intent?, schema_version }` — the slim on-disk project envelope
- `ProjectData` — `{ path, manifest: ProjectManifest }` — returned by `open_project`
- `SkillRun`, `SectionOutcome` — skill execution record and per-section outcome
- `SafetyScope` — `{ kind: "skill", skill_id, section_id, step_id } | { kind: "ad_hoc", run_id }`
- `WalkthroughStatus`, `WalkthroughAction`, `WalkthroughAnnotations`, `WalkthroughDraftResponse`, `ActionNodeEntry` — walkthrough recording and review types
- `AgentRunRequest` — request payload for `run_agent` (goal, agent endpoint, project path)

Do not edit manually.

## Key Files

| File | Role |
|------|------|
| `ui/src/components/shell/AppShell.tsx` | root orchestrator, view router, global overlay mounts |
| `ui/src/components/shell/OverviewView.tsx` | Overview composition (assistant + live runtime + SkillView) |
| `ui/src/components/shell/LogsBar.tsx` | collapsible log chrome (search, copy, clear) |
| `ui/src/App.tsx` | top-level layout, menu event listeners, app kind map |
| `ui/src/components/skill/SkillView.tsx` | virtualized section list; root of the skill execution surface |
| `ui/src/components/skill/SkillSectionCard.tsx` | per-section card with run state, fidelity dot, inline approval |
| `ui/src/components/skill/SkillSectionApprovalOverlay.tsx` | inline approval overlay for SafetyScope::Skill events |
| `ui/src/components/skill/SkillPatchDiffPreview.tsx` | four-pane patch diff; calls apply_skill_patch on Allow |
| `ui/src/components/skill/RunWithValuesForm.tsx` | variable input form; entry point for run_skill and resumeSkillFromFailure |
| `ui/src/components/WalkthroughSaveSheet.tsx` | recording-stop save overlay; calls save_walkthrough_as_skill |
| `ui/src/components/skills/SkillsPanel.tsx` | left-rail procedural-skill index |
| `ui/src/components/skills/SkillDetailView.tsx` | selected skill metadata and refinement proposal surface |
| `ui/src/components/skills/SkillRefinementForm.tsx` | draft-skill proposal review form |
| `ui/src/store/useAppStore.ts` | composed Zustand store hook |
| `ui/src/store/slices/types.ts` | `StoreState` composition |
| `ui/src/store/slices/agentSlice.ts` | agent loop live state (status, steps, pending approval, skill creation intent) |
| `ui/src/store/slices/skillsSlice.ts` | skill buckets, selected skill, section run state, failed section tracking |
| `ui/src/store/slices/uiSlice.ts` | panel state, skillFrozen flag |
| `ui/src/store/slices/executionSlice.ts` | executor state, run_skill, resumeSkillFromFailure |
| `ui/src/store/slices/walkthroughSlice.ts` | walkthrough lifecycle state and CDP modal |
| `ui/src/hooks/useSafetyEventRouter.ts` | routes SafetyScope events to the correct UI surface |
| `ui/src/hooks/useExecutorEvents.ts` | thin event composer (delegates to events/*.ts hooks) |
| `ui/src/hooks/events/useAgentEvents.ts` | agent-loop event subscriber (agent://*) |
| `ui/src/store/settings.ts` | persisted settings I/O |
