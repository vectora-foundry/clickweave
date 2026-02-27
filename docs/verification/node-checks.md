# Verification Nodes

Nodes with `role: Verification` produce inline verdicts during workflow execution. A verification failure stops the workflow immediately (fail-fast).

## How It Works

### 1. Marking a Node as Verification

In the Node Detail Modal's **Setup** tab, eligible node types show a **Verification** toggle. Only read-only node types can be verification nodes:

- `FindText`
- `FindImage`
- `ListWindows`
- `TakeScreenshot`

When enabled, the node's `role` field is set to `NodeRole::Verification` (stored in `workflow.rs`).

### 2. Verdict Evaluation (Inline During Execution)

After a Verification-role node executes successfully, a verdict is produced immediately — not in a post-run batch pass. Evaluation depends on the node type:

**Deterministic verdicts** (FindText, FindImage, ListWindows):

The MCP tool result is inspected directly. A non-empty array result means pass; an empty array means fail.

- `FindText` — checks `TextPresent` (are matches found?)
- `FindImage` — checks `TemplateFound` (are matches found?)
- `ListWindows` — checks `WindowTitleMatches` (are windows found?)

No LLM is involved. The verdict is derived purely from the result data.

**VLM-based verdicts** (TakeScreenshot):

Requires `expected_outcome` text on the node (set in the Setup tab). The VLM receives the captured screenshot and the expected outcome description, then returns a pass/fail verdict with reasoning.

If a TakeScreenshot node has `Verification` role but no `expected_outcome`, a `Warn` verdict is produced instead of fail.

### 3. Fail-Fast Behavior

If any verification verdict is `Fail`, execution stops immediately. The failing node is marked as failed and no subsequent nodes run. This differs from the old post-run check system which evaluated all checks after the workflow completed.

### 4. Results

- Accumulated in `runtime_verdicts` on the executor during the graph walk
- Persisted to `verdict.json` in each node's run directory after the graph walk
- Emitted as `executor://checks_completed` event to the frontend
- Displayed in the **VerdictBar** at the top of the app:
  - Green: PASSED (all verdicts pass)
  - Yellow: PASSED with warnings (e.g. missing `expected_outcome`)
  - Red: FAILED (at least one verdict failed)
  - Expandable to show per-node breakdowns with individual verdicts and reasoning

## Key Types

Defined in `crates/clickweave-core/src/workflow.rs`:

| Type | Purpose |
|------|---------|
| `NodeRole` | `Default` or `Verification` — set on each node |
| `CheckType` | `TextPresent`, `TemplateFound`, `WindowTitleMatches`, `ScreenshotMatch` |
| `CheckVerdict` | `Pass`, `Fail`, `Warn` |
| `CheckResult` | Individual check result: `check_name`, `check_type`, `verdict`, `reasoning` |
| `NodeVerdict` | Per-node verdict: `node_id`, `node_name`, `check_results`, `expected_outcome_verdict` |

## Key Files

| File | Role |
|------|------|
| `crates/clickweave-engine/src/executor/verdict.rs` | Deterministic verdict logic, VLM screenshot verdict, missing-outcome warning |
| `crates/clickweave-engine/src/executor/run_loop.rs` | Inline verdict evaluation after node execution, fail-fast logic, verdict accumulation and emission |
| `crates/clickweave-core/src/workflow.rs` | Core types: `NodeRole`, `CheckType`, `CheckVerdict`, `CheckResult`, `NodeVerdict` |
| `crates/clickweave-core/src/storage.rs` | `save_node_verdict()` — persists `verdict.json` per node |
| `ui/src/components/VerdictBar.tsx` | Verdict display with expandable per-node details |
| `ui/src/components/node-detail/tabs/SetupTab.tsx` | Verification toggle for eligible node types |
| `ui/src/store/slices/verdictSlice.ts` | Zustand state for verdicts |
