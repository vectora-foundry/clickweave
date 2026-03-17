import { useCallback } from "react";
import type { Node, NodeType } from "../../../../bindings";

type NodeUpdater = (patch: Partial<Node>) => void;

export function useNodeTypeUpdater(
  nodeType: NodeType,
  onUpdate: NodeUpdater,
): (patch: Record<string, unknown>) => void {
  return useCallback(
    (patch: Record<string, unknown>) => {
      onUpdate({ node_type: { ...nodeType, ...patch } as NodeType });
    },
    [nodeType, onUpdate],
  );
}
