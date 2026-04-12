# Workflow Execution (Conceptual)

Execution is a linear walk of tool-call nodes with guardrails, plus a live observe-act agent loop that can build and run workflows from a natural-language goal.

## How to Think About It

- Saved workflows are linear sequences of tool-call nodes; the executor advances node by node.
- The agent loop is the primary LLM-driven path — it observes the app, asks an LLM for one tool call, dispatches it, and appends the call to the workflow as a persisted node.
- Every meaningful step emits traceable evidence.

## Two Execution Paths

1. **Deterministic executor:** replays a saved workflow as a linear sequence of tool invocations. Each node maps to a concrete tool operation (click, type, launch app, take screenshot, etc.).
2. **Agent loop:** a goal-driven observe-act loop. The user types a natural-language goal; the agent LLM decides one tool call per step based on the current app state, dispatches it, and materializes the call as a new node on the workflow. No graph branching, no If/Switch/Loop — just a linear transcript.

## Runtime Modes: Test and Run

The executor has two runtime modes, chosen before execution begins:

- **Test** -- the interactive authoring mode. The executor runs each node, then verifies its effect through per-step supervision (see below). LLM decisions (element disambiguation, app resolution) are recorded into a decision cache so they can be replayed later. The decision cache is saved to disk when the workflow completes.
- **Run** -- the headless replay mode. Supervision is skipped. Previously cached LLM decisions are replayed deterministically, so elements and apps resolve the same way they did during the Test run without repeating LLM calls. If a cached decision no longer matches the live UI (e.g., the resolved element name is missing from the accessibility tree), the executor falls through to the LLM for a fresh resolution.

This Test-then-Run workflow means a workflow is authored once with human oversight, then executed repeatedly without it.

## Per-Step Supervision (Test Mode)

In Test mode, every action node is verified immediately after execution:

1. **Screenshot** -- the executor waits for UI to settle, then captures a window screenshot of the focused app (retrying up to 3 times if the window is not yet ready).
2. **VLM description** -- a vision-language model describes what the screen shows relative to the action that just ran. If no VLM is configured, this step is skipped and the judge works from the action trace alone.
3. **Supervisor judge** -- the supervisor LLM receives the fast-VLM description along with the full conversation history of prior steps and returns a pass/fail verdict with reasoning. The supervisor and fast-VLM endpoints are two of the three LLMs the user configures (alongside the agent LLM); both fall back to the agent endpoint if not separately configured.

Read-only nodes (TakeScreenshot, FindText, FindImage) skip supervision entirely, since their results are data, not UI-changing actions.

If the step passes, execution continues. If it fails, the executor pauses and presents the finding (plus screenshot) to the user, who can choose:

- **Retry** -- re-execute the node from scratch.
- **Skip** -- accept the current state and move on.
- **Abort** -- stop the workflow.

The supervision conversation history is persistent across the entire run, so the judge accumulates context about what the workflow has done so far.

## Focused App Tracking

The executor tracks which application is currently in focus. When a `launch_app` or `focus_window` (by app name) action runs, the resolved app name is stored as the focused app. This scoping is used throughout execution:

- **Screenshots** are captured as window-scoped screenshots of the focused app rather than full-screen captures.
- **find_text** and **click** operations use the focused app to scope accessibility queries, avoiding false matches from other windows.
- **Supervision** screenshots target the focused app window for accurate verification.

## Settle Delay

Each node has an optional `settle_ms` field. After a node executes successfully, the executor sleeps for this duration before finalizing the run and following the next edge. This gives the target application time to finish animations or state transitions before subsequent nodes act on the UI.

## Reliability Principles

- Retry failed nodes a bounded number of times.
- Evict resolution caches for the specific node being retried (app name, element name), not the entire cache -- other nodes' cached resolutions remain valid.
- Capture traces and artifacts per run so failures are diagnosable.
- In the agent loop, bound runs by max steps and max consecutive errors; compact the transcript (including snapshot supersession) so the prompt stays well under the LLM context window.

## Inline Verification Verdicts

Nodes can be marked with a Verification role (available on read-only node types: FindText, FindImage, TakeScreenshot). Verification verdicts are evaluated inline during the walk, immediately after the node executes -- not in a separate post-run pass.

**Deterministic verdicts** (FindText, FindImage): the MCP tool result is inspected directly. A non-empty result means pass; an empty result means fail. No LLM is involved.

**VLM-based verdicts** (TakeScreenshot): the captured screenshot is sent to the VLM along with the node's `expected_outcome` text. The VLM returns a pass/fail verdict with reasoning.

If any verification verdict is Fail, execution stops immediately (fail-fast). No subsequent nodes run. This gives workflows built-in test assertions that catch failures at the earliest possible point.

This is distinct from per-step supervision -- supervision verifies that each step took effect; verification nodes assert that the workflow produced the right business-level outcomes.

For exact runtime behavior and file-level references, see `docs/reference/engine/execution.md`.
