import { type Dispatch, type MutableRefObject, type SetStateAction, useCallback } from "react";
import {
  type Node as RFNode,
  type OnNodesChange,
  applyNodeChanges,
} from "@xyflow/react";
import type { Workflow } from "../../bindings";
import type { AppGroupMeta } from "../useAppGrouping";
import type { UserGroupMeta } from "../useUserGrouping";
import {
  APPROX_NODE_WIDTH,
  APPROX_NODE_HEIGHT,
  MIN_GROUP_WIDTH,
  MIN_GROUP_HEIGHT,
  groupConstants,
} from "./nodeBuilders";

interface UseNodeChangeHandlerParams {
  workflow: Workflow;
  collapsedApps: Set<string>;
  appGroups: Map<string, string[]>;
  nodeToAppGroup: Map<string, string>;
  appGroupMeta: Map<string, AppGroupMeta>;
  collapsedUserGroups: Set<string>;
  nodeToUserGroup: Map<string, string>;
  userGroupMeta: Map<string, UserGroupMeta>;
  selectionFromCanvasRef: MutableRefObject<boolean>;
  deletedNodeIdsRef: MutableRefObject<Set<string> | null>;
  onSelectNode: (id: string | null) => void;
  onCanvasSelectionChange: (hasMulti: boolean) => void;
  onNodePositionsChange: (updates: Map<string, { x: number; y: number }>) => void;
  onDeleteNodes: (ids: string[]) => void;
  setRfNodes: Dispatch<SetStateAction<RFNode[]>>;
}

/**
 * Handles ReactFlow onNodesChange events: position updates, selection
 * changes, deletion (with group expansion), and group resize on child
 * dimension changes.
 */
export function useNodeChangeHandler({
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
}: UseNodeChangeHandlerParams): OnNodesChange {
  return useCallback(
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
        // Deleting a selection removes those nodes from the workflow; any
        // canvas-only selection state we were tracking is now stale, so
        // drop it before the Escape handler can consume a phantom flag.
        onCanvasSelectionChange(false);
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
            if (nodeToAppGroup.get(n.id) !== n.parentId && nodeToUserGroup.get(n.id) !== n.parentId) {
              n.parentId = prevParents.get(n.id);
              n.extent = prevParents.get(n.id) ? "parent" as const : undefined;
            }
          }
        }

        const nodeMap = new Map(updatedNodes.map((n) => [n.id, n]));
        const posUpdates = new Map<string, { x: number; y: number }>();
        const affectedGroups = new Set<string>();
        let hasSelectChange = false;
        for (const change of changes) {
          if (change.type === "position" && change.position) {
            const rfNode = nodeMap.get(change.id);
            if (rfNode?.parentId) {
              affectedGroups.add(rfNode.parentId);
              const parentRfNode = nodeMap.get(rfNode.parentId);
              if (parentRfNode) {
                const { headerHeight, padding } = groupConstants(parentRfNode.type ?? "appGroup");
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
                  const { headerHeight: ph, padding: pp } = groupConstants(parentNode?.type ?? "appGroup");
                  posUpdates.set(child.id, {
                    x: child.position.x + change.position.x - pp,
                    y: child.position.y + change.position.y - ph - pp,
                  });
                }
              }
            }
          } else if (change.type === "select") {
            hasSelectChange = true;
          } else if (change.type === "dimensions") {
            const rfNode = nodeMap.get(change.id);
            if (rfNode?.parentId) affectedGroups.add(rfNode.parentId);
          }
        }
        // Resolve selection from the post-change RF node state. The detail
        // modal opens only when a single workflow node is the sole selection;
        // any extra selection (multiple nodes, or a group container) means
        // the modal must stay closed and the Escape handler needs a
        // non-`selectedNode` signal to clear the canvas.
        //   hasOther = true when there is ANY selection on canvas that
        //              `selectedNode` does not represent.
        let nextSelectedId: string | null = null;
        let hasOtherSelection = false;
        if (hasSelectChange) {
          let totalSelected = 0;
          let soleWorkflowId: string | null = null;
          for (const n of updatedNodes) {
            if (!n.selected) continue;
            totalSelected += 1;
            if (totalSelected === 1 && n.type === "workflow") {
              soleWorkflowId = n.id;
            } else {
              soleWorkflowId = null;
            }
            if (totalSelected > 1) break;
          }
          nextSelectedId = totalSelected === 1 ? soleWorkflowId : null;
          hasOtherSelection = totalSelected > 0 && nextSelectedId === null;
        }

        // Defer store updates to after the state updater returns to avoid
        // "Cannot update App while rendering GraphCanvas" (setState-during-render).
        if (posUpdates.size > 0 || hasSelectChange) {
          queueMicrotask(() => {
            if (posUpdates.size > 0) onNodePositionsChange(posUpdates);
            if (hasSelectChange) {
              selectionFromCanvasRef.current = true;
              onSelectNode(nextSelectedId);
              onCanvasSelectionChange(hasOtherSelection);
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
            const gc = groupConstants(updatedNodes[groupIdx].type ?? "appGroup");
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
    [onNodePositionsChange, onSelectNode, onCanvasSelectionChange, onDeleteNodes, collapsedApps, appGroups, nodeToAppGroup, appGroupMeta, nodeToUserGroup, userGroupMeta, collapsedUserGroups, workflow.groups, setRfNodes, selectionFromCanvasRef, deletedNodeIdsRef],
  );
}
