import { useCallback } from "react";
import { commands } from "../bindings";
import { useStore } from "../store/useAppStore";
import { buildDeletionAnnotation } from "../utils/deletionAnnotation";
import { REDACTED_SUMMARY } from "../utils/priorTurns";
import { isAgentActive } from "../store/slices/agentSlice";

type RemoveNodes = (ids: string[]) => void;
type DeleteGroupWithContents = (groupId: string) => void;

/**
 * Apply conversational-agent side-effects for any deletion that
 * touched agent-built nodes. Runs AFTER the workflow mutation so the
 * post-delete scan sees the final node set.
 *
 * Shared between the plain React-Flow remove path and the
 * `Delete Group + Contents` context-menu path so neither escapes the
 * skill-lineage prune / annotation / redact contract.
 */
function applyDeletionSideEffects(
  deletedAgentNodes: Array<{ id: string; name: string; source_run_id: string }>,
  preMessages: ReturnType<typeof useStore.getState>["messages"],
) {
  if (deletedAgentNodes.length === 0) return;

  const byRun = new Map<string, Array<{ id: string; name: string }>>();
  for (const n of deletedAgentNodes) {
    const list = byRun.get(n.source_run_id) ?? [];
    list.push({ id: n.id, name: n.name });
    byRun.set(n.source_run_id, list);
  }

  const annotation = buildDeletionAnnotation(byRun, preMessages);
  useStore.getState().pushSystemAnnotation(annotation);

  const { projectPath, workflow, storeTraces, setAssistantError } =
    useStore.getState();
  void commands
    .pruneSkillLineageForNodes({
      project_path: projectPath,
      project_name: workflow.name,
      project_id: workflow.id,
      node_ids: deletedAgentNodes.map((n) => n.id),
      store_traces: storeTraces,
    })
    .catch((e: unknown) => {
      setAssistantError(`Failed to prune skill lineage: ${String(e)}`);
    });

  // Scan the post-delete workflow for surviving agent nodes per run
  // id so we know which turns to redact vs. drop.
  const postWorkflow = useStore.getState().workflow;
  const survivingRunIds = new Set<string>();
  for (const n of postWorkflow.nodes) {
    if (n.source_run_id) survivingRunIds.add(n.source_run_id);
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
}

/**
 * Collect agent-built nodes from `workflow` whose id is in `idSet`.
 * Shared between the plain delete path (ids from the React-Flow change
 * handler) and the group-delete path (ids derived from the group's
 * members + child subgroups).
 */
function collectAgentNodes(
  workflow: ReturnType<typeof useStore.getState>["workflow"],
  idSet: Set<string>,
): Array<{ id: string; name: string; source_run_id: string }> {
  return workflow.nodes
    .filter((n) => idSet.has(n.id) && n.source_run_id != null)
    .map((n) => ({
      id: n.id,
      name: n.name,
      source_run_id: n.source_run_id as string,
    }));
}

/**
 * Expand a group id into the full set of node ids that
 * `deleteGroupWithContents` will remove — direct members plus every
 * sub-group's members, matching `useWorkflowMutations.deleteGroupWithContents`.
 */
function expandGroupNodeIds(
  workflow: ReturnType<typeof useStore.getState>["workflow"],
  groupId: string,
): Set<string> {
  const group = (workflow.groups ?? []).find((g) => g.id === groupId);
  const allNodeIds = new Set<string>();
  if (!group) return allNodeIds;
  for (const id of group.node_ids) allNodeIds.add(id);
  for (const sub of workflow.groups ?? []) {
    if (sub.parent_group_id === groupId) {
      for (const id of sub.node_ids) allNodeIds.add(id);
    }
  }
  return allNodeIds;
}

/**
 * Wrap the base workflow `removeNodes` with the conversational-agent
 * side-effects required by the spec (D1.C3 + D1.M3 + D1.M4):
 *
 * 1. Mutate the workflow via the underlying `removeNodes` callback.
 * 2. For the subset of deleted nodes that carry `source_run_id`
 *    (agent-built), invoke `prune_skill_lineage_for_nodes` so draft
 *    skills drop lineage for deleted nodes.
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
      const idSet = new Set(ids);
      const deletedAgentNodes = collectAgentNodes(pre.workflow, idSet);
      removeNodes(ids);
      applyDeletionSideEffects(deletedAgentNodes, pre.messages);
    },
    [removeNodes],
  );
}

/**
 * Wrap `deleteGroupWithContents` with the same conversational
 * side-effects as `useHandleDeleteNodes`. The group's direct members
 * plus every sub-group's members are considered "deleted" — if any
 * of those nodes carry `source_run_id`, we prune their skill lineage
 * and annotate / redact the transcript accordingly. Also enforces
 * the mid-run delete gate so grouped deletions don't bypass the
 * safety rails that protect plain deletions.
 */
export function useHandleDeleteGroupWithContents(
  deleteGroupWithContents: DeleteGroupWithContents,
) {
  return useCallback(
    (groupId: string) => {
      const pre = useStore.getState();
      // Mid-run delete gate: match the React-Flow handler's rejection
      // path so grouped deletes honor the same D1.H3 contract.
      if (isAgentActive(pre.agentStatus, pre.completionDisagreement)) {
        pre.setAssistantError(
          "Cannot modify the graph while the agent is running — stop it first.",
        );
        window.setTimeout(() => {
          useStore.getState().setAssistantError(null);
        }, 4000);
        return;
      }
      const allNodeIds = expandGroupNodeIds(pre.workflow, groupId);
      const deletedAgentNodes = collectAgentNodes(pre.workflow, allNodeIds);
      deleteGroupWithContents(groupId);
      applyDeletionSideEffects(deletedAgentNodes, pre.messages);
    },
    [deleteGroupWithContents],
  );
}
