import { describe, it, expect, vi } from "vitest";
import { renderHook } from "@testing-library/react";
import { useEdgeSync } from "./useEdgeSync";
import type { Workflow } from "../bindings";
import { useRef } from "react";
import { node, edge, makeWorkflow } from "./test-helpers";

function renderEdgeSync(params: {
  workflow: Workflow;
  hiddenNodeIds?: Set<string>;
  collapsedAppEdgeRewrites?: Map<string, string>;
  collapsedUserGroupEdgeRewrites?: Map<string, string>;
}) {
  const {
    workflow,
    hiddenNodeIds = new Set(),
    collapsedAppEdgeRewrites = new Map(),
    collapsedUserGroupEdgeRewrites = new Map(),
  } = params;
  return renderHook(() => {
    const deletedNodeIdsRef = useRef<Set<string> | null>(null);
    return useEdgeSync({
      workflow,
      hiddenNodeIds,
      collapsedAppEdgeRewrites,
      collapsedUserGroupEdgeRewrites,
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

  it("always filters LoopBody edges, keeps LoopDone", () => {
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
    const { result } = renderEdgeSync({ workflow: wf });
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

describe("useEdgeSync — app group edge rewriting", () => {
  it("rewrites edges from collapsed group members to anchor", () => {
    const wf = makeWorkflow(
      [
        node("fw1", "FocusWindow", { method: "AppName", value: "Discord", bring_to_front: true }),
        node("c1", "Click"),
        node("outside", "AiStep"),
      ],
      [edge("fw1", "c1"), edge("c1", "outside")],
    );
    const rewrites = new Map([["fw1", "fw1"], ["c1", "fw1"]]);
    const { result } = renderEdgeSync({
      workflow: wf,
      collapsedAppEdgeRewrites: rewrites,
    });
    expect(result.current.rfEdges).toHaveLength(1);
    expect(result.current.rfEdges[0].source).toBe("fw1");
    expect(result.current.rfEdges[0].target).toBe("outside");
  });

  it("filters internal edges within collapsed group", () => {
    const wf = makeWorkflow(
      [
        node("fw1", "FocusWindow", { method: "AppName", value: "Discord", bring_to_front: true }),
        node("c1", "Click"),
        node("t1", "TypeText", { text: "hi" }),
      ],
      [edge("fw1", "c1"), edge("c1", "t1")],
    );
    const rewrites = new Map([["fw1", "fw1"], ["c1", "fw1"], ["t1", "fw1"]]);
    const { result } = renderEdgeSync({
      workflow: wf,
      collapsedAppEdgeRewrites: rewrites,
    });
    expect(result.current.rfEdges).toHaveLength(0);
  });

  it("rewrites incoming edges to collapsed group", () => {
    const wf = makeWorkflow(
      [
        node("outside", "AiStep"),
        node("fw1", "FocusWindow", { method: "AppName", value: "Discord", bring_to_front: true }),
        node("c1", "Click"),
      ],
      [edge("outside", "fw1"), edge("fw1", "c1")],
    );
    const rewrites = new Map([["fw1", "fw1"], ["c1", "fw1"]]);
    const { result } = renderEdgeSync({
      workflow: wf,
      collapsedAppEdgeRewrites: rewrites,
    });
    expect(result.current.rfEdges).toHaveLength(1);
    expect(result.current.rfEdges[0].source).toBe("outside");
    expect(result.current.rfEdges[0].target).toBe("fw1");
  });

  it("deduplicates edges after rewriting", () => {
    const wf = makeWorkflow(
      [
        node("fw1", "FocusWindow", { method: "AppName", value: "Discord", bring_to_front: true }),
        node("c1", "Click"),
        node("t1", "TypeText", { text: "hi" }),
        node("outside", "AiStep"),
      ],
      [edge("fw1", "c1"), edge("c1", "t1"), edge("c1", "outside"), edge("t1", "outside")],
    );
    const rewrites = new Map([["fw1", "fw1"], ["c1", "fw1"], ["t1", "fw1"]]);
    const { result } = renderEdgeSync({
      workflow: wf,
      collapsedAppEdgeRewrites: rewrites,
    });
    expect(result.current.rfEdges).toHaveLength(1);
    expect(result.current.rfEdges[0].source).toBe("fw1");
    expect(result.current.rfEdges[0].target).toBe("outside");
  });

  it("no rewriting when map is empty", () => {
    const wf = makeWorkflow(
      [node("a", "AiStep"), node("b", "Click")],
      [edge("a", "b")],
    );
    const { result } = renderEdgeSync({
      workflow: wf,
      collapsedAppEdgeRewrites: new Map(),
    });
    expect(result.current.rfEdges).toHaveLength(1);
    expect(result.current.rfEdges[0].source).toBe("a");
    expect(result.current.rfEdges[0].target).toBe("b");
  });
});
