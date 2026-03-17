import type { Workflow } from "../bindings";

export function buildAppNameMap(workflow: Workflow): Map<string, string | null> {
  const result = new Map<string, string | null>();
  const nodeById = new Map(workflow.nodes.map((n) => [n.id, n]));

  const endLoopNodeIds = new Set(
    workflow.nodes.filter((n) => n.node_type.type === "EndLoop").map((n) => n.id),
  );

  const outgoing = new Map<string, string[]>();
  const inDegree = new Map<string, number>();
  for (const n of workflow.nodes) inDegree.set(n.id, 0);
  for (const e of workflow.edges) {
    // Skip EndLoop back-edges (EndLoop→Loop) and LoopDone edges (Loop→exit)
    // to avoid cycles that would prevent topological sort from completing.
    if (endLoopNodeIds.has(e.from)) continue;
    if (e.output?.type === "LoopDone") continue;
    const list = outgoing.get(e.from) ?? [];
    list.push(e.to);
    outgoing.set(e.from, list);
    inDegree.set(e.to, (inDegree.get(e.to) ?? 0) + 1);
  }

  const queue: string[] = [];
  for (const [id, deg] of inDegree) {
    if (deg === 0) queue.push(id);
  }

  let head = 0;
  while (head < queue.length) {
    const id = queue[head++];
    const n = nodeById.get(id);

    if (n?.node_type.type === "FocusWindow") {
      const nt = n.node_type as { method: string; value: string | null };
      if (nt.method === "AppName") {
        result.set(id, nt.value);
      } else {
        result.set(id, null);
      }
    }

    const name = result.get(id);
    const hasEntry = result.has(id);
    for (const target of outgoing.get(id) ?? []) {
      if (hasEntry && !result.has(target)) {
        result.set(target, name ?? null);
      }
      inDegree.set(target, (inDegree.get(target) ?? 0) - 1);
      if (inDegree.get(target) === 0) queue.push(target);
    }
  }

  return result;
}

export function computeAppMembers(
  workflow: Workflow,
  appNameMap: Map<string, string | null>,
): Map<string, string[]> {
  const endLoopNodeIds = new Set(
    workflow.nodes.filter((n) => n.node_type.type === "EndLoop").map((n) => n.id),
  );

  const outgoing = new Map<string, string[]>();
  const inDegree = new Map<string, number>();
  for (const n of workflow.nodes) inDegree.set(n.id, 0);
  for (const e of workflow.edges) {
    // Skip EndLoop back-edges (EndLoop→Loop) and LoopDone edges (Loop→exit)
    // to avoid cycles that would prevent topological sort from completing.
    if (endLoopNodeIds.has(e.from)) continue;
    if (e.output?.type === "LoopDone") continue;
    const list = outgoing.get(e.from) ?? [];
    list.push(e.to);
    outgoing.set(e.from, list);
    inDegree.set(e.to, (inDegree.get(e.to) ?? 0) + 1);
  }

  const queue: string[] = [];
  for (const [id, deg] of inDegree) {
    if (deg === 0) queue.push(id);
  }

  const groups = new Map<string, string[]>();
  const nodeGroupAnchor = new Map<string, string>();

  let head = 0;
  while (head < queue.length) {
    const id = queue[head++];
    const appName = appNameMap.get(id);
    const hasAppName = appNameMap.has(id) && appName != null;
    const nodeObj = workflow.nodes.find((n) => n.id === id);

    if (hasAppName) {
      if (
        nodeObj?.node_type.type === "FocusWindow" &&
        (nodeObj.node_type as { method: string }).method === "AppName"
      ) {
        const groupId = `appgroup-${id}`;
        groups.set(groupId, [id]);
        nodeGroupAnchor.set(id, id);
      } else {
        const upstreamAnchor = nodeGroupAnchor.get(id);
        if (upstreamAnchor) {
          const groupId = `appgroup-${upstreamAnchor}`;
          groups.get(groupId)?.push(id);
        }
      }
    }

    const currentAnchor = nodeGroupAnchor.get(id);
    for (const target of outgoing.get(id) ?? []) {
      if (currentAnchor && !nodeGroupAnchor.has(target)) {
        const targetAppName = appNameMap.get(target);
        const currentAppName = appNameMap.get(id);
        if (targetAppName != null && targetAppName === currentAppName) {
          nodeGroupAnchor.set(target, currentAnchor);
        }
      }
      inDegree.set(target, (inDegree.get(target) ?? 0) - 1);
      if (inDegree.get(target) === 0) queue.push(target);
    }
  }

  return groups;
}
