import { useCallback } from "react";
import type { Edge, Node, NodeGroup, NodeType, Workflow } from "../bindings";
import { topologicalSortMembers } from "../utils/groupValidation";

/** Extract the variant name from a serde-tagged NodeType. */
function nodeTypeName(nodeType: Record<string, unknown>): string {
  return (nodeType as { type?: string }).type ?? "";
}

const AUTO_ID_BASE: Record<string, string> = {
  FindText: "find_text", FindImage: "find_image", FindApp: "find_app",
  TakeScreenshot: "take_screenshot", Click: "click", Hover: "hover",
  Drag: "drag", TypeText: "type_text", PressKey: "press_key",
  Scroll: "scroll", FocusWindow: "focus_window", LaunchApp: "launch_app",
  QuitApp: "quit_app", CdpWait: "cdp_wait", CdpClick: "cdp_click",
  CdpHover: "cdp_hover", CdpFill: "cdp_fill", CdpType: "cdp_type",
  CdpPressKey: "cdp_press_key", CdpNavigate: "cdp_navigate",
  CdpNewPage: "cdp_new_page", CdpClosePage: "cdp_close_page",
  CdpSelectPage: "cdp_select_page", CdpHandleDialog: "cdp_handle_dialog",
  AiStep: "ai_step", McpToolCall: "mcp_tool_call", AppDebugKitOp: "app_debug_kit_op",
};

function generateAutoId(
  typeName: string,
  counters: Record<string, number>,
): { autoId: string; base: string; counter: number } {
  const base = AUTO_ID_BASE[typeName] ?? typeName.toLowerCase().replace(/\s+/g, "_");
  const counter = (counters[base] ?? 0) + 1;
  return { autoId: `${base}_${counter}`, base, counter };
}

/** Count effective members: direct nodes (not in any child subgroup) + child subgroups.
 *  A parent with only subgroup members but ≥2 subgroups stays alive. */
function effectiveMemberCount(group: NodeGroup, allGroups: NodeGroup[]): number {
  const childSubgroups = allGroups.filter((sg) => sg.parent_group_id === group.id);
  const childNodeIds = new Set(childSubgroups.flatMap((sg) => sg.node_ids));
  const directMembers = group.node_ids.filter((id: string) => !childNodeIds.has(id));
  return directMembers.length + childSubgroups.length;
}

/** Cascade auto-dissolve: remove groups with <2 effective members, then promote
 *  orphaned subgroups to top-level and re-check. */
export function autoDissolveGroups(groups: Workflow["groups"]): NodeGroup[] {
  let result = [...(groups ?? [])];
  let changed = true;
  while (changed) {
    changed = false;
    const before = result.length;
    result = result.filter((g) => effectiveMemberCount(g, result) >= 2);
    if (result.length !== before) changed = true;
    // Promote orphaned subgroups to top-level instead of removing them
    const survivingIds = new Set(result.map((g) => g.id));
    for (let i = 0; i < result.length; i++) {
      if (result[i].parent_group_id && !survivingIds.has(result[i].parent_group_id!)) {
        result[i] = { ...result[i], parent_group_id: null };
        changed = true;
      }
    }
  }
  return result;
}

export function useWorkflowMutations(
  setWorkflow: React.Dispatch<React.SetStateAction<Workflow>>,
  setSelectedNode: React.Dispatch<React.SetStateAction<string | null>>,
  nodesLength: number,
  pushHistory: (label: string) => void,
) {
  const addNode = useCallback(
    (nodeType: NodeType) => {
      pushHistory("Add Node");
      const id = crypto.randomUUID();
      const offsetX = (nodesLength % 4) * 250;
      const offsetY = Math.floor(nodesLength / 4) * 150;
      setWorkflow((prev) => {
        const typeName = nodeTypeName(nodeType as unknown as Record<string, unknown>);
        const counters: Record<string, number> = Object.fromEntries(Object.entries(prev.next_id_counters ?? {}).filter((e): e is [string, number] => e[1] != null));
        const { autoId, base, counter } = generateAutoId(typeName, counters);
        counters[base] = counter;
        const node: Node = {
          id,
          node_type: nodeType,
          position: { x: 200 + offsetX, y: 150 + offsetY },
          name: nodeType.type === "AiStep" ? "AI Step" : nodeType.type.replace(/([A-Z])/g, " $1").trim(),
          enabled: true,
          timeout_ms: null,
          settle_ms: null,
          retries: 0,
          trace_level: "Minimal",
          role: "Default" as const,
          expected_outcome: null,
          auto_id: autoId,
        };
        return { ...prev, nodes: [...prev.nodes, node], next_id_counters: counters };
      });
      setSelectedNode(id);
    },
    [nodesLength, setWorkflow, setSelectedNode, pushHistory],
  );

  const removeNodes = useCallback(
    (ids: string[]) => {
      pushHistory(ids.length === 1 ? "Delete Node" : "Delete Nodes");
      const idSet = new Set(ids);
      setWorkflow((prev) => {
        const updatedGroups = (prev.groups ?? []).map((g) => ({
          ...g,
          node_ids: g.node_ids.filter((id) => !idSet.has(id)),
        }));
        return {
          ...prev,
          nodes: prev.nodes.filter((n) => !idSet.has(n.id)),
          edges: prev.edges.filter((e) => !idSet.has(e.from) && !idSet.has(e.to)),
          groups: autoDissolveGroups(updatedGroups),
        };
      });
      setSelectedNode((prev) => (prev !== null && idSet.has(prev) ? null : prev));
    },
    [setWorkflow, setSelectedNode, pushHistory],
  );

  /** Remove edges without pushing a separate history entry.
   *  Used when extra edges are deleted as part of a node-delete operation
   *  whose snapshot was already captured by removeNodes. */
  const removeEdgesOnly = useCallback(
    (edges: Edge[]) => {
      setWorkflow((prev) => ({
        ...prev,
        edges: prev.edges.filter(
          (e) =>
            !edges.some(
              (r) => e.from === r.from && e.to === r.to,
            ),
        ),
      }));
    },
    [setWorkflow],
  );

  const updateNodePositions = useCallback(
    (updates: Map<string, { x: number; y: number }>) => {
      setWorkflow((prev) => ({
        ...prev,
        nodes: prev.nodes.map((n) => {
          const pos = updates.get(n.id);
          return pos ? { ...n, position: { x: pos.x, y: pos.y } } : n;
        }),
      }));
    },
    [setWorkflow],
  );

  const updateNode = useCallback(
    (id: string, updates: Partial<Node>) => {
      pushHistory("Edit Node");
      setWorkflow((prev) => ({
        ...prev,
        nodes: prev.nodes.map((n) => (n.id === id ? { ...n, ...updates } : n)),
      }));
    },
    [setWorkflow, pushHistory],
  );

  const addEdge = useCallback(
    (from: string, to: string, sourceHandle?: string) => {
      pushHistory("Add Edge");
      setWorkflow((prev) => {
        // Replace existing edge from the same source
        const filtered = prev.edges.filter((e) => e.from !== from);
        const edge: Edge = { from, to };
        return { ...prev, edges: [...filtered, edge] };
      });
    },
    [setWorkflow, pushHistory],
  );

  const dataConnect = useCallback(
    (sourceNodeId: string, targetNodeId: string, sourceField: string, targetInputKey: string) => {
      pushHistory("Wire Variable");
      setWorkflow((prev) => {
        // Find source node auto_id
        const sourceNode = prev.nodes.find((n) => n.id === sourceNodeId);
        if (!sourceNode?.auto_id) return prev;
        const outputRef = { node: sourceNode.auto_id, field: sourceField };
        // Set the _ref param on the target node
        return {
          ...prev,
          nodes: prev.nodes.map((n) => {
            if (n.id !== targetNodeId) return n;
            return {
              ...n,
              node_type: { ...n.node_type, [targetInputKey]: outputRef },
            };
          }),
        };
      });
    },
    [setWorkflow, pushHistory],
  );

  const removeEdge = useCallback(
    (from: string, to: string) => {
      pushHistory("Remove Edge");
      setWorkflow((prev) => ({
        ...prev,
        edges: prev.edges.filter((e) => e.from !== from || e.to !== to),
      }));
    },
    [setWorkflow, pushHistory],
  );

  const createGroup = useCallback(
    (name: string, color: string, nodeIds: string[], parentGroupId: string | null = null) => {
      pushHistory("Create Group");
      const id = crypto.randomUUID();
      setWorkflow((prev) => ({
        ...prev,
        groups: [...(prev.groups ?? []), { id, name, color, node_ids: nodeIds, parent_group_id: parentGroupId }],
      }));
      return id;
    },
    [setWorkflow, pushHistory],
  );

  const removeGroup = useCallback(
    (groupId: string) => {
      pushHistory("Ungroup");
      setWorkflow((prev) => ({
        ...prev,
        groups: (prev.groups ?? []).filter((g) => g.id !== groupId && g.parent_group_id !== groupId),
      }));
    },
    [setWorkflow, pushHistory],
  );

  const deleteGroupWithContents = useCallback(
    (groupId: string) => {
      pushHistory("Delete Group");
      setWorkflow((prev) => {
        const group = (prev.groups ?? []).find((g) => g.id === groupId);
        if (!group) return prev;
        const allNodeIds = new Set(group.node_ids);
        for (const sub of prev.groups ?? []) {
          if (sub.parent_group_id === groupId) {
            for (const id of sub.node_ids) allNodeIds.add(id);
          }
        }
        return {
          ...prev,
          nodes: prev.nodes.filter((n) => !allNodeIds.has(n.id)),
          edges: prev.edges.filter((e) => !allNodeIds.has(e.from) && !allNodeIds.has(e.to)),
          groups: autoDissolveGroups(
            (prev.groups ?? [])
              .filter((g) => g.id !== groupId && g.parent_group_id !== groupId)
              .map((g) => ({
                ...g,
                node_ids: g.node_ids.filter((id) => !allNodeIds.has(id)),
              }))
          ),
        };
      });
      setSelectedNode(null);
    },
    [setWorkflow, setSelectedNode, pushHistory],
  );

  const renameGroup = useCallback(
    (groupId: string, name: string) => {
      pushHistory("Rename Group");
      setWorkflow((prev) => ({
        ...prev,
        groups: (prev.groups ?? []).map((g) => (g.id === groupId ? { ...g, name } : g)),
      }));
    },
    [setWorkflow, pushHistory],
  );

  const recolorGroup = useCallback(
    (groupId: string, color: string) => {
      pushHistory("Recolor Group");
      setWorkflow((prev) => ({
        ...prev,
        groups: (prev.groups ?? []).map((g) => (g.id === groupId ? { ...g, color } : g)),
      }));
    },
    [setWorkflow, pushHistory],
  );

  const addNodesToGroup = useCallback(
    (groupId: string, nodeIds: string[]) => {
      pushHistory("Add to Group");
      setWorkflow((prev) => ({
        ...prev,
        groups: (prev.groups ?? []).map((g) => {
          if (g.id !== groupId) return g;
          const allIds = [...g.node_ids, ...nodeIds];
          return { ...g, node_ids: topologicalSortMembers(allIds, prev) };
        }),
      }));
    },
    [setWorkflow, pushHistory],
  );

  const removeNodesFromGroup = useCallback(
    (groupId: string, nodeIds: string[]) => {
      pushHistory("Remove from Group");
      const removeSet = new Set(nodeIds);
      setWorkflow((prev) => {
        const updated = (prev.groups ?? []).map((g) =>
          g.id === groupId
            ? { ...g, node_ids: g.node_ids.filter((id) => !removeSet.has(id)) }
            : g,
        );
        return { ...prev, groups: autoDissolveGroups(updated) };
      });
    },
    [setWorkflow, pushHistory],
  );

  const removeNode = useCallback(
    (id: string) => removeNodes([id]),
    [removeNodes],
  );

  return {
    addNode, removeNode, removeNodes, removeEdgesOnly,
    updateNodePositions, updateNode, addEdge, dataConnect, removeEdge,
    createGroup, removeGroup, deleteGroupWithContents,
    renameGroup, recolorGroup, addNodesToGroup, removeNodesFromGroup,
  };
}
