import { useMemo } from "react";
import type { Edge } from "@xyflow/react";
import type { Node as WfNode } from "../bindings";

/** Derive data-carrying edges from OutputRef params in the workflow. */
export function useDataEdges(nodes: WfNode[]): Edge[] {
  return useMemo(() => {
    // Build auto_id -> node UUID lookup
    const autoIdToUuid = new Map<string, string>();
    for (const node of nodes) {
      if (node.auto_id) autoIdToUuid.set(node.auto_id, node.id);
    }

    const edges: Edge[] = [];

    for (const node of nodes) {
      const inner = Object.values(node.node_type)[0] as Record<string, unknown> | undefined;
      if (!inner) continue;

      // Scan for _ref fields
      for (const [key, val] of Object.entries(inner)) {
        if (!key.endsWith("_ref") || val == null) continue;
        const ref = val as { node: string; field: string };
        const sourceUuid = autoIdToUuid.get(ref.node);
        if (!sourceUuid) continue;

        edges.push({
          id: `data-${ref.node}-${ref.field}-${node.id}-${key}`,
          source: sourceUuid,
          target: node.id,
          sourceHandle: `data-${ref.field}`,
          targetHandle: `data-input-${key}`,
          type: "dataEdge",
          data: { fieldType: "Object", fieldName: ref.field },
          animated: false,
          deletable: false,
          focusable: false,
          selectable: false,
        });
      }
    }

    return edges;
  }, [nodes]);
}
