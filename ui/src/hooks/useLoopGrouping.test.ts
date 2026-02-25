import { describe, it, expect } from "vitest";
import { renderHook, act } from "@testing-library/react";
import { useLoopGrouping } from "./useLoopGrouping";
import type { Node, Edge, Workflow } from "../bindings";

function node(id: string, type: string, params?: Record<string, unknown>): Node {
  return {
    id,
    node_type: { type, ...params } as Node["node_type"],
    position: { x: 0, y: 0 },
    name: id,
    enabled: true,
    timeout_ms: null,
    settle_ms: null,
    retries: 0,
    trace_level: "Full",
    expected_outcome: null,
    checks: [],
  };
}

function edge(from: string, to: string, output?: Edge["output"]): Edge {
  return { from, to, output: output ?? null };
}

function makeWorkflow(nodes: Node[], edges: Edge[]): Workflow {
  return { id: "test-id", name: "test", nodes, edges };
}

describe("useLoopGrouping", () => {
  it("hiddenNodeIds contains EndLoop IDs", () => {
    const wf = makeWorkflow(
      [
        node("loop1", "Loop", { exit_condition: { type: "Always" }, max_iterations: 3 }),
        node("a", "AiStep"),
        node("end1", "EndLoop", { loop_id: "loop1" }),
      ],
      [
        edge("loop1", "a", { type: "LoopBody" }),
        edge("a", "end1"),
        edge("end1", "loop1"),
      ],
    );
    const { result } = renderHook(() => useLoopGrouping(wf));
    expect(result.current.hiddenNodeIds.has("end1")).toBe(true);
  });

  it("hiddenNodeIds contains body nodes of collapsed loops", () => {
    const wf = makeWorkflow(
      [
        node("loop1", "Loop", { exit_condition: { type: "Always" }, max_iterations: 3 }),
        node("a", "AiStep"),
        node("end1", "EndLoop", { loop_id: "loop1" }),
      ],
      [
        edge("loop1", "a", { type: "LoopBody" }),
        edge("a", "end1"),
        edge("end1", "loop1"),
      ],
    );
    const { result } = renderHook(() => useLoopGrouping(wf));
    // New loops auto-collapse — body node "a" should be hidden
    expect(result.current.hiddenNodeIds.has("a")).toBe(true);
  });

  it("toggleLoopCollapse toggles a loop in/out of collapsed set", () => {
    const wf = makeWorkflow(
      [
        node("loop1", "Loop", { exit_condition: { type: "Always" }, max_iterations: 3 }),
        node("a", "AiStep"),
        node("end1", "EndLoop", { loop_id: "loop1" }),
      ],
      [
        edge("loop1", "a", { type: "LoopBody" }),
        edge("a", "end1"),
        edge("end1", "loop1"),
      ],
    );
    const { result } = renderHook(() => useLoopGrouping(wf));
    // loop1 starts collapsed (auto-collapse)
    expect(result.current.collapsedLoops.has("loop1")).toBe(true);

    act(() => result.current.toggleLoopCollapse("loop1"));
    expect(result.current.collapsedLoops.has("loop1")).toBe(false);
    expect(result.current.hiddenNodeIds.has("a")).toBe(false);

    act(() => result.current.toggleLoopCollapse("loop1"));
    expect(result.current.collapsedLoops.has("loop1")).toBe(true);
    expect(result.current.hiddenNodeIds.has("a")).toBe(true);
  });

  it("new loops auto-collapse on discovery", () => {
    const wf1 = makeWorkflow([], []);
    const { result, rerender } = renderHook(
      ({ wf }) => useLoopGrouping(wf),
      { initialProps: { wf: wf1 } },
    );
    expect(result.current.collapsedLoops.size).toBe(0);

    const wf2 = makeWorkflow(
      [
        node("loop1", "Loop", { exit_condition: { type: "Always" }, max_iterations: 3 }),
        node("a", "AiStep"),
        node("end1", "EndLoop", { loop_id: "loop1" }),
      ],
      [
        edge("loop1", "a", { type: "LoopBody" }),
        edge("a", "end1"),
        edge("end1", "loop1"),
      ],
    );
    rerender({ wf: wf2 });
    expect(result.current.collapsedLoops.has("loop1")).toBe(true);
  });

  it("removed loops are cleaned from collapsed set", () => {
    const wf1 = makeWorkflow(
      [
        node("loop1", "Loop", { exit_condition: { type: "Always" }, max_iterations: 3 }),
        node("end1", "EndLoop", { loop_id: "loop1" }),
      ],
      [
        edge("loop1", "end1", { type: "LoopBody" }),
        edge("end1", "loop1"),
      ],
    );
    const { result, rerender } = renderHook(
      ({ wf }) => useLoopGrouping(wf),
      { initialProps: { wf: wf1 } },
    );
    expect(result.current.collapsedLoops.has("loop1")).toBe(true);

    const wf2 = makeWorkflow([], []);
    rerender({ wf: wf2 });
    expect(result.current.collapsedLoops.has("loop1")).toBe(false);
  });

  it("endLoopForLoop maps loop ID to EndLoop node ID", () => {
    const wf = makeWorkflow(
      [
        node("loop1", "Loop", { exit_condition: { type: "Always" }, max_iterations: 3 }),
        node("end1", "EndLoop", { loop_id: "loop1" }),
      ],
      [
        edge("loop1", "end1", { type: "LoopBody" }),
        edge("end1", "loop1"),
      ],
    );
    const { result } = renderHook(() => useLoopGrouping(wf));
    expect(result.current.endLoopForLoop.get("loop1")).toBe("end1");
  });
});
