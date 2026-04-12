import { describe, it, expect, vi } from "vitest";
import { act, renderHook } from "@testing-library/react";
import { useRef, useState } from "react";
import type { Node as RFNode, NodeChange } from "@xyflow/react";
import { useNodeChangeHandler } from "./useNodeChangeHandler";
import { makeWorkflow, node } from "../test-helpers";

function rfNode(id: string, type: string, selected = false): RFNode {
  return {
    id,
    type,
    position: { x: 0, y: 0 },
    data: {},
    selected,
  };
}

function workflowNode(id: string, selected = false): RFNode {
  return rfNode(id, "workflow", selected);
}

function renderHandler(initialRfNodes: RFNode[]) {
  const onSelectNode = vi.fn<(id: string | null) => void>();
  const onCanvasSelectionChange = vi.fn<(has: boolean) => void>();
  const onNodePositionsChange = vi.fn();
  const onDeleteNodes = vi.fn();

  const wf = makeWorkflow(
    initialRfNodes
      .filter((n) => n.type === "workflow")
      .map((n) => node(n.id, "Click")),
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
      onCanvasSelectionChange,
      onNodePositionsChange,
      onDeleteNodes,
      setRfNodes,
    });
    return { handler, rfNodes, selectionFromCanvasRef };
  });

  return { hook, onSelectNode, onCanvasSelectionChange, onNodePositionsChange, onDeleteNodes };
}

// Dispatch changes through the handler inside act() so React state updates
// settle, then drain the microtask queue where onSelectNode is scheduled.
async function dispatch(handler: (c: NodeChange[]) => void, changes: NodeChange[]) {
  await act(async () => {
    handler(changes);
    await new Promise<void>((resolve) => queueMicrotask(resolve));
  });
}

describe("useNodeChangeHandler — selection resolution", () => {
  it("selects the node when a single node becomes selected", async () => {
    const { hook, onSelectNode } = renderHandler([
      workflowNode("a"),
      workflowNode("b"),
    ]);

    const changes: NodeChange[] = [{ type: "select", id: "a", selected: true }];
    await dispatch(hook.result.current.handler, changes);

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
    await dispatch(hook.result.current.handler, changes);

    expect(onSelectNode).toHaveBeenCalledTimes(1);
    expect(onSelectNode).toHaveBeenCalledWith(null);
  });

  it("clears selection when a workflow node and a group container are both selected", async () => {
    // Box-select can catch one workflow node plus the enclosing appGroup or
    // userGroup container; the total RF selection is 2, so the modal stays
    // closed even though exactly one *workflow* node is in the mix.
    const { hook, onSelectNode, onCanvasSelectionChange } = renderHandler([
      workflowNode("a"),
      rfNode("grp1", "appGroup"),
    ]);

    const changes: NodeChange[] = [
      { type: "select", id: "a", selected: true },
      { type: "select", id: "grp1", selected: true },
    ];
    await dispatch(hook.result.current.handler, changes);

    expect(onSelectNode).toHaveBeenCalledWith(null);
    expect(onCanvasSelectionChange).toHaveBeenCalledWith(true);
  });

  it("flags canvas selection when only a group container is selected", async () => {
    // A lone selected appGroup/userGroup container doesn't live in the
    // detail modal (selectedNode stays null), but the Escape handler still
    // needs to clear it, so hasCanvasSelection flips true.
    const { hook, onSelectNode, onCanvasSelectionChange } = renderHandler([
      rfNode("grp1", "userGroup"),
      workflowNode("a"),
    ]);

    const changes: NodeChange[] = [
      { type: "select", id: "grp1", selected: true },
    ];
    await dispatch(hook.result.current.handler, changes);

    expect(onSelectNode).toHaveBeenCalledWith(null);
    expect(onCanvasSelectionChange).toHaveBeenCalledWith(true);
  });

  it("clears selection when shift-click extends an existing selection", async () => {
    const { hook, onSelectNode } = renderHandler([
      workflowNode("a", true),
      workflowNode("b"),
    ]);

    // Shift-click on "b" adds it to the selection without deselecting "a".
    const changes: NodeChange[] = [{ type: "select", id: "b", selected: true }];
    await dispatch(hook.result.current.handler, changes);

    expect(onSelectNode).toHaveBeenCalledTimes(1);
    expect(onSelectNode).toHaveBeenCalledWith(null);
  });

  it("clears selection when all nodes are deselected", async () => {
    const { hook, onSelectNode } = renderHandler([
      workflowNode("a", true),
    ]);

    const changes: NodeChange[] = [{ type: "select", id: "a", selected: false }];
    await dispatch(hook.result.current.handler, changes);

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
    await dispatch(hook.result.current.handler, changes);

    expect(onSelectNode).not.toHaveBeenCalled();
    expect(onNodePositionsChange).toHaveBeenCalledTimes(1);
  });

  it("clears the canvas-selection flag when nodes are deleted", async () => {
    // Deleting a multi-selection must not leave hasCanvasSelection stuck at
    // true, otherwise a subsequent Escape is consumed by a phantom flag.
    const { hook, onCanvasSelectionChange, onDeleteNodes } = renderHandler([
      workflowNode("a", true),
      workflowNode("b", true),
    ]);

    const changes: NodeChange[] = [
      { type: "remove", id: "a" },
      { type: "remove", id: "b" },
    ];
    await dispatch(hook.result.current.handler, changes);

    expect(onDeleteNodes).toHaveBeenCalledWith(["a", "b"]);
    expect(onCanvasSelectionChange).toHaveBeenCalledWith(false);
  });

  it("marks selection as coming from the canvas so the sync effect skips one pass", async () => {
    const { hook } = renderHandler([workflowNode("a")]);

    const changes: NodeChange[] = [{ type: "select", id: "a", selected: true }];
    await dispatch(hook.result.current.handler, changes);

    expect(hook.result.current.selectionFromCanvasRef.current).toBe(true);
  });
});
