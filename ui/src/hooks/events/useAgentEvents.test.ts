import { createElement } from "react";
import { act, cleanup, render } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { Node } from "../../bindings";

const eventMock = vi.hoisted(() => ({
  listeners: new Map<string, Set<(event: { payload: unknown }) => void>>(),
  listen: vi.fn(
    async (
      topic: string,
      handler: (event: { payload: unknown }) => void,
    ): Promise<() => void> => {
      const set = eventMock.listeners.get(topic) ?? new Set();
      set.add(handler);
      eventMock.listeners.set(topic, set);
      return () => set.delete(handler);
    },
  ),
}));

vi.mock("@tauri-apps/api/event", () => ({
  listen: eventMock.listen,
}));

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(),
}));

import { useStore } from "../../store/useAppStore";
import { isStaleRunId, useAgentEvents } from "./useAgentEvents";

function makeAgentNode(id: string, runId: string): Node {
  return {
    id,
    name: id,
    node_type: { type: "CdpWait", text: "Ready", timeout_ms: 1000 },
    position: { x: 0, y: 0 },
    enabled: true,
    timeout_ms: null,
    settle_ms: null,
    retries: 0,
    trace_level: "Minimal",
    role: "Default",
    expected_outcome: null,
    source_run_id: runId,
  };
}

function AgentEventsHarness() {
  useAgentEvents();
  return null;
}

async function mountSubscriptions() {
  render(createElement(AgentEventsHarness));
  await act(async () => {
    await Promise.resolve();
  });
}

function emit(topic: string, payload: unknown) {
  act(() => {
    for (const handler of eventMock.listeners.get(topic) ?? []) {
      handler({ payload });
    }
  });
}

describe("useAgentEvents trace subscriptions", () => {
  beforeEach(() => {
    eventMock.listeners.clear();
    eventMock.listen.mockClear();
    useStore.setState({
      agentRunId: "run-1",
      agentStatus: "running",
      agentSteps: [],
      runTraces: {},
      messages: [],
      completionDisagreement: null,
      agentError: null,
      pendingRunNodes: {},
      pendingRunEdges: {},
      workflow: {
        id: "00000000-0000-0000-0000-000000000001",
        name: "wf",
        nodes: [],
        edges: [],
        groups: [],
      },
    });
  });

  afterEach(() => {
    cleanup();
    eventMock.listeners.clear();
  });

  it("applies task-state, world-model, and boundary trace events", async () => {
    await mountSubscriptions();

    emit("agent://task_state_changed", {
      run_id: "run-1",
      task_state: {
        goal: "goal",
        phase: "executing",
        subgoal_stack: [
          {
            id: "subgoal-1",
            text: "Open settings",
            pushed_at_step: 0,
            parent: null,
          },
        ],
        watch_slots: [],
        hypotheses: [],
        milestones: [],
      },
    });
    emit("agent://world_model_changed", {
      run_id: "run-1",
      diff: { changed_fields: ["elements"] },
    });
    emit("agent://boundary_record_written", {
      run_id: "run-1",
      boundary_kind: "subgoal_completed",
      step_index: 1,
      milestone_text: "Settings opened",
    });

    const trace = useStore.getState().runTraces["run-1"];
    expect(trace.phase).toBe("executing");
    expect(trace.activeSubgoal).toBe("Open settings");
    expect(trace.worldModelDeltas).toEqual([
      { stepIndex: 0, changedFields: ["elements"] },
    ]);
    expect(trace.milestones).toEqual([
      {
        stepIndex: 1,
        kind: "subgoal_completed",
        text: "Settings opened",
      },
    ]);
  });

  it("pushes trace steps and terminal frames from step and terminal events", async () => {
    await mountSubscriptions();

    emit("agent://task_state_changed", {
      run_id: "run-1",
      task_state: {
        goal: "goal",
        phase: "recovering",
        subgoal_stack: [],
        watch_slots: [],
        hypotheses: [],
        milestones: [],
      },
    });
    emit("agent://step_failed", {
      run_id: "run-1",
      step_number: 2,
      tool_name: "cdp_click",
      error: "not found",
    });
    emit("agent://stopped", {
      run_id: "run-1",
      reason: "user_cancelled_disagreement",
    });

    const trace = useStore.getState().runTraces["run-1"];
    expect(trace.steps).toEqual([
      {
        stepIndex: 2,
        toolName: "cdp_click",
        phase: "recovering",
        body: "not found",
        failed: true,
      },
    ]);
    expect(trace.terminalFrame).toEqual({
      kind: "disagreement_cancelled",
      detail: "user cancelled after VLM disagreement",
    });
  });

  it("buffers node and edge events, then commits them on complete", async () => {
    await mountSubscriptions();

    const node = makeAgentNode("agent-node-1", "run-1");
    const edge = { from: "anchor", to: "agent-node-1" };

    emit("agent://node_added", {
      run_id: "run-1",
      node,
    });
    emit("agent://edge_added", {
      run_id: "run-1",
      edge,
    });

    expect(useStore.getState().workflow.nodes).toEqual([]);
    expect(useStore.getState().workflow.edges).toEqual([]);
    expect(useStore.getState().pendingRunNodes["run-1"]).toEqual([node]);
    expect(useStore.getState().pendingRunEdges["run-1"]).toEqual([edge]);

    emit("agent://complete", {
      run_id: "run-1",
      summary: "Done",
    });

    const state = useStore.getState();
    expect(state.workflow.nodes).toEqual([node]);
    expect(state.workflow.edges).toEqual([edge]);
    expect(state.pendingRunNodes["run-1"]).toBeUndefined();
    expect(state.pendingRunEdges["run-1"]).toBeUndefined();
    expect(state.runTraces["run-1"].terminalFrame).toEqual({
      kind: "complete",
      detail: "Done",
    });
  });

  it("drops buffered canvas changes on stopped, error, and destructive-cap terminal paths", async () => {
    await mountSubscriptions();

    const stoppedNode = makeAgentNode("stopped-node", "run-1");
    useStore.getState().bufferAgentNode("run-1", stoppedNode);
    emit("agent://stopped", {
      run_id: "run-1",
      reason: "user_stopped",
    });
    expect(useStore.getState().workflow.nodes).toEqual([]);
    expect(useStore.getState().pendingRunNodes["run-1"]).toBeUndefined();

    useStore.setState({
      agentRunId: "run-2",
      agentStatus: "running",
      pendingRunNodes: {},
      pendingRunEdges: {},
    });
    const errorNode = makeAgentNode("error-node", "run-2");
    useStore.getState().bufferAgentNode("run-2", errorNode);
    emit("agent://error", {
      run_id: "run-2",
      message: "boom",
    });
    expect(useStore.getState().workflow.nodes).toEqual([]);
    expect(useStore.getState().pendingRunNodes["run-2"]).toBeUndefined();

    useStore.setState({
      agentRunId: "run-3",
      agentStatus: "running",
      pendingRunNodes: {},
      pendingRunEdges: {},
    });
    const capNode = makeAgentNode("cap-node", "run-3");
    useStore.getState().bufferAgentNode("run-3", capNode);
    emit("agent://consecutive_destructive_cap_hit", {
      run_id: "run-3",
      recent_tool_names: ["delete_file"],
      cap: 1,
    });
    expect(useStore.getState().workflow.nodes).toEqual([]);
    expect(useStore.getState().pendingRunNodes["run-3"]).toBeUndefined();
  });
});

describe("terminal events stamp agentRunFinishedAt (D24)", () => {
  beforeEach(() => {
    eventMock.listeners.clear();
    eventMock.listen.mockClear();
    useStore.setState({
      agentRunId: "run-1",
      agentStatus: "running",
      agentSteps: [],
      runTraces: {},
      messages: [],
      completionDisagreement: null,
      agentError: null,
      pendingRunNodes: {},
      pendingRunEdges: {},
      agentRunStartedAt: 1_000,
      agentRunFinishedAt: null,
      workflow: {
        id: "00000000-0000-0000-0000-000000000001",
        name: "wf",
        nodes: [],
        edges: [],
        groups: [],
      },
    });
  });

  afterEach(() => {
    cleanup();
    eventMock.listeners.clear();
  });

  it("agent://complete stamps agentRunFinishedAt", async () => {
    await mountSubscriptions();
    emit("agent://complete", { run_id: "run-1", summary: "Done" });
    expect(useStore.getState().agentRunFinishedAt).not.toBeNull();
  });

  it("agent://stopped stamps agentRunFinishedAt", async () => {
    await mountSubscriptions();
    emit("agent://stopped", { run_id: "run-1", reason: "user_stopped" });
    expect(useStore.getState().agentRunFinishedAt).not.toBeNull();
  });

  it("agent://error stamps agentRunFinishedAt even when status is not running", async () => {
    // Racing-error-after-stop: status was already flipped, but the
    // freeze should still reflect the last terminal moment.
    useStore.setState({ agentStatus: "stopped" });
    await mountSubscriptions();
    emit("agent://error", { run_id: "run-1", message: "boom" });
    expect(useStore.getState().agentRunFinishedAt).not.toBeNull();
  });
});

describe("isStaleRunId", () => {
  it("treats a null active run as stale so events during stop/restart are dropped", () => {
    expect(isStaleRunId(null, "run-a")).toBe(true);
  });

  it("rejects events whose run_id does not match the active run", () => {
    expect(isStaleRunId("run-b", "run-a")).toBe(true);
  });

  it("accepts events whose run_id matches the active run", () => {
    expect(isStaleRunId("run-b", "run-b")).toBe(false);
  });
});
