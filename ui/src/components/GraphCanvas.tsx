import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  ReactFlow,
  Background,
  Controls,
  SelectionMode,
  type Node as RFNode,
  type NodeTypes,
  type EdgeTypes,
  MarkerType,
} from "@xyflow/react";
import "@xyflow/react/dist/style.css";
import type { Workflow, Edge } from "../bindings";
import { useAppGrouping } from "../hooks/useAppGrouping";
import { useUserGrouping } from "../hooks/useUserGrouping";
import { useNodeSync } from "../hooks/useNodeSync";
import { useEdgeSync } from "../hooks/useEdgeSync";
import { AppGroupNode } from "./AppGroupNode";
import { UserGroupNode } from "./UserGroupNode";
import { WorkflowNode } from "./WorkflowNode";
import { GroupContextMenu, type GroupContextMenuItem } from "./GroupContextMenu";
import { CreateGroupPopover } from "./CreateGroupPopover";
import { validateGroupCreation, topologicalSortMembers, expandCollapsedSelection } from "../utils/groupValidation";
import { isTextInput } from "../hooks/useUndoRedoKeyboard";

interface GraphCanvasProps {
  workflow: Workflow;
  selectedNode: string | null;
  activeNode: string | null;
  canvasSelectionResetTick: number;
  onSelectNode: (id: string | null) => void;
  onCanvasSelectionChange: (hasMulti: boolean) => void;
  onNodePositionsChange: (updates: Map<string, { x: number; y: number }>) => void;
  onEdgesChange: (edges: Edge[]) => void;
  onConnect: (from: string, to: string, sourceHandle?: string) => void;
  onDeleteNodes: (ids: string[]) => void;
  onRemoveExtraEdges: (edges: Edge[]) => void;
  onBeforeNodeDrag?: () => void;
  onCreateGroup: (name: string, color: string, nodeIds: string[], parentGroupId: string | null) => void;
  onRemoveGroup: (groupId: string) => void;
  onDeleteGroupWithContents: (groupId: string) => void;
  onRenameGroup: (groupId: string, name: string) => void;
  onRecolorGroup: (groupId: string, color: string) => void;
  onAddNodesToGroup: (groupId: string, nodeIds: string[]) => void;
  onRemoveNodesFromGroup: (groupId: string, nodeIds: string[]) => void;
}

export function GraphCanvas({
  workflow,
  selectedNode,
  activeNode,
  canvasSelectionResetTick,
  onSelectNode,
  onCanvasSelectionChange,
  onNodePositionsChange,
  onEdgesChange,
  onConnect,
  onDeleteNodes,
  onRemoveExtraEdges,
  onBeforeNodeDrag,
  onCreateGroup,
  onRemoveGroup,
  onDeleteGroupWithContents,
  onRenameGroup,
  onRecolorGroup,
  onAddNodesToGroup,
  onRemoveNodesFromGroup,
}: GraphCanvasProps) {
  const nodeTypes: NodeTypes = useMemo(
    () => ({ workflow: WorkflowNode, appGroup: AppGroupNode, userGroup: UserGroupNode }),
    [],
  );

  const edgeTypes: EdgeTypes = useMemo(
    () => ({}),
    [],
  );

  const appState = useAppGrouping(workflow);
  const userGroupState = useUserGrouping(workflow);

  // Inline rename state — declared before useNodeSync so it can be passed in
  const [renamingGroupId, setRenamingGroupId] = useState<string | null>(null);

  const handleRenameConfirm = useCallback(
    (groupId: string, newName: string) => {
      if (newName.trim()) onRenameGroup(groupId, newName.trim());
      setRenamingGroupId(null);
    },
    [onRenameGroup],
  );

  const handleRenameCancel = useCallback(() => {
    setRenamingGroupId(null);
  }, []);

  const { rfNodes, handleNodesChange, handleNodeDragStart, deletedNodeIdsRef } = useNodeSync({
    workflow,
    selectedNode,
    activeNode,
    canvasSelectionResetTick,
    collapsedApps: appState.collapsedApps,
    appGroups: appState.appGroups,
    nodeToAppGroup: appState.nodeToAppGroup,
    appGroupMeta: appState.appGroupMeta,
    toggleAppCollapse: appState.toggleAppCollapse,
    collapsedUserGroups: userGroupState.collapsedUserGroups,
    nodeToUserGroup: userGroupState.nodeToUserGroup,
    userGroupMeta: userGroupState.userGroupMeta,
    toggleUserGroupCollapse: userGroupState.toggleUserGroupCollapse,
    renamingGroupId,
    onRenameConfirm: handleRenameConfirm,
    onRenameCancel: handleRenameCancel,
    onSelectNode,
    onCanvasSelectionChange,
    onNodePositionsChange,
    onDeleteNodes,
    onBeforeNodeDrag,
  });

  // Ref for rfNodes so callbacks/effects can read current value without re-subscribing
  const rfNodesRef = useRef(rfNodes);
  rfNodesRef.current = rfNodes;

  const mergedHiddenNodeIds = useMemo(() => {
    const ids = new Set<string>();
    for (const id of userGroupState.hiddenUserGroupNodeIds) ids.add(id);
    return ids;
  }, [userGroupState.hiddenUserGroupNodeIds]);

  const { rfEdges, handleEdgesChange, handleConnect } = useEdgeSync({
    workflow,
    hiddenNodeIds: mergedHiddenNodeIds,
    collapsedAppEdgeRewrites: appState.collapsedAppEdgeRewrites,
    collapsedUserGroupEdgeRewrites: userGroupState.userGroupEdgeRewrites,
    deletedNodeIdsRef,
    onEdgesChange,
    onRemoveExtraEdges,
    onConnect,
  });

  const handlePaneClick = useCallback(() => {
    onSelectNode(null);
    onCanvasSelectionChange(false);
    setContextMenu(null);
    setCreateGroupPopover(null);
  }, [onSelectNode, onCanvasSelectionChange]);

  // ── Context menu + group creation popover state ──────────────────
  const [contextMenu, setContextMenu] = useState<{
    position: { x: number; y: number };
    items: GroupContextMenuItem[];
  } | null>(null);

  const [createGroupPopover, setCreateGroupPopover] = useState<{
    position: { x: number; y: number };
    nodeIds: string[];
    parentGroupId: string | null;
  } | null>(null);

  const groupColorIndexRef = useRef(0);

  // ── Context menu handler ─────────────────────────────────────────
  const handleContextMenu = useCallback(
    (event: React.MouseEvent | MouseEvent, rfNode?: RFNode) => {
      event.preventDefault();
      setCreateGroupPopover(null);

      const pos = { x: event.clientX, y: event.clientY };

      // Get the wrapper div's bounding rect so we can convert to relative position
      const target = (event as React.MouseEvent).currentTarget ?? (event.target as HTMLElement);
      const wrapperEl = (target as HTMLElement).closest?.("[data-graph-canvas-wrapper]") as HTMLElement | null;
      let relativePos: { x: number; y: number };
      if (wrapperEl) {
        const rect = wrapperEl.getBoundingClientRect();
        relativePos = { x: event.clientX - rect.left, y: event.clientY - rect.top };
      } else {
        relativePos = pos;
      }

      const items: GroupContextMenuItem[] = [];

      if (rfNode) {
        const nodeData = rfNode.data as Record<string, unknown>;

        // Case 1: Right-click on expanded user group container
        if (rfNode.type === "userGroup") {
          const groupId = rfNode.id;
          const meta = userGroupState.userGroupMeta.get(groupId);
          if (meta) {
            items.push({
              label: "Rename",
              action: () => setRenamingGroupId(groupId),
            });
            items.push({
              label: "Change Color",
              colorPicker: {
                currentColor: meta.color,
                onPickColor: (color: string) => onRecolorGroup(groupId, color),
              },
            });
            items.push({
              label: "Ungroup",
              action: () => onRemoveGroup(groupId),
            });
            items.push({
              label: "Delete Group + Contents",
              action: () => onDeleteGroupWithContents(groupId),
              danger: true,
            });
          }
          setContextMenu({ position: relativePos, items });
          return;
        }

        // Case 2: Right-click on collapsed user group pill
        if (nodeData.isUserGroupPill) {
          const groupId = nodeData.userGroupId as string;
          const meta = userGroupState.userGroupMeta.get(groupId);
          if (meta) {
            items.push({
              label: "Expand",
              action: () => userGroupState.toggleUserGroupCollapse(groupId),
            });
            items.push({
              label: "Rename",
              action: () => setRenamingGroupId(groupId),
            });
            items.push({
              label: "Ungroup",
              action: () => onRemoveGroup(groupId),
            });
            items.push({
              label: "Delete Group + Contents",
              action: () => onDeleteGroupWithContents(groupId),
              danger: true,
            });
          }
          setContextMenu({ position: relativePos, items });
          return;
        }

        // Case 3: Right-click on individual node inside a user group
        const userGroupId = userGroupState.nodeToUserGroup.get(rfNode.id);
        if (userGroupId) {
          const meta = userGroupState.userGroupMeta.get(userGroupId);
          items.push({
            label: `Remove from ${meta?.name ?? "Group"}`,
            action: () => onRemoveNodesFromGroup(userGroupId, [rfNode.id]),
          });
          setContextMenu({ position: relativePos, items });
          return;
        }

        // Case 4: Single node — check if adjacent to existing groups for "Add to" option
        if (rfNode && !userGroupState.nodeToUserGroup.has(rfNode.id)) {
          const existingGroups = workflow.groups ?? [];
          for (const group of existingGroups) {
            const groupNodeSet = new Set(group.node_ids);
            if (groupNodeSet.has(rfNode.id)) continue;
            // Check graph adjacency
            const hasEdge = workflow.edges.some(
              (e) =>
                (e.from === rfNode.id && groupNodeSet.has(e.to)) ||
                (e.to === rfNode.id && groupNodeSet.has(e.from)),
            );
            if (!hasEdge) continue;
            // Validate: don't allow adding a node that would break auto-group invariants
            const candidateIds = [...group.node_ids, rfNode.id];
            const validation = validateGroupCreation(
              candidateIds, workflow, existingGroups.filter((g) => g.id !== group.id),
              appState.appGroups,
            );
            if (!validation.valid) continue;
            items.push({
              label: `Add to "${group.name}"`,
              action: () => onAddNodesToGroup(group.id, [rfNode.id]),
            });
          }
        }

        if (items.length > 0) {
          setContextMenu({ position: relativePos, items });
          return;
        }
      }

      // Case 5: Multi-selection (2+ nodes) — check for "Create Group" option
      const selectedNodes = rfNodesRef.current.filter(
        (n) => n.selected && !n.hidden,
      );
      if (selectedNodes.length >= 2) {
        const rawSelectedIds = selectedNodes.map((n: RFNode) => n.id);
        const expandedIds = expandCollapsedSelection(
          rawSelectedIds,
          appState.collapsedApps, appState.nodeToAppGroup, appState.appGroups,
        );
        const existingGroups = workflow.groups ?? [];

        const result = validateGroupCreation(
          expandedIds,
          workflow,
          existingGroups,
          appState.appGroups,
        );

        if (result.valid) {
          const sorted = topologicalSortMembers(expandedIds, workflow);
          items.push({
            label: "Create Group",
            action: () => {
              setContextMenu(null);
              setCreateGroupPopover({
                position: relativePos,
                nodeIds: sorted,
                parentGroupId: result.parentGroupId ?? null,
              });
            },
          });
        }
      }

      if (items.length > 0) {
        setContextMenu({ position: relativePos, items });
      } else {
        setContextMenu(null);
      }
    },
    [
      workflow,
      appState.appGroups,
      appState.collapsedApps,
      appState.nodeToAppGroup,
      userGroupState.userGroupMeta,
      userGroupState.nodeToUserGroup,
      userGroupState.toggleUserGroupCollapse,
      onRenameGroup,
      onRemoveGroup,
      onDeleteGroupWithContents,
      onRemoveNodesFromGroup,
      onAddNodesToGroup,
      onRecolorGroup,
    ],
  );

  const handleCreateGroupConfirm = useCallback(
    (name: string, color: string) => {
      if (!createGroupPopover) return;
      onCreateGroup(name, color, createGroupPopover.nodeIds, createGroupPopover.parentGroupId);
      groupColorIndexRef.current += 1;
      setCreateGroupPopover(null);
    },
    [createGroupPopover, onCreateGroup],
  );

  // ── Cmd+G keyboard shortcut for quick group creation ──────────
  const wrapperRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    function handleKeyDown(e: KeyboardEvent) {
      if (isTextInput(document.activeElement)) return;
      if (!(e.metaKey || e.ctrlKey) || e.key.toLowerCase() !== "g") return;

      e.preventDefault();

      const currentNodes = rfNodesRef.current;
      const selectedNodes = currentNodes.filter((n) => n.selected && !n.hidden);
      if (selectedNodes.length < 2) return;

      const rawSelectedIds = selectedNodes.map((n) => n.id);
      const expandedIds = expandCollapsedSelection(
        rawSelectedIds,
        appState.collapsedApps, appState.nodeToAppGroup, appState.appGroups,
      );

      const existingGroups = workflow.groups ?? [];
      const result = validateGroupCreation(
        expandedIds, workflow, existingGroups,
        appState.appGroups,
      );
      if (!result.valid) return;

      const sorted = topologicalSortMembers(expandedIds, workflow);

      const rect = wrapperRef.current?.getBoundingClientRect();
      const pos = rect
        ? { x: rect.width / 2 - 120, y: rect.height / 2 - 80 }
        : { x: 200, y: 200 };

      setContextMenu(null);
      setCreateGroupPopover({
        position: pos,
        nodeIds: sorted,
        parentGroupId: result.parentGroupId ?? null,
      });
    }

    window.addEventListener("keydown", handleKeyDown);
    return () => window.removeEventListener("keydown", handleKeyDown);
  }, [workflow, appState]);

  // Type-safe connection validation for data port drag (called on every mousemove)
  const isValidConnection = useCallback(
    (_connection: { source?: string | null; target?: string | null; sourceHandle?: string | null; targetHandle?: string | null }) => {
      return true;
    },
    [],
  );

  return (
    <div ref={wrapperRef} className="relative h-full w-full" data-graph-canvas-wrapper>
      <ReactFlow
        nodes={rfNodes}
        edges={rfEdges}
        nodeTypes={nodeTypes}
        edgeTypes={edgeTypes}
        onNodesChange={handleNodesChange}
        onEdgesChange={handleEdgesChange}
        onConnect={handleConnect}
        isValidConnection={isValidConnection}
        onNodeDragStart={handleNodeDragStart}
        onPaneClick={handlePaneClick}
        onPaneContextMenu={(e) => handleContextMenu(e)}
        onNodeClick={(event, rfNode) => {
          if (rfNode.type !== "workflow") return;
          // Shift/Ctrl/Meta-click is a multi-select gesture — leave selection
          // resolution to onNodesChange so the detail modal doesn't open while
          // the user is picking multiple nodes.
          if (event.shiftKey || event.ctrlKey || event.metaKey) return;
          onSelectNode(rfNode.id);
        }}
        onNodeContextMenu={(e, rfNode) => handleContextMenu(e, rfNode)}
        deleteKeyCode={["Backspace", "Delete"]}
        selectionOnDrag
        selectionMode={SelectionMode.Partial}
        panOnDrag={[1]}
        panOnScroll
        fitView
        fitViewOptions={{ maxZoom: 1 }}
        snapToGrid
        snapGrid={[20, 20]}
        defaultEdgeOptions={{
          type: "smoothstep",
          selectable: true,
          markerEnd: { type: MarkerType.ArrowClosed, color: "#666" },
          style: { stroke: "#555", strokeWidth: 2 },
        }}
        proOptions={{ hideAttribution: true }}
        style={{ background: "var(--bg-dark)" }}
      >
        <Background color="#333" gap={20} />
        <Controls
          showInteractive={false}
          style={{ background: "var(--bg-panel)", borderColor: "var(--border)" }}
        />
      </ReactFlow>

      {contextMenu && (
        <GroupContextMenu
          position={contextMenu.position}
          items={contextMenu.items}
          onClose={() => setContextMenu(null)}
        />
      )}

      {createGroupPopover && (
        <CreateGroupPopover
          position={createGroupPopover.position}
          defaultColorIndex={groupColorIndexRef.current}
          onConfirm={handleCreateGroupConfirm}
          onCancel={() => setCreateGroupPopover(null)}
        />
      )}
    </div>
  );
}
