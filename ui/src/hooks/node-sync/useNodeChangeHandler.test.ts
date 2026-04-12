import { describe, it, expect, vi } from "vitest";
import { renderHook } from "@testing-library/react";
import { useRef, useState } from "react";
import type { Node as RFNode, NodeChange } from "@xyflow/react";
import { useNodeChangeHandler } from "./useNodeChangeHandler";
import { makeWorkflow, node } from "../test-helpers";

function workflowNode(id: string, selected = false): RFNode {
  return {
    id,
    type: "workflow",
    position: { x: 0, y: 0 },
    data: {},
    selected,
  };
}

function renderHandler(initialRfNodes: RFNode[]) {
  const onSelectNode = vi.fn<(id: string | null) => void>();
  const onNodePositionsChange = vi.fn();
  const onDeleteNodes = vi.fn();

  const wf = makeWorkflow(
    initialRfNodes.map((n) => node(n.id, "Click")),
    [],
  );

  const hook = renderHook(() => {
    const [rfNodes, setRfNodes] = useState<RFNode[]>(initialRfNodes);
    const selectionFromCanvasRef = useRef(false);
    const deletedNodeIdsRef = useRef<Set<string> | null>(null);
    const handler = useNodeChangeHandler({
      workflow: wf,
      collapsedApps: new Set(),
      appGroups: new Map(),
      nodeToAppGroup: new Map(),
      appGroupMeta: new Map(),
      collapsedUserGroups: new Set(),
      nodeToUserGroup: new Map(),
      userGroupMeta: new Map(),
      selectionFromCanvasRef,
      deletedNodeIdsRef,
      onSelectNode,
      onNodePositionsChange,
      onDeleteNodes,
      setRfNodes,
    });
    return { handler, rfNodes, selectionFromCanvasRef };
  });

  return { hook, onSelectNode, onNodePositionsChange, onDeleteNodes };
}

// queueMicrotask schedules onSelectNode after the setState updater returns;
// flushPromises forces microtasks to drain so assertions can run afterward.
const flushMicrotasks = () => new Promise<void>((resolve) => queueMicrotask(resolve));

describe("useNodeChangeHandler — selection resolution", () => {
  it("selects the node when a single node becomes selected", async () => {
    const { hook, onSelectNode } = renderHandler([
      workflowNode("a"),
      workflowNode("b"),
    ]);

    const changes: NodeChange[] = [{ type: "select", id: "a", selected: true }];
    hook.result.current.handler(changes);
    await flushMicrotasks();

    expect(onSelectNode).toHaveBeenCalledTimes(1);
    expect(onSelectNode).toHaveBeenCalledWith("a");
  });

  it("clears selection when multiple nodes become selected (drag-selection box)", async () => {
    const { hook, onSelectNode } = renderHandler([
      workflowNode("a"),
      workflowNode("b"),
      workflowNode("c"),
    ]);

    const changes: NodeChange[] = [
      { type: "select", id: "a", selected: true },
      { type: "select", id: "b", selected: true },
      { type: "select", id: "c", selected: true },
    ];
    hook.result.current.handler(changes);
    await flushMicrotasks();

    expect(onSelectNode).toHaveBeenCalledTimes(1);
    expect(onSelectNode).toHaveBeenCalledWith(null);
  });

  it("clears selection when shift-click extends an existing selection", async () => {
    const { hook, onSelectNode } = renderHandler([
      workflowNode("a", true),
      workflowNode("b"),
    ]);

    // Shift-click on "b" adds it to the selection without deselecting "a".
    const changes: NodeChange[] = [{ type: "select", id: "b", selected: true }];
    hook.result.current.handler(changes);
    await flushMicrotasks();

    expect(onSelectNode).toHaveBeenCalledTimes(1);
    expect(onSelectNode).toHaveBeenCalledWith(null);
  });

  it("clears selection when all nodes are deselected", async () => {
    const { hook, onSelectNode } = renderHandler([
      workflowNode("a", true),
    ]);

    const changes: NodeChange[] = [{ type: "select", id: "a", selected: false }];
    hook.result.current.handler(changes);
    await flushMicrotasks();

    expect(onSelectNode).toHaveBeenCalledTimes(1);
    expect(onSelectNode).toHaveBeenCalledWith(null);
  });

  it("does not fire onSelectNode for pure position changes", async () => {
    const { hook, onSelectNode, onNodePositionsChange } = renderHandler([
      workflowNode("a"),
    ]);

    const changes: NodeChange[] = [
      { type: "position", id: "a", position: { x: 100, y: 200 } },
    ];
    hook.result.current.handler(changes);
    await flushMicrotasks();

    expect(onSelectNode).not.toHaveBeenCalled();
    expect(onNodePositionsChange).toHaveBeenCalledTimes(1);
  });

  it("marks selection as coming from the canvas so the sync effect skips one pass", async () => {
    const { hook } = renderHandler([workflowNode("a")]);

    const changes: NodeChange[] = [{ type: "select", id: "a", selected: true }];
    hook.result.current.handler(changes);
    await flushMicrotasks();

    expect(hook.result.current.selectionFromCanvasRef.current).toBe(true);
  });
});
