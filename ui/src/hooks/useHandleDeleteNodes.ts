import { useCallback } from "react";
import { commands } from "../bindings";
import { useStore } from "../store/useAppStore";
import { buildDeletionAnnotation } from "../utils/deletionAnnotation";
import { REDACTED_SUMMARY } from "../utils/priorTurns";

type RemoveNodes = (ids: string[]) => void;

/**
 * Wrap the base workflow `removeNodes` with the conversational-agent
 * side-effects required by the spec (D1.C3 + D1.M3 + D1.M4):
 *
 * 1. Mutate the workflow via the underlying `removeNodes` callback.
 * 2. For the subset of deleted nodes that carry `source_run_id`
 *    (agent-built), invoke `prune_agent_cache_for_nodes` so the
 *    on-disk cache evicts entries produced by those nodes.
 * 3. Append a centered `system` chat annotation summarizing the
 *    deletion, grouped by run ID.
 * 4. Redact partial-turn assistant summaries and drop fully-gone
 *    turns from the chat transcript.
 *
 * Non-agent deletions fall through to the underlying `removeNodes`
 * with no assistant-side effect.
 */
export function useHandleDeleteNodes(removeNodes: RemoveNodes) {
  return useCallback(
    (ids: string[]) => {
      const pre = useStore.getState();
      const deletedAgentNodes = ids
        .map((id) => pre.workflow.nodes.find((n) => n.id === id))
        .filter((n): n is NonNullable<typeof n> => n != null)
        .filter(
          (n): n is typeof n & { source_run_id: string } =>
            (n as { source_run_id?: string | null }).source_run_id != null,
        )
        .map((n) => ({
          id: n.id,
          name: n.name,
          source_run_id: (n as { source_run_id: string }).source_run_id,
        }));

      // (a) Mutate the workflow first (history + auto-dissolve happen here).
      removeNodes(ids);

      if (deletedAgentNodes.length === 0) return;

      // (b) Group by run id for annotation + survival scan.
      const byRun = new Map<string, Array<{ id: string; name: string }>>();
      for (const n of deletedAgentNodes) {
        const list = byRun.get(n.source_run_id) ?? [];
        list.push({ id: n.id, name: n.name });
        byRun.set(n.source_run_id, list);
      }

      const annotation = buildDeletionAnnotation(byRun, pre.messages);
      useStore.getState().pushSystemAnnotation(annotation);

      // (c) Kick off cache prune. The Tauri command early-returns when
      //     `store_traces` is false (D1.M4) so fire-and-forget is safe.
      const {
        projectPath,
        workflow,
        storeTraces,
        setAssistantError,
      } = useStore.getState();
      void commands
        .pruneAgentCacheForNodes({
          project_path: projectPath,
          workflow_name: workflow.name,
          workflow_id: workflow.id,
          node_ids: deletedAgentNodes.map((n) => n.id),
          store_traces: storeTraces,
        })
        .catch((e: unknown) => {
          setAssistantError(`Failed to prune cache: ${String(e)}`);
        });

      // (d) Scan the *post-delete* workflow for surviving agent nodes
      //     per run id so we know which turns to redact vs. drop.
      const postWorkflow = useStore.getState().workflow;
      const survivingRunIds = new Set<string>();
      for (const n of postWorkflow.nodes) {
        const rid = (n as { source_run_id?: string | null }).source_run_id;
        if (rid) survivingRunIds.add(rid);
      }
      const partialRuns = new Set<string>();
      const fullyGoneRuns = new Set<string>();
      for (const runId of byRun.keys()) {
        if (survivingRunIds.has(runId)) partialRuns.add(runId);
        else fullyGoneRuns.add(runId);
      }
      if (partialRuns.size > 0) {
        useStore
          .getState()
          .mapMessagesByRunIds(partialRuns, (m) =>
            m.role === "assistant" ? { ...m, content: REDACTED_SUMMARY } : m,
          );
      }
      if (fullyGoneRuns.size > 0) {
        useStore.getState().dropTurnsByRunIds(fullyGoneRuns);
      }
    },
    [removeNodes],
  );
}
