import { type Dispatch, type SetStateAction, useEffect } from "react";
import type { Node as RFNode } from "@xyflow/react";
import type { Workflow } from "../../bindings";
import type { AppGroupMeta } from "../useAppGrouping";
import type { UserGroupMeta } from "../useUserGrouping";
import {
  LOOP_HEADER_HEIGHT,
  LOOP_PADDING,
  APP_GROUP_HEADER_HEIGHT,
  APP_GROUP_PADDING,
  USER_GROUP_HEADER_HEIGHT,
  USER_GROUP_PADDING,
  APPROX_NODE_WIDTH,
  APPROX_NODE_HEIGHT,
  MIN_GROUP_WIDTH,
  MIN_GROUP_HEIGHT,
  groupConstants,
  buildAppKindMap,
  toRFNode,
} from "./nodeBuilders";

interface UseRfNodeBuilderParams {
  workflow: Workflow;
  selectedNode: string | null;
  activeNode: string | null;
  collapsedLoops: Set<string>;
  loopMembers: Map<string, string[]>;
  nodeToLoops: Map<string, string[]>;
  endLoopIds: Set<string>;
  endLoopForLoop: Map<string, string>;
  toggleLoopCollapse: (loopId: string) => void;
  collapsedApps: Set<string>;
  appGroups: Map<string, string[]>;
  nodeToAppGroup: Map<string, string>;
  appGroupMeta: Map<string, AppGroupMeta>;
  toggleAppCollapse: (groupId: string) => void;
  collapsedUserGroups: Set<string>;
  nodeToUserGroup: Map<string, string>;
  userGroupMeta: Map<string, UserGroupMeta>;
  toggleUserGroupCollapse: (groupId: string) => void;
  renamingGroupId: string | null;
  onRenameConfirm: (groupId: string, newName: string) => void;
  onRenameCancel: () => void;
  onDeleteNodes: (ids: string[]) => void;
  setRfNodes: Dispatch<SetStateAction<RFNode[]>>;
}

/**
 * Syncs workflow nodes into ReactFlow node state.
 *
 * Handles loop groups, app groups, user groups, collapsed/expanded states,
 * and parent-child relationships. Runs whenever workflow structure or
 * grouping state changes.
 */
export function useRfNodeBuilder({
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
}: UseRfNodeBuilderParams) {
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
    setRfNodes,
  ]);
}
