# Frontend Architecture (Conceptual)

The frontend is a workflow editor plus execution cockpit.

![Frontend & Tauri IPC](frontend-ipc.drawio.png)

## Primary UX Surfaces

- **Graph canvas** for building and wiring workflow nodes. Nodes are added via a collapsible node palette on the left (not drag-and-drop). Loop groups can be collapsed/expanded. During execution, the currently-running node is highlighted (`activeNode`), distinct from the user-selected node.
- **Node detail modal** for setup and trace inspection (3 tabs: Setup, Trace, Runs).
- **Assistant panel** for conversational edits and patch proposals.
- **Walkthrough panel** for reviewing and annotating a recorded walkthrough draft. Users can rename, delete, and annotate steps before applying the draft to the canvas.
- **Recording bar** -- a global overlay window that shows recording controls (pause, resume, stop, cancel) during walkthrough capture.
- **Verdict bar and modal** for displaying inline verification results. The bar shows pass/fail/warn status at the top of the app; the modal expands to show per-node breakdowns with individual verdicts and reasoning.
- **Run/log surfaces** for execution feedback.
- **Supervision modal** for human-in-the-loop review during Test runs. When a step fails verification the engine pauses and the modal shows the node name, a finding description, and an optional screenshot. The user can retry the step, skip past it, or abort the entire run.
- **Intent empty state** -- when a new project has no nodes, an onboarding screen prompts the user to describe their intent, which triggers assistant-based planning.

## Execution Modes

Workflows can be launched in two modes, selectable from the toolbar:

- **Test** -- the engine verifies each step after it executes by taking a screenshot and evaluating the result. If verification fails the supervision modal pauses the run for human review. On completion the engine saves a decision cache so subsequent runs can replay known-good choices faster.
- **Run** -- the engine executes steps without per-step supervision, running straight through to completion. This is the production-like mode used once a workflow has been verified in Test mode.

The current mode is stored in execution state and sent to the backend as part of the run request, so the frontend never needs to know the details of what the engine skips -- it simply reacts to whichever events the backend emits.

## State Philosophy

A single Zustand store composed from 9 slices keeps cross-feature coordination simple:

- project/workflow editing (ProjectSlice),
- execution state -- run status, current mode, supervision pause (ExecutionSlice),
- undo/redo history -- up to 50 snapshots in each direction via `structuredClone` (HistorySlice),
- assistant conversation and pending patches (AssistantSlice),
- settings -- persisted to disk via `tauri-plugin-store` (SettingsSlice),
- logs and verdicts (LogSlice, VerdictSlice),
- walkthrough recording -- session state, events, draft, annotations, recording bar lifecycle, CDP app selection modal (WalkthroughSlice),
- UI chrome/selection state (UiSlice).

All workflow mutations (add/remove nodes, connect edges, update positions) go through `useWorkflowMutations`, which automatically pushes undo history on each change.

## Event-Driven Runtime UX

Backend events stream into the store, and UI updates are derived from state rather than direct imperative DOM updates.

## Why This Matters

- Graph editing stays responsive while execution runs.
- Assistant changes are staged as patches before apply.
- Trace/artifact views make failures debuggable without leaving the app.

For exact file/component/state contracts, see `docs/reference/frontend/architecture.md`.
