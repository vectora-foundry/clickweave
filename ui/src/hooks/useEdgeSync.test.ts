import { describe, it, expect, vi } from "vitest";
import { renderHook } from "@testing-library/react";
import { useEdgeSync } from "./useEdgeSync";
import type { Workflow } from "../bindings";
import { useRef } from "react";
import { node, edge, makeWorkflow } from "./test-helpers";

function renderEdgeSync(params: {
  workflow: Workflow;
  hiddenNodeIds?: Set<string>;
  collapsedLoops?: Set<string>;
}) {
  const { workflow, hiddenNodeIds = new Set(), collapsedLoops = new Set() } = params;
  return renderHook(() => {
    const deletedNodeIdsRef = useRef<Set<string> | null>(null);
    return useEdgeSync({
      workflow,
      hiddenNodeIds,
      collapsedLoops,
      deletedNodeIdsRef,
      onEdgesChange: vi.fn(),
      onRemoveExtraEdges: vi.fn(),
      onConnect: vi.fn(),
    });
  });
}

describe("useEdgeSync", () => {
  it("filters edges to hidden nodes", () => {
    const wf = makeWorkflow(
      [node("a", "AiStep"), node("b", "Click"), node("c", "AiStep")],
      [edge("a", "b"), edge("b", "c")],
    );
    const { result } = renderEdgeSync({
      workflow: wf,
      hiddenNodeIds: new Set(["b"]),
    });
    expect(result.current.rfEdges).toHaveLength(0);
  });

  it("filters LoopBody edges when loop is collapsed", () => {
    const wf = makeWorkflow(
      [
        node("loop1", "Loop", { exit_condition: { type: "Always" }, max_iterations: 3 }),
        node("a", "AiStep"),
        node("done", "AiStep"),
      ],
      [
        edge("loop1", "a", { type: "LoopBody" }),
        edge("loop1", "done", { type: "LoopDone" }),
      ],
    );
    const { result } = renderEdgeSync({
      workflow: wf,
      collapsedLoops: new Set(["loop1"]),
    });
    // LoopBody edge filtered, LoopDone edge kept
    expect(result.current.rfEdges).toHaveLength(1);
    expect(result.current.rfEdges[0].label).toBe("done");
  });

  it("generates edge IDs in from-to-handle format", () => {
    const wf = makeWorkflow(
      [node("a", "AiStep"), node("b", "Click")],
      [edge("a", "b")],
    );
    const { result } = renderEdgeSync({ workflow: wf });
    expect(result.current.rfEdges[0].id).toBe("a-b-default");
  });

  it("derives edge labels from output type", () => {
    const wf = makeWorkflow(
      [
        node("if1", "If", { condition: { type: "Always" } }),
        node("a", "AiStep"),
        node("b", "Click"),
      ],
      [
        edge("if1", "a", { type: "IfTrue" }),
        edge("if1", "b", { type: "IfFalse" }),
      ],
    );
    const { result } = renderEdgeSync({ workflow: wf });
    const labels = result.current.rfEdges.map((e) => e.label);
    expect(labels).toContain("true");
    expect(labels).toContain("false");
  });

  it("includes SwitchCase name as label", () => {
    const wf = makeWorkflow(
      [
        node("sw1", "Switch", { cases: [{ name: "foo" }] }),
        node("a", "AiStep"),
      ],
      [edge("sw1", "a", { type: "SwitchCase", name: "foo" })],
    );
    const { result } = renderEdgeSync({ workflow: wf });
    expect(result.current.rfEdges[0].label).toBe("foo");
  });
});
