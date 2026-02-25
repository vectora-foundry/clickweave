import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { Workflow } from "../bindings";
import { computeLoopMembers } from "../utils/loopMembers";

export function useLoopGrouping(workflow: Workflow) {
  const [collapsedLoops, setCollapsedLoops] = useState<Set<string>>(new Set());

  const loopMembers = useMemo(
    () => computeLoopMembers(workflow.nodes, workflow.edges),
    [workflow.nodes, workflow.edges],
  );

  // Invert: for each body node, which loops is it in?
  const nodeToLoops = useMemo(() => {
    const map = new Map<string, string[]>();
    for (const [loopId, bodyIds] of loopMembers) {
      for (const bodyId of bodyIds) {
        const loops = map.get(bodyId) ?? [];
        loops.push(loopId);
        map.set(bodyId, loops);
      }
    }
    return map;
  }, [loopMembers]);

  // Set of EndLoop node IDs — always hidden
  const endLoopIds = useMemo(() => {
    const ids = new Set<string>();
    for (const n of workflow.nodes) {
      if (n.node_type.type === "EndLoop") ids.add(n.id);
    }
    return ids;
  }, [workflow.nodes]);

  // Map from loop ID to its EndLoop node ID (for cascade delete)
  const endLoopForLoop = useMemo(() => {
    const map = new Map<string, string>();
    for (const n of workflow.nodes) {
      if (n.node_type.type === "EndLoop") {
        map.set(n.node_type.loop_id, n.id);
      }
    }
    return map;
  }, [workflow.nodes]);

  const toggleLoopCollapse = useCallback((loopId: string) => {
    setCollapsedLoops((prev) => {
      const next = new Set(prev);
      if (next.has(loopId)) next.delete(loopId);
      else next.add(loopId);
      return next;
    });
  }, []);

  // Default new loops to collapsed
  const knownLoopsRef = useRef<Set<string>>(new Set());
  useEffect(() => {
    const currentLoopIds = new Set(loopMembers.keys());
    const newLoops: string[] = [];
    for (const loopId of currentLoopIds) {
      if (!knownLoopsRef.current.has(loopId)) newLoops.push(loopId);
    }
    setCollapsedLoops((prev) => {
      const next = new Set(prev);
      for (const loopId of newLoops) next.add(loopId);
      for (const id of next) {
        if (!currentLoopIds.has(id)) next.delete(id);
      }
      return next;
    });
    knownLoopsRef.current = currentLoopIds;
  }, [loopMembers]);

  // Build set of hidden node IDs for edge filtering
  const hiddenNodeIds = useMemo(() => {
    const ids = new Set<string>(endLoopIds);
    for (const [nodeId, parentLoops] of nodeToLoops) {
      if (parentLoops.some((lid) => collapsedLoops.has(lid))) {
        ids.add(nodeId);
      }
    }
    return ids;
  }, [endLoopIds, nodeToLoops, collapsedLoops]);

  return {
    collapsedLoops,
    loopMembers,
    nodeToLoops,
    endLoopIds,
    endLoopForLoop,
    toggleLoopCollapse,
    hiddenNodeIds,
  };
}
