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
  collapsedLoops: Set<string>;
  loopMembers: Map<string, string[]>;
  nodeToLoops: Map<string, string[]>;
  endLoopIds: Set<string>;
  endLoopForLoop: Map<string, string>;
  toggleLoopCollapse: (loopId: string) => void;
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
  onNodePositionsChange: (updates: Map<string, { x: number; y: number }>) => void;
  onDeleteNodes: (ids: string[]) => void;
  onBeforeNodeDrag?: () => void;
}

export function useNodeSync({
  workflow,
  selectedNode,
  activeNode,
  collapsedLoops,
  loopMembers,
  nodeToLoops,
  endLoopIds,
  endLoopForLoop,
  toggleLoopCollapse,
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
    collapsedLoops,
    loopMembers,
    nodeToLoops,
    endLoopIds,
    endLoopForLoop,
    toggleLoopCollapse,
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

  // ── Handle RF node changes (position, selection, deletion) ─────────
  const handleNodesChange: OnNodesChange = useNodeChangeHandler({
    workflow,
    collapsedApps,
    appGroups,
    nodeToAppGroup,
    appGroupMeta,
    nodeToLoops,
    collapsedUserGroups,
    nodeToUserGroup,
    userGroupMeta,
    selectionFromCanvasRef,
    deletedNodeIdsRef,
    onSelectNode,
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
