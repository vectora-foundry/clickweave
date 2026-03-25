import { useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";
import {
  type Node as RFNode,
  type OnNodesChange,
  applyNodeChanges,
} from "@xyflow/react";
import type { AppKind, Workflow } from "../bindings";
import { usesCdp } from "../utils/appKind";
import { nodeMetadata, defaultNodeMetadata } from "../constants/nodeMetadata";
import { buildDag, type DagGraph, isAppAnchorNode } from "../utils/appGroupComputation";
import type { AppGroupMeta } from "./useAppGrouping";
import type { UserGroupMeta } from "./useUserGrouping";

// Layout constants for loop group positioning
const LOOP_HEADER_HEIGHT = 40;
const LOOP_PADDING = 20;
const APPROX_NODE_WIDTH = 160;
const APPROX_NODE_HEIGHT = 50;
const MIN_GROUP_WIDTH = 300;
const MIN_GROUP_HEIGHT = 150;

// App group layout constants
const APP_GROUP_HEADER_HEIGHT = 36;
const APP_GROUP_PADDING = 20;

// User group layout constants
const USER_GROUP_HEADER_HEIGHT = 36;
const USER_GROUP_PADDING = 20;

/** Return layout constants for a group node type. */
function groupConstants(parentType: string): { headerHeight: number; padding: number } {
  if (parentType === "appGroup") return { headerHeight: APP_GROUP_HEADER_HEIGHT, padding: APP_GROUP_PADDING };
  if (parentType === "userGroup") return { headerHeight: USER_GROUP_HEADER_HEIGHT, padding: USER_GROUP_PADDING };
  return { headerHeight: LOOP_HEADER_HEIGHT, padding: LOOP_PADDING };
}

function clickSubtitle(nt: Workflow["nodes"][number]["node_type"]): string | undefined {
  if (nt.type !== "Click") return undefined;
  if (nt.target) {
    if (nt.target.type === "Text") return nt.target.text;
    if (nt.target.type === "CdpElement") return nt.target.name;
    if (nt.target.type === "WindowControl") {
      const names: Record<string, string> = { Close: "Close window", Minimize: "Minimize window", Maximize: "Maximize window", Zoom: "Zoom window" };
      return names[nt.target.action] ?? nt.target.action;
    }
  }
  if (nt.template_image) return "image match";
  if (nt.x != null && nt.y != null) return `at (${Math.round(nt.x)}, ${Math.round(nt.y)})`;
  return undefined;
}

/** Forward-propagate app_kind from FocusWindow nodes to all downstream nodes. */
export function buildAppKindMap(workflow: Workflow, dag?: DagGraph): Map<string, AppKind> {
  const { nodeById, outgoing, inDegree: inDegreeOriginal } = dag ?? buildDag(workflow);
  const inDegree = new Map(inDegreeOriginal);

  const result = new Map<string, AppKind>();
  const queue: string[] = [];
  for (const [id, deg] of inDegree) {
    if (deg === 0) queue.push(id);
  }

  let head = 0;
  while (head < queue.length) {
    const id = queue[head++];
    const node = nodeById.get(id);

    if (node?.node_type.type === "FocusWindow") {
      result.set(id, node.node_type.app_kind ?? "Native");
    } else if (isAppAnchorNode(node)) {
      // launch_app McpToolCall — extract app_kind from arguments
      const args = node!.node_type.type === "McpToolCall" ? node!.node_type.arguments : null;
      const kind = (typeof args === "object" && args !== null && !Array.isArray(args)
        ? args.app_kind : undefined) as AppKind | undefined;
      result.set(id, kind ?? "Native");
    }

    const kind = result.get(id);
    for (const target of outgoing.get(id) ?? []) {
      if (kind && !result.has(target)) {
        result.set(target, kind);
      }
      inDegree.set(target, (inDegree.get(target) ?? 0) - 1);
      if (inDegree.get(target) === 0) queue.push(target);
    }
  }

  return result;
}

function nodeSubtitle(
  nt: Workflow["nodes"][number]["node_type"],
  appKind: AppKind | undefined,
): string | undefined {
  // Click nodes: show target info
  const click = clickSubtitle(nt);
  if (click) {
    // Append DevTools context if applicable
    if (appKind && usesCdp(appKind)) return `${click} · via DevTools`;
    return click;
  }
  // Non-FocusWindow nodes: show DevTools context if inherited from upstream
  if (nt.type !== "FocusWindow" && appKind && usesCdp(appKind)) {
    return "via DevTools";
  }
  return undefined;
}

function toRFNode(
  node: Workflow["nodes"][number],
  selectedNode: string | null,
  activeNode: string | null,
  onDelete: () => void,
  appKind: AppKind | undefined,
  existing?: RFNode,
): RFNode {
  const meta = nodeMetadata[node.node_type.type] ?? defaultNodeMetadata;
  return {
    ...(existing ?? {}),
    parentId: undefined,
    extent: undefined,
    hidden: undefined,
    style: undefined,
    id: node.id,
    type: "workflow",
    position: existing?.position ?? { x: node.position.x, y: node.position.y },
    selected: existing?.selected ?? (node.id === selectedNode),
    data: {
      label: node.name,
      nodeType: node.node_type.type,
      icon: meta.icon,
      color: meta.color,
      isActive: node.id === activeNode,
      enabled: node.enabled,
      onDelete,
      switchCases: node.node_type.type === "Switch"
        ? (node.node_type as { type: "Switch"; cases: { name: string }[] }).cases.map((c) => c.name)
        : [],
      role: node.role,
      subtitle: nodeSubtitle(node.node_type, appKind),
    },
  };
}

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

  // Sync workflow nodes into RF node state
  useEffect(() => {
    setRfNodes((prev) => {
      const prevMap = new Map(prev.map((n) => [n.id, n]));
      const wfNodeMap = new Map(workflow.nodes.map((n) => [n.id, n]));
      const appKindMap = buildAppKindMap(workflow);

      // Build set of anchor IDs for app groups
      const appGroupAnchors = new Set<string>();
      for (const meta of appGroupMeta.values()) {
        appGroupAnchors.add(meta.anchorId);
      }

      const nodes: RFNode[] = [];
      const groupNodeIndices = new Map<string, number>();
      const expandedGroupChildren = new Map<string, RFNode[]>();

      for (const node of workflow.nodes) {
        const existing = prevMap.get(node.id);

        // EndLoop nodes are always hidden
        if (endLoopIds.has(node.id)) {
          const base = toRFNode(node, selectedNode, activeNode, () => onDeleteNodes([node.id]), appKindMap.get(node.id), existing);
          nodes.push({ ...base, hidden: true });
          continue;
        }

        // Loop nodes: collapsed vs expanded
        if (node.node_type.type === "Loop") {
          const bodyIds = loopMembers.get(node.id) ?? [];
          const bodyCount = bodyIds.length;

          if (collapsedLoops.has(node.id)) {
            const endLoopId = endLoopForLoop.get(node.id);
            const base = toRFNode(node, selectedNode, activeNode, () => {
              const ids = [...bodyIds];
              if (endLoopId) ids.push(endLoopId);
              ids.push(node.id);
              onDeleteNodes(ids);
            }, appKindMap.get(node.id), existing);
            nodes.push({
              ...base,
              type: "workflow",
              data: {
                ...base.data,
                bodyCount,
                onToggleCollapse: () => toggleLoopCollapse(node.id),
              },
            });
          } else {
            const base = toRFNode(node, selectedNode, activeNode, () => onDeleteNodes([node.id]), appKindMap.get(node.id), existing);
            expandedGroupChildren.set(node.id, []);
            const idx = nodes.length;
            nodes.push({
              ...base,
              type: "loopGroup",
              data: {
                label: node.name,
                bodyCount,
                isActive: node.id === activeNode,
                enabled: node.enabled,
                onToggleCollapse: () => toggleLoopCollapse(node.id),
              },
            });
            groupNodeIndices.set(node.id, idx);
          }
          continue;
        }

        // App group anchor nodes (skip if inside a loop — loop takes precedence)
        if (appGroupAnchors.has(node.id) && !nodeToLoops.has(node.id)) {
          const groupId = nodeToAppGroup.get(node.id);
          if (!groupId) continue;
          const meta = appGroupMeta.get(groupId);
          if (!meta) continue;
          const memberIds = appGroups.get(groupId) ?? [];
          // Visible members: exclude nodes that render inside loops (loop takes precedence),
          // and exclude Loop/EndLoop nodes themselves (they render as their own group type)
          const visibleMemberIds = memberIds.filter((id) => {
            if (nodeToLoops.has(id)) return false;
            const wfNode = wfNodeMap.get(id);
            if (wfNode?.node_type.type === "Loop" || wfNode?.node_type.type === "EndLoop") return false;
            return true;
          });

          if (collapsedApps.has(groupId)) {
            // Collapsed — render as workflow pill using anchor's real ID
            const base = toRFNode(node, selectedNode, activeNode, () => {
              onDeleteNodes(memberIds);
            }, appKindMap.get(node.id), existing);
            nodes.push({
              ...base,
              type: "workflow",
              data: {
                ...base.data,
                label: meta.appName,
                color: meta.color,
                icon: "AG",
                bodyCount: visibleMemberIds.length,
                hideSourceHandle: true,
                onToggleCollapse: () => toggleAppCollapse(groupId),
              },
            });
          } else {
            // Expanded — emit synthetic parent + anchor as child
            const parentPosition = existing?.position ?? { x: node.position.x, y: node.position.y };

            // Synthetic group parent node
            const existingGroup = prevMap.get(groupId);
            const parentIdx = nodes.length;
            nodes.push({
              id: groupId,
              type: "appGroup",
              position: existingGroup?.position ?? parentPosition,
              draggable: true,
              selected: false,
              data: {
                appName: meta.appName,
                color: meta.color,
                memberCount: visibleMemberIds.length,
                isActive: node.id === activeNode,
                onToggleCollapse: () => toggleAppCollapse(groupId),
              },
            });
            groupNodeIndices.set(groupId, parentIdx);
            expandedGroupChildren.set(groupId, []);

            // Anchor as child inside the group
            const anchorBase = toRFNode(node, selectedNode, activeNode, () => onDeleteNodes([node.id]), appKindMap.get(node.id), existing);
            const relativePosition = existing?.parentId === groupId
              ? existing.position
              : { x: APP_GROUP_PADDING, y: APP_GROUP_HEADER_HEIGHT + APP_GROUP_PADDING };
            const childNode = {
              ...anchorBase,
              parentId: groupId,
              extent: "parent" as const,
              position: relativePosition,
              style: { ...anchorBase.style, transition: "opacity 150ms ease 50ms" },
            };
            nodes.push(childNode);
            expandedGroupChildren.get(groupId)?.push(childNode);
          }
          continue;
        }

        // App group member nodes (non-anchor)
        // Skip if this node is a loop body member — loop takes precedence (spec edge case)
        const appGroup = nodeToAppGroup.get(node.id);
        if (appGroup && !appGroupAnchors.has(node.id) && !nodeToLoops.has(node.id)) {
          const base = toRFNode(node, selectedNode, activeNode, () => onDeleteNodes([node.id]), appKindMap.get(node.id), existing);

          if (collapsedApps.has(appGroup)) {
            nodes.push({ ...base, hidden: true });
          } else {
            const meta = appGroupMeta.get(appGroup);
            const anchorNode = meta ? wfNodeMap.get(meta.anchorId) : undefined;

            let relativePosition = base.position;
            if (existing?.parentId === appGroup) {
              relativePosition = existing.position;
            } else if (anchorNode) {
              relativePosition = {
                x: node.position.x - anchorNode.position.x + APP_GROUP_PADDING,
                y: node.position.y - anchorNode.position.y + APP_GROUP_HEADER_HEIGHT + APP_GROUP_PADDING,
              };
            }

            const childNode = {
              ...base,
              parentId: appGroup,
              extent: "parent" as const,
              position: relativePosition,
              style: { ...base.style, transition: "opacity 150ms ease 50ms" },
            };
            nodes.push(childNode);
            expandedGroupChildren.get(appGroup)?.push(childNode);
          }
          continue;
        }

        // Body nodes of a loop
        const parentLoops = nodeToLoops.get(node.id);
        if (parentLoops && parentLoops.length > 0) {
          const base = toRFNode(node, selectedNode, activeNode, () => onDeleteNodes([node.id]), appKindMap.get(node.id), existing);

          const anyCollapsed = parentLoops.some((lid) => collapsedLoops.has(lid));
          if (anyCollapsed) {
            nodes.push({ ...base, hidden: true });
          } else {
            const parentId = parentLoops[parentLoops.length - 1];
            const loopWfNode = wfNodeMap.get(parentId);

            let relativePosition = base.position;
            if (existing?.parentId === parentId) {
              relativePosition = existing.position;
            } else if (loopWfNode) {
              relativePosition = {
                x: node.position.x - loopWfNode.position.x + LOOP_PADDING,
                y: node.position.y - loopWfNode.position.y + LOOP_HEADER_HEIGHT + LOOP_PADDING,
              };
            }

            const childNode: RFNode = {
              ...base,
              parentId,
              extent: "parent" as const,
              position: relativePosition,
              style: {
                ...base.style,
                transition: "opacity 150ms ease 50ms",
              },
            };
            nodes.push(childNode);
            expandedGroupChildren.get(parentId)?.push(childNode);
          }
          continue;
        }

        // Regular node
        const base = toRFNode(node, selectedNode, activeNode, () => onDeleteNodes([node.id]), appKindMap.get(node.id), existing);
        nodes.push(base);
      }

      // Size each expanded group node to contain all its children, then center them
      for (const [groupId, children] of expandedGroupChildren) {
        const idx = groupNodeIndices.get(groupId);
        if (idx === undefined) continue;
        const groupNode = nodes[idx];
        const gc = groupConstants(groupNode.type ?? "loopGroup");

        let maxX = 0;
        let maxY = 0;
        let maxChildW = 0;
        for (const child of children) {
          const measured = prevMap.get(child.id)?.measured;
          const childW = measured?.width ?? APPROX_NODE_WIDTH;
          const childH = measured?.height ?? APPROX_NODE_HEIGHT;
          maxX = Math.max(maxX, child.position.x + childW);
          maxY = Math.max(maxY, child.position.y + childH);
          maxChildW = Math.max(maxChildW, childW);
        }

        const containerW = Math.max(MIN_GROUP_WIDTH, maxX + gc.padding);
        groupNode.style = {
          ...groupNode.style,
          width: containerW,
          height: Math.max(MIN_GROUP_HEIGHT, maxY + gc.padding),
        };

        // Center children horizontally within the container
        const centerX = (containerW - maxChildW) / 2;
        if (centerX > gc.padding) {
          for (const child of children) {
            // Only center on initial layout (when child hasn't been manually positioned)
            if (!prevMap.get(child.id)?.parentId) {
              child.position = { x: centerX, y: child.position.y };
            }
          }
        }
      }

      // ── Second pass: user group rendering ──────────────────────────
      // Runs after auto-groups (loops, app groups) are resolved.
      // Reassigns rendered nodes into user group containers or collapses them into pills.
      const nodeIndexById = new Map<string, number>();
      for (let i = 0; i < nodes.length; i++) nodeIndexById.set(nodes[i].id, i);

      // Pre-build reverse map: anchor node ID → app group ID
      const anchorToAppGroup = new Map<string, string>();
      for (const [agId, agMeta] of appGroupMeta) {
        anchorToAppGroup.set(agMeta.anchorId, agId);
      }

      for (const group of workflow.groups ?? []) {
        const meta = userGroupMeta.get(group.id);
        if (!meta) continue;
        if (group.node_ids.length === 0) continue;

        // Skip collapsed groups whose parent user group is also collapsed
        if (meta.parentGroupId && collapsedUserGroups.has(meta.parentGroupId)) continue;

        const anchorId = meta.anchorId;
        const anchorIdx = nodeIndexById.get(anchorId);

        if (collapsedUserGroups.has(group.id)) {
          // ── Collapsed: convert anchor to pill, hide all other members ──
          if (anchorIdx !== undefined) {
            const anchorNode = nodes[anchorIdx];
            nodes[anchorIdx] = {
              ...anchorNode,
              type: "workflow",
              data: {
                ...anchorNode.data,
                label: meta.name,
                color: meta.color,
                icon: "\uD83D\uDCC1",
                bodyCount: meta.flatMemberCount,
                isUserGroupPill: true,
                userGroupId: group.id,
                isRenaming: renamingGroupId === group.id,
                onRenameConfirm: (newName: string) => onRenameConfirm(group.id, newName),
                onRenameCancel,
                onToggleCollapse: () => toggleUserGroupCollapse(group.id),
              },
            };
            // Preserve the anchor's existing parentId (e.g., if inside an auto-group)
          }

          // Hide all non-anchor members
          for (const nodeId of group.node_ids) {
            if (nodeId === anchorId) continue;
            const idx = nodeIndexById.get(nodeId);
            if (idx !== undefined) {
              nodes[idx] = { ...nodes[idx], hidden: true };
            }
            // Also hide synthetic app group containers whose anchor is a member
            const agId = anchorToAppGroup.get(nodeId);
            if (agId) {
              const agIdx = nodeIndexById.get(agId);
              if (agIdx !== undefined) nodes[agIdx] = { ...nodes[agIdx], hidden: true };
            }
          }
        } else {
          // ── Expanded: create synthetic container, reparent members ──
          const anchorNode = anchorIdx !== undefined ? nodes[anchorIdx] : undefined;
          const existingGroupNode = prevMap.get(group.id);

          // Compute anchor's absolute position: if anchor is inside an auto-group
          // (has parentId pointing to an app group), its position is relative —
          // add the parent's position. Skip when parent is a user group (set by
          // a previous iteration) to avoid double-offset when subgroup is
          // reparented back into that user group.
          let anchorAbsPosition = anchorNode?.position ?? { x: 0, y: 0 };
          const anchorParentIsAutoGroup = anchorNode?.parentId
            ? appGroups.has(anchorNode.parentId)
            : false;
          if (anchorParentIsAutoGroup && !existingGroupNode) {
            const parentIdx = nodeIndexById.get(anchorNode!.parentId!);
            const parentNode = parentIdx !== undefined ? nodes[parentIdx] : undefined;
            if (parentNode) {
              anchorAbsPosition = {
                x: anchorAbsPosition.x + parentNode.position.x,
                y: anchorAbsPosition.y + parentNode.position.y,
              };
            }
          }

          const containerPosition = existingGroupNode?.position
            ?? anchorAbsPosition;

          // Determine if the user group should be inside an auto-group.
          // Only check actual auto-group IDs (appGroups keys), NOT user group parents
          // which may have been set by a previous iteration of this second pass.
          const anchorAutoParent = anchorNode?.parentId;
          const isAutoGroupParent = anchorAutoParent ? appGroups.has(anchorAutoParent) : false;

          let containerParentId: string | undefined;
          if (anchorAutoParent && isAutoGroupParent) {
            const autoGroupMembers = appGroups.get(anchorAutoParent) ?? [];
            const userGroupNodeSet = new Set(group.node_ids);
            const autoGroupFullyWrapped = autoGroupMembers.every((m) => userGroupNodeSet.has(m));
            if (autoGroupFullyWrapped) {
              // User group wraps the auto-group — user group is the outer container
              const autoGroupIdx = nodeIndexById.get(anchorAutoParent!);
              const autoGroupNode = autoGroupIdx !== undefined ? nodes[autoGroupIdx] : undefined;
              containerParentId = autoGroupNode?.parentId;
            } else {
              // User group is inside the auto-group
              containerParentId = anchorAutoParent;
            }
          } else if (anchorAutoParent && !isAutoGroupParent) {
            // Parent is a user group (set by earlier iteration) — subgroup stays inside parent
            containerParentId = anchorAutoParent;
          }

          const containerIdx = nodes.length;
          nodes.push({
            id: group.id,
            type: "userGroup",
            position: containerPosition,
            parentId: containerParentId,
            extent: containerParentId ? "parent" as const : undefined,
            draggable: true,
            selected: false,
            data: {
              name: meta.name,
              color: meta.color,
              memberCount: meta.flatMemberCount,
              isRenaming: renamingGroupId === group.id,
              onRenameConfirm: (newName: string) => onRenameConfirm(group.id, newName),
              onRenameCancel,
              onToggleCollapse: () => toggleUserGroupCollapse(group.id),
            },
          });
          nodeIndexById.set(group.id, containerIdx);

          // Reparent all member nodes to the user group container
          const userGroupChildren: RFNode[] = [];
          for (const nodeId of group.node_ids) {
            const idx = nodeIndexById.get(nodeId);
            if (idx === undefined) continue;
            const memberNode = nodes[idx];
            if (memberNode.hidden) continue;

            let relativePosition: { x: number; y: number };
            if (memberNode.parentId === group.id) {
              // Already parented to this group in a previous render — keep position
              relativePosition = memberNode.position;
            } else if (anchorNode) {
              // Compute relative position from the anchor
              const memberAbsX = memberNode.position.x;
              const memberAbsY = memberNode.position.y;
              const anchorAbsX = anchorNode.position.x;
              const anchorAbsY = anchorNode.position.y;
              relativePosition = {
                x: memberAbsX - anchorAbsX + USER_GROUP_PADDING,
                y: memberAbsY - anchorAbsY + USER_GROUP_HEADER_HEIGHT + USER_GROUP_PADDING,
              };
            } else {
              relativePosition = { x: USER_GROUP_PADDING, y: USER_GROUP_HEADER_HEIGHT + USER_GROUP_PADDING };
            }

            nodes[idx] = {
              ...memberNode,
              parentId: group.id,
              extent: "parent" as const,
              position: relativePosition,
              style: { ...memberNode.style, transition: "opacity 150ms ease 50ms" },
            };
            userGroupChildren.push(nodes[idx]);

            // Also reparent any synthetic auto-group container whose anchor is this member
            const agId = anchorToAppGroup.get(nodeId);
            if (agId && !collapsedApps.has(agId)) {
              const agIdx = nodeIndexById.get(agId);
              if (agIdx !== undefined) {
                const agNode = nodes[agIdx];

                let agRelPos: { x: number; y: number };
                if (agNode.parentId === group.id) {
                  agRelPos = agNode.position;
                } else {
                  agRelPos = {
                    x: agNode.position.x - anchorAbsPosition.x + USER_GROUP_PADDING,
                    y: agNode.position.y - anchorAbsPosition.y + USER_GROUP_HEADER_HEIGHT + USER_GROUP_PADDING,
                  };
                }

                nodes[agIdx] = {
                  ...agNode,
                  parentId: group.id,
                  extent: "parent" as const,
                  position: agRelPos,
                };
                userGroupChildren.push(nodes[agIdx]);
              }
            }
          }

          // Size the user group container to fit its children
          let maxX = 0;
          let maxY = 0;
          for (const child of userGroupChildren) {
            const measured = prevMap.get(child.id)?.measured;
            const childW = measured?.width ?? (child.style?.width as number | undefined) ?? APPROX_NODE_WIDTH;
            const childH = measured?.height ?? (child.style?.height as number | undefined) ?? APPROX_NODE_HEIGHT;
            maxX = Math.max(maxX, child.position.x + childW);
            maxY = Math.max(maxY, child.position.y + childH);
          }

          nodes[containerIdx] = {
            ...nodes[containerIdx],
            style: {
              ...nodes[containerIdx].style,
              width: Math.max(MIN_GROUP_WIDTH, maxX + USER_GROUP_PADDING),
              height: Math.max(MIN_GROUP_HEIGHT, maxY + USER_GROUP_PADDING),
            },
          };
        }
      }

      // React Flow requires parent nodes before children in the array
      nodes.sort((a, b) => {
        const aHasParent = a.parentId ? 1 : 0;
        const bHasParent = b.parentId ? 1 : 0;
        return aHasParent - bHasParent;
      });

      return nodes;
    });
  }, [
    workflow.nodes,
    workflow.edges,
    workflow.groups,
    activeNode,
    onDeleteNodes,
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
  ]);

  // Sync external selectedNode changes into RF selection state
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

  const handleNodesChange: OnNodesChange = useCallback(
    (changes) => {
      let removeIds: string[] = [];
      for (const change of changes) {
        if (change.type === "remove") removeIds.push(change.id);
      }
      // Expand collapsed app group anchor deletions to include all members
      if (removeIds.length > 0) {
        const expanded: string[] = [];
        const expandedSet = new Set<string>();
        const addUnique = (m: string) => { if (!expandedSet.has(m)) { expandedSet.add(m); expanded.push(m); } };
        for (const id of removeIds) {
          // User group container deletion → expand to all member nodes
          const ugMeta = userGroupMeta.get(id);
          if (ugMeta) {
            const group = workflow.groups?.find((g) => g.id === id);
            if (group) {
              for (const m of group.node_ids) addUnique(m);
            }
            continue;
          }

          const groupId = nodeToAppGroup.get(id);
          const meta = groupId ? appGroupMeta.get(groupId) : undefined;
          if (meta?.anchorId === id && collapsedApps.has(groupId!)) {
            const members = appGroups.get(groupId!) ?? [];
            for (const m of members) addUnique(m);
          } else {
            addUnique(id);
          }
        }
        removeIds = expanded;
      }
      // Expand collapsed user group pill deletions to include all members
      if (removeIds.length > 0) {
        const expanded: string[] = [];
        const expandedSet = new Set<string>();
        const addUnique = (m: string) => { if (!expandedSet.has(m)) { expandedSet.add(m); expanded.push(m); } };
        for (const id of removeIds) {
          const ugId = nodeToUserGroup.get(id);
          const ugMeta = ugId ? userGroupMeta.get(ugId) : undefined;
          if (ugMeta?.anchorId === id && collapsedUserGroups.has(ugId!)) {
            const group = workflow.groups?.find((g) => g.id === ugId);
            if (group) {
              for (const m of group.node_ids) addUnique(m);
            } else {
              addUnique(id);
            }
          } else {
            addUnique(id);
          }
        }
        removeIds = expanded;
      }
      if (removeIds.length > 0) {
        // TIMING CONTRACT: deletedNodeIdsRef is set here and consumed by useEdgeSync's
        // handleEdgesChange. React Flow fires edge removal callbacks synchronously after
        // node removal callbacks. The queueMicrotask clears it for nodes with no edges.
        deletedNodeIdsRef.current = new Set(removeIds);
        onDeleteNodes(removeIds);
        queueMicrotask(() => { deletedNodeIdsRef.current = null; });
        return;
      }

      setRfNodes((prev) => {
        const prevParents = new Map(prev.map((n) => [n.id, n.parentId]));
        const updatedNodes = applyNodeChanges(changes, prev);

        // Prevent React Flow from auto-parenting nodes into groups they don't belong to.
        // Only allow parentId if it was already set (from our sync effect) or explicitly
        // part of the change set.
        for (const n of updatedNodes) {
          if (n.parentId && n.parentId !== prevParents.get(n.id)) {
            // Check if this node actually belongs to this group
            if (nodeToAppGroup.get(n.id) !== n.parentId && !nodeToLoops.get(n.id)?.includes(n.parentId) && nodeToUserGroup.get(n.id) !== n.parentId) {
              n.parentId = prevParents.get(n.id);
              n.extent = prevParents.get(n.id) ? "parent" as const : undefined;
            }
          }
        }

        const nodeMap = new Map(updatedNodes.map((n) => [n.id, n]));
        const posUpdates = new Map<string, { x: number; y: number }>();
        const affectedGroups = new Set<string>();
        let selectId: string | null = null;
        for (const change of changes) {
          if (change.type === "position" && change.position) {
            const rfNode = nodeMap.get(change.id);
            if (rfNode?.parentId) {
              affectedGroups.add(rfNode.parentId);
              const parentRfNode = nodeMap.get(rfNode.parentId);
              if (parentRfNode) {
                const { headerHeight, padding } = groupConstants(parentRfNode.type ?? "loopGroup");
                // Synthetic group containers need anchor remapping even as children
                const groupAnchor = userGroupMeta.get(change.id)?.anchorId ?? appGroupMeta.get(change.id)?.anchorId;
                const posKey = groupAnchor ?? change.id;
                posUpdates.set(posKey, {
                  x: change.position.x + parentRfNode.position.x - padding,
                  y: change.position.y + parentRfNode.position.y - headerHeight - padding,
                });
                // Persist children of nested synthetic group containers
                if (groupAnchor) {
                  for (const child of updatedNodes) {
                    if (child.parentId === change.id) {
                      const { headerHeight: ch, padding: cp } = groupConstants(rfNode.type ?? "userGroup");
                      const childAnchor = userGroupMeta.get(child.id)?.anchorId ?? appGroupMeta.get(child.id)?.anchorId ?? child.id;
                      posUpdates.set(childAnchor, {
                        x: child.position.x + change.position.x + parentRfNode.position.x - padding - cp,
                        y: child.position.y + change.position.y + parentRfNode.position.y - headerHeight - padding - ch - cp,
                      });
                    }
                  }
                }
              }
            } else {
              // If dragging a synthetic group parent, map position to its anchor node
              const meta = appGroupMeta.get(change.id) ?? (userGroupMeta.get(change.id) ? { anchorId: userGroupMeta.get(change.id)!.anchorId } : undefined);
              if (meta) {
                posUpdates.set(meta.anchorId, change.position);
              } else {
                posUpdates.set(change.id, change.position);
              }
              for (const child of updatedNodes) {
                if (child.parentId === change.id) {
                  const parentNode = nodeMap.get(change.id);
                  const { headerHeight: ph, padding: pp } = groupConstants(parentNode?.type ?? "loopGroup");
                  posUpdates.set(child.id, {
                    x: child.position.x + change.position.x - pp,
                    y: child.position.y + change.position.y - ph - pp,
                  });
                }
              }
            }
          } else if (change.type === "select" && change.selected) {
            selectId = change.id;
          } else if (change.type === "dimensions") {
            const rfNode = nodeMap.get(change.id);
            if (rfNode?.parentId) affectedGroups.add(rfNode.parentId);
          }
        }
        // Defer store updates to after the state updater returns to avoid
        // "Cannot update App while rendering GraphCanvas" (setState-during-render).
        if (posUpdates.size > 0 || selectId) {
          queueMicrotask(() => {
            if (posUpdates.size > 0) onNodePositionsChange(posUpdates);
            if (selectId) {
              selectionFromCanvasRef.current = true;
              onSelectNode(selectId);
            }
          });
        }

        // Resize loop groups when child dimensions or positions change
        if (affectedGroups.size > 0) {
          for (const groupId of affectedGroups) {
            const groupIdx = updatedNodes.findIndex((n) => n.id === groupId);
            if (groupIdx === -1) continue;
            const children = updatedNodes.filter((n) => n.parentId === groupId);
            let maxX = 0;
            let maxY = 0;
            for (const child of children) {
              const childW = child.measured?.width ?? APPROX_NODE_WIDTH;
              const childH = child.measured?.height ?? APPROX_NODE_HEIGHT;
              maxX = Math.max(maxX, child.position.x + childW);
              maxY = Math.max(maxY, child.position.y + childH);
            }
            const gc = groupConstants(updatedNodes[groupIdx].type ?? "loopGroup");
            updatedNodes[groupIdx] = {
              ...updatedNodes[groupIdx],
              style: {
                ...updatedNodes[groupIdx].style,
                width: Math.max(MIN_GROUP_WIDTH, maxX + gc.padding),
                height: Math.max(MIN_GROUP_HEIGHT, maxY + gc.padding),
              },
            };
          }
        }

        return updatedNodes;
      });
    },
    [onNodePositionsChange, onSelectNode, onDeleteNodes, collapsedApps, appGroups, nodeToAppGroup, appGroupMeta, nodeToLoops, nodeToUserGroup, userGroupMeta, collapsedUserGroups, workflow.groups],
  );

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
