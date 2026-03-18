import type { Workflow, NodeGroup } from "../bindings";

export interface ValidationResult {
  valid: boolean;
  error?: string;
  parentGroupId?: string | null;
}

/**
 * Check if the given node IDs form a connected induced subgraph of the
 * workflow DAG, treating edges as undirected. Requires at least 2 nodes.
 */
export function isConnectedSubgraph(nodeIds: string[], workflow: Workflow): boolean {
  if (nodeIds.length < 2) return false;

  const memberSet = new Set(nodeIds);

  // Build an undirected adjacency list restricted to the induced subgraph.
  const adj = new Map<string, string[]>();
  for (const id of nodeIds) {
    adj.set(id, []);
  }
  for (const edge of workflow.edges) {
    if (memberSet.has(edge.from) && memberSet.has(edge.to)) {
      adj.get(edge.from)!.push(edge.to);
      adj.get(edge.to)!.push(edge.from);
    }
  }

  // BFS from the first node.
  const visited = new Set<string>();
  const queue: string[] = [nodeIds[0]];
  visited.add(nodeIds[0]);

  while (queue.length > 0) {
    const current = queue.shift()!;
    for (const neighbor of adj.get(current) ?? []) {
      if (!visited.has(neighbor)) {
        visited.add(neighbor);
        queue.push(neighbor);
      }
    }
  }

  return visited.size === memberSet.size;
}

/**
 * Validate a proposed group creation.
 *
 * Rules:
 * - At least 2 nodes required.
 * - Selected nodes must form a connected induced subgraph.
 * - No partial overlap with any auto-group (loop or app) — all or nothing.
 * - No partial overlap with any existing user group — all or nothing.
 * - Nesting depth is capped at 2: a subgroup (group with a parent) cannot
 *   itself become a parent.
 *
 * Returns `{ valid: true, parentGroupId }` on success, where `parentGroupId`
 * is the id of the existing group that fully contains the selection (if any),
 * or null if the selection is at the top level.
 */
export function validateGroupCreation(
  selectedIds: string[],
  workflow: Workflow,
  existingGroups: NodeGroup[],
  loopMembers: Map<string, string[]>,
  appGroups: Map<string, string[]>,
): ValidationResult {
  if (selectedIds.length < 2) {
    return { valid: false, error: "Select at least 2 nodes to create a group." };
  }

  if (!isConnectedSubgraph(selectedIds, workflow)) {
    return { valid: false, error: "Selected nodes must form a connected subgraph." };
  }

  const selectedSet = new Set(selectedIds);

  // Determine the deepest existing user group that fully contains the selection.
  // This candidate becomes the parent of the new group (if any).
  // Do this before overlap checks so nesting violations are reported with a
  // clear message rather than a misleading "partial overlap" message.
  let parentGroupId: string | undefined = undefined;

  for (const group of existingGroups) {
    const groupSet = new Set(group.node_ids);
    const selectionIsInsideGroup = selectedIds.every((id) => groupSet.has(id));
    if (selectionIsInsideGroup) {
      // Prefer the most specific (smallest) containing group.
      if (parentGroupId === undefined) {
        parentGroupId = group.id;
      } else {
        const currentParent = existingGroups.find((g) => g.id === parentGroupId)!;
        if (group.node_ids.length < currentParent.node_ids.length) {
          parentGroupId = group.id;
        }
      }
    }
  }

  // Nesting depth check: the resolved parent must not itself be a subgroup.
  if (parentGroupId !== undefined) {
    const parent = existingGroups.find((g) => g.id === parentGroupId)!;
    if (parent.parent_group_id !== null) {
      return {
        valid: false,
        error: "Nesting beyond 2 levels is not allowed.",
      };
    }
  }

  // Check partial overlap with loop auto-groups.
  // "Partial overlap" = the selection intersects the auto-group but neither
  // fully contains it nor is fully contained within it.
  for (const [, memberIds] of loopMembers) {
    const autoGroupSet = new Set(memberIds);
    const overlappingCount = memberIds.filter((id) => selectedSet.has(id)).length;
    const selectionFullyInside = selectedIds.every((id) => autoGroupSet.has(id));
    const selectionFullyContains = overlappingCount === memberIds.length;
    if (overlappingCount > 0 && !selectionFullyInside && !selectionFullyContains) {
      return {
        valid: false,
        error: "Partial overlap with a loop group is not allowed. Include all or none of its nodes.",
      };
    }
  }

  // Check partial overlap with app auto-groups.
  for (const [, memberIds] of appGroups) {
    const autoGroupSet = new Set(memberIds);
    const overlappingCount = memberIds.filter((id) => selectedSet.has(id)).length;
    const selectionFullyInside = selectedIds.every((id) => autoGroupSet.has(id));
    const selectionFullyContains = overlappingCount === memberIds.length;
    if (overlappingCount > 0 && !selectionFullyInside && !selectionFullyContains) {
      return {
        valid: false,
        error: "Partial overlap with an app group is not allowed. Include all or none of its nodes.",
      };
    }
  }

  // Check partial overlap with existing user groups.
  for (const group of existingGroups) {
    const groupSet = new Set(group.node_ids);
    const overlappingCount = group.node_ids.filter((id) => selectedSet.has(id)).length;
    const selectionFullyInside = selectedIds.every((id) => groupSet.has(id));
    const selectionFullyContains = overlappingCount === group.node_ids.length;
    if (overlappingCount > 0 && !selectionFullyInside && !selectionFullyContains) {
      return {
        valid: false,
        error: `Partial overlap with group "${group.name}" is not allowed. Include all or none of its nodes.`,
      };
    }
  }

  return { valid: true, parentGroupId };
}

/**
 * Sort the given group member IDs in topological order within the workflow DAG.
 * Only edges between nodes in the provided set are considered.
 * Uses Kahn's algorithm.
 */
export function topologicalSortMembers(nodeIds: string[], workflow: Workflow): string[] {
  const memberSet = new Set(nodeIds);

  // Build adjacency list and in-degree map restricted to the induced subgraph.
  const inDegree = new Map<string, number>();
  const adj = new Map<string, string[]>();
  for (const id of nodeIds) {
    inDegree.set(id, 0);
    adj.set(id, []);
  }

  for (const edge of workflow.edges) {
    if (memberSet.has(edge.from) && memberSet.has(edge.to)) {
      adj.get(edge.from)!.push(edge.to);
      inDegree.set(edge.to, (inDegree.get(edge.to) ?? 0) + 1);
    }
  }

  // Kahn's algorithm: start with all zero-in-degree nodes.
  const queue: string[] = [];
  for (const id of nodeIds) {
    if (inDegree.get(id) === 0) {
      queue.push(id);
    }
  }

  const sorted: string[] = [];
  while (queue.length > 0) {
    const current = queue.shift()!;
    sorted.push(current);
    for (const neighbor of adj.get(current) ?? []) {
      const newDegree = (inDegree.get(neighbor) ?? 0) - 1;
      inDegree.set(neighbor, newDegree);
      if (newDegree === 0) {
        queue.push(neighbor);
      }
    }
  }

  // If there's a cycle (shouldn't happen in a DAG workflow), append remaining.
  for (const id of nodeIds) {
    if (!sorted.includes(id)) {
      sorted.push(id);
    }
  }

  return sorted;
}
