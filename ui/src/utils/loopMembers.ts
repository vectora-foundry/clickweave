import type { Node, Edge } from "../bindings";

/**
 * Compute loop body members. Returns an empty map since control-flow
 * (Loop/EndLoop) nodes have been removed.
 */
export function computeLoopMembers(
  _nodes: Node[],
  _edges: Edge[],
): Map<string, string[]> {
  return new Map();
}
