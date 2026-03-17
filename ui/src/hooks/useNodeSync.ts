import { useCallback, useEffect, useRef, useState } from "react";
import {
  type Node as RFNode,
  type OnNodesChange,
  applyNodeChanges,
} from "@xyflow/react";
import type { AppKind, Workflow } from "../bindings";
import { usesCdp } from "../utils/appKind";
import { nodeMetadata, defaultNodeMetadata } from "../constants/nodeMetadata";
import { buildDag, type DagGraph } from "../utils/appGroupComputation";
import type { AppGroupMeta } from "./useAppGrouping";

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

/** Return layout constants for a group node type. */
function groupConstants(parentType: string): { headerHeight: number; padding: number } {
  if (parentType === "appGroup") return { headerHeight: APP_GROUP_HEADER_HEIGHT, padding: APP_GROUP_PADDING };
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
      const kind = (node.node_type as { app_kind?: AppKind }).app_kind ?? "Native";
      result.set(id, kind);
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
                bodyCount: memberIds.length,
                hideSourceHandle: true,
                onToggleCollapse: () => toggleAppCollapse(groupId),
              },
            });
          } else {
            // Expanded — emit synthetic parent + anchor as child
            const parentPosition = existing?.position ?? { x: node.position.x, y: node.position.y };

            // Synthetic group parent node
            const parentIdx = nodes.length;
            nodes.push({
              id: groupId,
              type: "appGroup",
              position: parentPosition,
              selected: false,
              data: {
                appName: meta.appName,
                color: meta.color,
                memberCount: memberIds.length,
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
            const childNode: typeof anchorBase = {
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

            const childNode: typeof base = {
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

      // Size each expanded loop group node to contain all its children
      for (const [loopId, children] of expandedGroupChildren) {
        const idx = groupNodeIndices.get(loopId);
        if (idx === undefined) continue;
        const groupNode = nodes[idx];

        let maxX = 0;
        let maxY = 0;
        for (const child of children) {
          const measured = prevMap.get(child.id)?.measured;
          const childW = measured?.width ?? APPROX_NODE_WIDTH;
          const childH = measured?.height ?? APPROX_NODE_HEIGHT;
          maxX = Math.max(maxX, child.position.x + childW);
          maxY = Math.max(maxY, child.position.y + childH);
        }

        groupNode.style = {
          ...groupNode.style,
          width: Math.max(MIN_GROUP_WIDTH, maxX + LOOP_PADDING),
          height: Math.max(MIN_GROUP_HEIGHT, maxY + LOOP_PADDING),
        };
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
  ]);

  // Sync external selectedNode changes into RF selection state
  useEffect(() => {
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
        for (const id of removeIds) {
          const groupId = nodeToAppGroup.get(id);
          const meta = groupId ? appGroupMeta.get(groupId) : undefined;
          if (meta?.anchorId === id && collapsedApps.has(groupId!)) {
            const members = appGroups.get(groupId!) ?? [];
            for (const m of members) {
              if (!expanded.includes(m)) expanded.push(m);
            }
          } else {
            expanded.push(id);
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
        const updatedNodes = applyNodeChanges(changes, prev);

        const nodeMap = new Map(updatedNodes.map((n) => [n.id, n]));
        const posUpdates = new Map<string, { x: number; y: number }>();
        const affectedGroups = new Set<string>();
        for (const change of changes) {
          if (change.type === "position" && change.position) {
            const rfNode = nodeMap.get(change.id);
            if (rfNode?.parentId) {
              affectedGroups.add(rfNode.parentId);
              const parentRfNode = nodeMap.get(rfNode.parentId);
              if (parentRfNode) {
                const { headerHeight, padding } = groupConstants(parentRfNode.type ?? "loopGroup");
                posUpdates.set(change.id, {
                  x: change.position.x + parentRfNode.position.x - padding,
                  y: change.position.y + parentRfNode.position.y - headerHeight - padding,
                });
              }
            } else {
              posUpdates.set(change.id, change.position);
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
            selectionFromCanvasRef.current = true;
            onSelectNode(change.id);
          } else if (change.type === "dimensions") {
            const rfNode = nodeMap.get(change.id);
            if (rfNode?.parentId) affectedGroups.add(rfNode.parentId);
          }
        }
        if (posUpdates.size > 0) onNodePositionsChange(posUpdates);

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
    [onNodePositionsChange, onSelectNode, onDeleteNodes, collapsedApps, appGroups, nodeToAppGroup, appGroupMeta],
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
