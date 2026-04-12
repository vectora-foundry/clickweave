# Architecture Overview (Conceptual)

Clickweave has one core idea: drive desktop automation through a live observe-act agent loop, and persist what the agent does as a linear, replayable sequence of tool calls.

## Mental Model

1. A user starts with a natural-language goal.
2. The agent loop runs against the target app: observe the screen, ask the LLM for one tool call, dispatch it via MCP, append the call as a node on the workflow canvas. Repeat until the goal is done or the agent asks to replan.
3. Alternatively, the user records a walkthrough; OS-level events are normalized and synthesized into a draft workflow that can be reviewed before applying to the canvas.
4. The user can re-run the saved workflow deterministically. Verification nodes produce inline pass/fail verdicts during the walk (fail-fast). In Test mode, each step is also verified by a supervision loop that can pause execution and let the user retry, skip, or abort. In Run mode, execution proceeds without supervision, replaying decisions recorded during Test.
5. Results, traces, verdicts, and artifacts feed back into iteration.

## Layered System

- Core model layer: workflow node/edge types and validation rules. Workflows are linear sequences of tool calls -- there are no control-flow nodes.
- Agent layer: the observe-act loop that drives the agent LLM against MCP tools, builds the workflow live, and handles approval gating and context compaction (including snapshot supersession).
- Walkthrough layer: records user actions via OS-level event capture, normalizes and synthesizes them into a draft workflow, and seeds a decision cache for replay.
- Execution layer: deterministic replay of saved tool-call sequences.
- Verification layer: nodes marked as Verification produce inline verdicts during execution. Deterministic checks (FindText, FindImage) inspect results directly; VLM-based checks (TakeScreenshot) evaluate screenshots against expected outcomes. A failed verdict stops the walk immediately (fail-fast).
- Supervision layer: after each step, the fast VLM captures the screen state and the supervisor LLM judges whether the step succeeded. On failure, execution pauses and the user can retry, skip, or abort. Active in Test mode only.
- Integration layer: MCP bridge to external automation tools (`native-devtools-mcp`, `chrome-devtools-mcp`).
- UI layer: canvas view + agent cockpit + run/trace/walkthrough review/verdict display.

## Why This Split Exists

- Letting the agent dispatch MCP tools directly keeps authoring and execution on the same surface -- what the agent does is exactly what gets saved as a workflow.
- Deterministic replay of saved workflows makes runs inspectable and cheap. Because some tool calls involve LLM-driven resolution (element disambiguation, app name matching), true replay depends on the decision cache recording those choices during Test and replaying them in Run.
- Verification nodes produce inline verdicts that stop execution immediately on failure, giving workflows built-in test assertions.
- Walkthrough recording provides an alternative authoring path: demonstrate the task, then review and refine the generated graph.
- Trace + verdicts create a feedback loop for reliability.

## Reliability Strategy

The system assumes LLM output and runtime environments are imperfect, so it uses:

- bounded agent runs with max-steps and max-consecutive-errors caps,
- transcript compaction and snapshot supersession so the agent prompt stays well under the LLM context window,
- approval gating (pre-approved tool categories or step-by-step approval) that lets the user intervene before irreversible actions,
- runtime retries with targeted cache eviction on retry (per-node, not whole cache),
- inline verification verdicts from Verification-role nodes that fail-fast on assertion failure,
- step-level supervision in Test mode, where a fast VLM + supervisor LLM verify each step's outcome and pause for human judgment on failure,
- a decision cache that records LLM-driven choices during Test, then replays them deterministically in Run,
- walkthrough recording as an alternative authoring path that captures real interactions and seeds the decision cache with observed choices,
- persisted traces and artifacts for diagnosis.

The two execution modes reflect this strategy: Test mode is interactive, with supervision and decision recording; Run mode is hands-off, replaying cached decisions without supervision overhead.

## What Humans Should Keep in Mind

- A workflow is a linear sequence of tool calls -- no branching, no loops encoded in the graph.
- Authoring by agent and authoring by hand (or by walkthrough) produce the same shape of artifact.
- "Success" is not only node completion; verification verdicts and supervision judgments matter.

For code-coupled details, see `docs/reference/architecture/overview.md`.
