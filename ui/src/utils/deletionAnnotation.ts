import type { AssistantMessage } from "../store/slices/assistantSlice";

/**
 * Build the `system` chat annotation summarizing what the user just
 * deleted. Grouped by the run ID each node was produced by; the
 * goal label is looked up from the matching `user` message.
 */
export function buildDeletionAnnotation(
  byRun: Map<string, Array<{ id: string; name: string }>>,
  messages: AssistantMessage[],
): string {
  const runLabels = new Map<string, string>();
  for (const runId of byRun.keys()) {
    const userMsg = messages.find(
      (m) => m.role === "user" && m.runId === runId,
    );
    const label = userMsg?.content ?? "previous run";
    runLabels.set(
      runId,
      label.length > 40 ? `${label.slice(0, 40)}...` : label,
    );
  }

  const totalNodes = Array.from(byRun.values()).reduce(
    (acc, arr) => acc + arr.length,
    0,
  );

  if (byRun.size === 1) {
    const firstEntry = byRun.entries().next().value;
    if (!firstEntry) return `Deleted ${totalNodes} node(s)`;
    const [runId, nodes] = firstEntry;
    const label = runLabels.get(runId) ?? "previous run";
    if (nodes.length === 1) {
      return `Deleted "${nodes[0].name}" from "${label}"`;
    }
    const names = nodes.map((n) => n.name).join(", ");
    return `Deleted ${nodes.length} nodes from "${label}" (${names})`;
  }

  const parts: string[] = [];
  for (const [runId, nodes] of byRun.entries()) {
    parts.push(`${nodes.length} from "${runLabels.get(runId) ?? runId}"`);
  }
  return `Deleted ${totalNodes} nodes: ${parts.join(", ")}`;
}
