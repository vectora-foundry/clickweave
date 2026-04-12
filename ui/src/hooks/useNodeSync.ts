import { useCallback, useLayoutEffect, useRef, useState } from "react";
import type { Node as RFNode, OnNodesChange } from "@xyflow/react";
import type { Workflow } from "../bindings";
import type { AppGroupMeta } from "./useAppGrouping";
import type { UserGroupMeta } from "./useUserGrouping";
import { useRfNodeBuilder } from "./node-sync/useRfNodeBuilder";
import { useNodeChangeHandler } from "./node-sync/useNodeChangeHandler";

// Re-export buildAppKindMap so existing imports from App.tsx continue to work.
export { buildAppKindMap } from "./node-sync/nodeBuilders";

interface UseNodeSyncParams {
  workflow: Workflow;
  selectedNode: string | null;
  activeNode: string | null;
  canvasSelectionResetTick: number;
  // App grouping params
  collapsedApps: Set<string>;
  appGroups: Map<string, string[]>;
  nodeToAppGroup: Map<string, string>;
  appGroupMeta: Map<string, AppGroupMeta>;
  toggleAppCollapse: (groupId: string) => void;
  // User grouping params
  collapsedUserGroups: Set<string>;
  nodeToUserGroup: Map<string, string>;
  userGroupMeta: Map<string, UserGroupMeta>;
  toggleUserGroupCollapse: (groupId: string) => void;
  renamingGroupId: string | null;
  onRenameConfirm: (groupId: string, newName: string) => void;
  onRenameCancel: () => void;
  onSelectNode: (id: string | null) => void;
  onCanvasSelectionChange: (hasMulti: boolean) => void;
  onNodePositionsChange: (updates: Map<string, { x: number; y: number }>) => void;
  onDeleteNodes: (ids: string[]) => void;
  onBeforeNodeDrag?: () => void;
}

export function useNodeSync({
  workflow,
  selectedNode,
  activeNode,
  canvasSelectionResetTick,
  collapsedApps,
  appGroups,
  nodeToAppGroup,
  appGroupMeta,
  toggleAppCollapse,
  collapsedUserGroups,
  nodeToUserGroup,
  userGroupMeta,
  toggleUserGroupCollapse,
  renamingGroupId,
  onRenameConfirm,
  onRenameCancel,
  onSelectNode,
  onCanvasSelectionChange,
  onNodePositionsChange,
  onDeleteNodes,
  onBeforeNodeDrag,
}: UseNodeSyncParams) {
  const [rfNodes, setRfNodes] = useState<RFNode[]>([]);
  const selectionFromCanvasRef = useRef(false);
  const deletedNodeIdsRef = useRef<Set<string> | null>(null);

  // ── Build RF nodes from workflow state ──────────────────────────────
  useRfNodeBuilder({
    workflow,
    selectedNode,
    activeNode,
    collapsedApps,
    appGroups,
    nodeToAppGroup,
    appGroupMeta,
    toggleAppCollapse,
    collapsedUserGroups,
    nodeToUserGroup,
    userGroupMeta,
    toggleUserGroupCollapse,
    renamingGroupId,
    onRenameConfirm,
    onRenameCancel,
    onDeleteNodes,
    setRfNodes,
  });

  // ── Sync external selectedNode changes into RF selection state ──────
  // useLayoutEffect ensures deselection runs before paint, preventing a frame where
  // rfNodes still shows the node as selected after the modal closes.
  useLayoutEffect(() => {
    if (selectionFromCanvasRef.current) {
      selectionFromCanvasRef.current = false;
      return;
    }
    setRfNodes((prev) =>
      prev.map((n) => {
        const shouldBeSelected = n.id === selectedNode;
        if (n.selected === shouldBeSelected) return n;
        return { ...n, selected: shouldBeSelected };
      }),
    );
  }, [selectedNode]);

  // ── Force-clear RF selection when the store requests a full reset ───
  // Escape while a multi-selection is active goes through this path:
  // `selectedNode` is already null, so the effect above no-ops; the reset
  // tick gives us an explicit signal to wipe every RF node's `selected`
  // flag without threading an imperative handle out of this hook.
  useLayoutEffect(() => {
    if (canvasSelectionResetTick === 0) return;
    setRfNodes((prev) => {
      let changed = false;
      const next = prev.map((n) => {
        if (!n.selected) return n;
        changed = true;
        return { ...n, selected: false };
      });
      return changed ? next : prev;
    });
  }, [canvasSelectionResetTick, setRfNodes]);

  // ── Handle RF node changes (position, selection, deletion) ─────────
  const handleNodesChange: OnNodesChange = useNodeChangeHandler({
    workflow,
    collapsedApps,
    appGroups,
    nodeToAppGroup,
    appGroupMeta,
    collapsedUserGroups,
    nodeToUserGroup,
    userGroupMeta,
    selectionFromCanvasRef,
    deletedNodeIdsRef,
    onSelectNode,
    onCanvasSelectionChange,
    onNodePositionsChange,
    onDeleteNodes,
    setRfNodes,
  });

  const handleNodeDragStart = useCallback(() => {
    onBeforeNodeDrag?.();
  }, [onBeforeNodeDrag]);

  return {
    rfNodes,
    handleNodesChange,
    handleNodeDragStart,
    deletedNodeIdsRef,
  };
}
