import type { Workflow } from "../bindings";

export const APP_GROUP_ID_PREFIX = "appgroup-";

export interface DagGraph {
  nodeById: Map<string, Workflow["nodes"][number]>;
  outgoing: Map<string, string[]>;
  inDegree: Map<string, number>;
}

/**
 * Build a cycle-safe DAG from a workflow, skipping EndLoop back-edges.
 * When skipLoopDone is true, also skip LoopDone edges (needed for app group
 * computation where LoopDone back-edges can create cycles in test workflows).
 * buildAppKindMap needs LoopDone edges to propagate context past loops.
 */
export function buildDag(workflow: Workflow, opts?: { skipLoopDone?: boolean }): DagGraph {
  const skipLoopDone = opts?.skipLoopDone ?? false;
  const nodeById = new Map(workflow.nodes.map((n) => [n.id, n]));

  const endLoopNodeIds = new Set(
    workflow.nodes.filter((n) => n.node_type.type === "EndLoop").map((n) => n.id),
  );

  const outgoing = new Map<string, string[]>();
  const inDegree = new Map<string, number>();
  for (const n of workflow.nodes) inDegree.set(n.id, 0);
  for (const e of workflow.edges) {
    if (endLoopNodeIds.has(e.from)) continue;
    if (skipLoopDone && e.output?.type === "LoopDone") continue;
    const list = outgoing.get(e.from) ?? [];
    list.push(e.to);
    outgoing.set(e.from, list);
    inDegree.set(e.to, (inDegree.get(e.to) ?? 0) + 1);
  }

  return { nodeById, outgoing, inDegree };
}

export function buildAppNameMap(workflow: Workflow, dag?: DagGraph): Map<string, string | null> {
  const { nodeById, outgoing, inDegree: inDegreeOriginal } = dag ?? buildDag(workflow, { skipLoopDone: true });
  // Clone inDegree since we mutate it during the walk
  const inDegree = new Map(inDegreeOriginal);

  const result = new Map<string, string | null>();
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
  dag?: DagGraph,
): Map<string, string[]> {
  const { nodeById, outgoing, inDegree: inDegreeOriginal } = dag ?? buildDag(workflow, { skipLoopDone: true });
  const inDegree = new Map(inDegreeOriginal);

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
    const nodeObj = nodeById.get(id);

    if (hasAppName) {
      if (
        nodeObj?.node_type.type === "FocusWindow" &&
        (nodeObj.node_type as { method: string }).method === "AppName"
      ) {
        const groupId = `${APP_GROUP_ID_PREFIX}${id}`;
        groups.set(groupId, [id]);
        nodeGroupAnchor.set(id, id);
      } else {
        const upstreamAnchor = nodeGroupAnchor.get(id);
        if (upstreamAnchor) {
          const groupId = `${APP_GROUP_ID_PREFIX}${upstreamAnchor}`;
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
