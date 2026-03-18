import { useCallback, useMemo, useRef, useState } from "react";
import {
  ReactFlow,
  Background,
  Controls,
  SelectionMode,
  type Node as RFNode,
  type NodeTypes,
  MarkerType,
} from "@xyflow/react";
import "@xyflow/react/dist/style.css";
import type { Workflow, Edge } from "../bindings";
import { useLoopGrouping } from "../hooks/useLoopGrouping";
import { useAppGrouping } from "../hooks/useAppGrouping";
import { useUserGrouping } from "../hooks/useUserGrouping";
import { useNodeSync } from "../hooks/useNodeSync";
import { useEdgeSync } from "../hooks/useEdgeSync";
import { AppGroupNode } from "./AppGroupNode";
import { LoopGroupNode } from "./LoopGroupNode";
import { UserGroupNode } from "./UserGroupNode";
import { WorkflowNode } from "./WorkflowNode";
import { GroupContextMenu, type GroupContextMenuItem } from "./GroupContextMenu";
import { CreateGroupPopover } from "./CreateGroupPopover";
import { validateGroupCreation, topologicalSortMembers } from "../utils/groupValidation";

interface GraphCanvasProps {
  workflow: Workflow;
  selectedNode: string | null;
  activeNode: string | null;
  onSelectNode: (id: string | null) => void;
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
  onSelectNode,
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
    () => ({ workflow: WorkflowNode, loopGroup: LoopGroupNode, appGroup: AppGroupNode, userGroup: UserGroupNode }),
    [],
  );

  const loopState = useLoopGrouping(workflow);
  const appState = useAppGrouping(workflow);
  const userGroupState = useUserGrouping(workflow);

  const { rfNodes, handleNodesChange, handleNodeDragStart, deletedNodeIdsRef } = useNodeSync({
    workflow,
    selectedNode,
    activeNode,
    ...loopState,
    collapsedApps: appState.collapsedApps,
    appGroups: appState.appGroups,
    nodeToAppGroup: appState.nodeToAppGroup,
    appGroupMeta: appState.appGroupMeta,
    toggleAppCollapse: appState.toggleAppCollapse,
    collapsedUserGroups: userGroupState.collapsedUserGroups,
    nodeToUserGroup: userGroupState.nodeToUserGroup,
    userGroupMeta: userGroupState.userGroupMeta,
    toggleUserGroupCollapse: userGroupState.toggleUserGroupCollapse,
    onSelectNode,
    onNodePositionsChange,
    onDeleteNodes,
    onBeforeNodeDrag,
  });

  const mergedHiddenNodeIds = useMemo(() => {
    const ids = new Set(loopState.hiddenNodeIds);
    for (const id of userGroupState.hiddenUserGroupNodeIds) ids.add(id);
    return ids;
  }, [loopState.hiddenNodeIds, userGroupState.hiddenUserGroupNodeIds]);

  const { rfEdges, handleEdgesChange, handleConnect } = useEdgeSync({
    workflow,
    hiddenNodeIds: mergedHiddenNodeIds,
    collapsedLoops: loopState.collapsedLoops,
    collapsedAppEdgeRewrites: appState.collapsedAppEdgeRewrites,
    collapsedUserGroupEdgeRewrites: userGroupState.userGroupEdgeRewrites,
    deletedNodeIdsRef,
    onEdgesChange,
    onRemoveExtraEdges,
    onConnect,
  });

  const handlePaneClick = useCallback(() => {
    onSelectNode(null);
    setContextMenu(null);
    setCreateGroupPopover(null);
  }, [onSelectNode]);

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
      const relativePos = wrapperEl
        ? { x: event.clientX - wrapperEl.getBoundingClientRect().left, y: event.clientY - wrapperEl.getBoundingClientRect().top }
        : pos;

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
              action: () => {
                const newName = window.prompt("Rename group:", meta.name);
                if (newName && newName.trim()) onRenameGroup(groupId, newName.trim());
              },
            });
            items.push({
              label: "Change Color",
              action: () => {
                // TODO: inline color picker — stub for now
              },
              disabled: true,
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
              action: () => {
                const newName = window.prompt("Rename group:", meta.name);
                if (newName && newName.trim()) onRenameGroup(groupId, newName.trim());
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

        // Case 5: Single node — check if adjacent to existing groups for "Add to" option
        const existingGroups = workflow.groups ?? [];
        for (const group of existingGroups) {
          const groupNodeSet = new Set(group.node_ids);
          // Skip if node is already in this group
          if (groupNodeSet.has(rfNode.id)) continue;
          // Skip if node is already in another user group
          if (userGroupState.nodeToUserGroup.has(rfNode.id)) continue;
          // Check if this node has an edge to any member of the group
          const hasEdge = workflow.edges.some(
            (e) =>
              (e.from === rfNode.id && groupNodeSet.has(e.to)) ||
              (e.to === rfNode.id && groupNodeSet.has(e.from)),
          );
          if (hasEdge) {
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
      const selectedNodes = rfNodes.filter(
        (n) => n.selected && !n.hidden,
      );
      if (selectedNodes.length >= 2) {
        const selectedIds = selectedNodes.map((n: RFNode) => n.id);
        // Build loopMembers map in format expected by validateGroupCreation
        const loopMembersMap = loopState.loopMembers;
        // Build appGroups map
        const appGroupsMap = appState.appGroups;
        const existingGroups = workflow.groups ?? [];

        const result = validateGroupCreation(
          selectedIds,
          workflow,
          existingGroups,
          loopMembersMap,
          appGroupsMap,
        );

        if (result.valid) {
          const sorted = topologicalSortMembers(selectedIds, workflow);
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
      loopState.loopMembers,
      appState.appGroups,
      userGroupState.userGroupMeta,
      userGroupState.nodeToUserGroup,
      userGroupState.toggleUserGroupCollapse,
      onRenameGroup,
      onRemoveGroup,
      onDeleteGroupWithContents,
      onRemoveNodesFromGroup,
      onAddNodesToGroup,
      rfNodes,
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

  return (
    <div className="relative h-full w-full" data-graph-canvas-wrapper>
      <ReactFlow
        nodes={rfNodes}
        edges={rfEdges}
        nodeTypes={nodeTypes}
        onNodesChange={handleNodesChange}
        onEdgesChange={handleEdgesChange}
        onConnect={handleConnect}
        onNodeDragStart={handleNodeDragStart}
        onPaneClick={handlePaneClick}
        onPaneContextMenu={(e) => handleContextMenu(e)}
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
          type: "default",
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
