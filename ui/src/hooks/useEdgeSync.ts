import { useCallback, useEffect, useMemo, useState } from "react";
import {
  type Edge as RFEdge,
  type OnEdgesChange,
  type OnConnect,
  type Connection,
  applyEdgeChanges,
} from "@xyflow/react";
import type { Workflow, Edge, EdgeOutput } from "../bindings";
import { edgeOutputToHandle } from "../utils/edgeHandles";

function getEdgeLabel(output: EdgeOutput | null): string | undefined {
  if (!output) return undefined;
  switch (output.type) {
    case "IfTrue": return "true";
    case "IfFalse": return "false";
    case "SwitchCase": return output.name;
    case "SwitchDefault": return "default";
    case "LoopBody": return "body";
    case "LoopDone": return "done";
  }
}

interface UseEdgeSyncParams {
  workflow: Workflow;
  hiddenNodeIds: Set<string>;
  collapsedAppEdgeRewrites: Map<string, string>;
  collapsedUserGroupEdgeRewrites: Map<string, string>;
  deletedNodeIdsRef: React.MutableRefObject<Set<string> | null>;
  onEdgesChange: (edges: Edge[]) => void;
  onRemoveExtraEdges: (edges: Edge[]) => void;
  onConnect: (from: string, to: string, sourceHandle?: string) => void;
}

export function useEdgeSync({
  workflow,
  hiddenNodeIds,
  collapsedAppEdgeRewrites,
  collapsedUserGroupEdgeRewrites,
  deletedNodeIdsRef,
  onEdgesChange,
  onRemoveExtraEdges,
  onConnect,
}: UseEdgeSyncParams) {
  // Stable combined rewrite map: app group rewrites first, then user group rewrites
  // (user group takes visual precedence as outermost container).
  const combinedRewrites = useMemo(() => {
    const map = new Map<string, string>();
    for (const [k, v] of collapsedAppEdgeRewrites) map.set(k, v);
    for (const [k, v] of collapsedUserGroupEdgeRewrites) map.set(k, v);
    return map;
  }, [collapsedAppEdgeRewrites, collapsedUserGroupEdgeRewrites]);

  const rfEdges: RFEdge[] = useMemo(() => {
    // Step 1: Rewrite edges for collapsed app groups and user groups
    const rewritten: { from: string; to: string; output: EdgeOutput | null }[] = [];
    const seen = new Set<string>();

    for (const e of workflow.edges) {
      // LoopBody edges are always hidden — containment communicates the relationship.
      // LoopDone edges are always visible — they show the loop exit connection.
      if (e.output?.type === "LoopBody") continue;

      const from = combinedRewrites.get(e.from) ?? e.from;
      const to = combinedRewrites.get(e.to) ?? e.to;

      // Internal edge (both endpoints in same collapsed group) — skip
      if (from === to && combinedRewrites.has(e.from)) continue;

      // Deduplicate rewritten edges
      const key = `${from}-${to}-${edgeOutputToHandle(e.output) ?? "default"}`;
      if (seen.has(key)) continue;
      seen.add(key);

      rewritten.push({ from, to, output: e.output });
    }

    // Step 2: Apply remaining hidden-node filter
    return rewritten
      .filter((e) => {
        if (hiddenNodeIds.has(e.from) || hiddenNodeIds.has(e.to)) return false;
        return true;
      })
      .map((e) => ({
        id: `${e.from}-${e.to}-${edgeOutputToHandle(e.output) ?? "default"}`,
        source: e.from,
        target: e.to,
        sourceHandle: edgeOutputToHandle(e.output),
        label: getEdgeLabel(e.output),
        labelStyle: { fill: "var(--text-muted)", fontSize: 10 },
        labelBgStyle: { fill: "var(--bg-panel)", opacity: 0.8 },
      }));
  }, [workflow.edges, hiddenNodeIds, combinedRewrites]);

  // Internal RF edge state — preserves selection state across renders
  const [rfEdgeState, setRfEdgeState] = useState<RFEdge[]>([]);
  useEffect(() => {
    setRfEdgeState(rfEdges);
  }, [rfEdges]);

  const handleEdgesChange: OnEdgesChange = useCallback(
    (changes) => {
      const removals = changes.filter((c) => c.type === "remove");

      // If a node deletion just happened (set by useNodeSync's handleNodesChange),
      // identify extra selected edges that need silent removal.
      if (deletedNodeIdsRef.current) {
        const deletedIds = deletedNodeIdsRef.current;
        deletedNodeIdsRef.current = null;

        if (removals.length > 0) {
          const extraEdges: Edge[] = [];
          for (const removal of removals) {
            const rfEdge = rfEdgeState.find((e) => e.id === removal.id);
            if (rfEdge && !deletedIds.has(rfEdge.source) && !deletedIds.has(rfEdge.target)) {
              const handle = rfEdge.sourceHandle ?? undefined;
              const original = workflow.edges.find(
                (e) =>
                  e.from === rfEdge.source &&
                  e.to === rfEdge.target &&
                  edgeOutputToHandle(e.output) === handle,
              );
              if (original) extraEdges.push(original);
            }
          }
          if (extraEdges.length > 0) onRemoveExtraEdges(extraEdges);
        }
        setRfEdgeState((prev) => applyEdgeChanges(changes, prev));
        return;
      }

      // Normal path — propagate removals to the workflow store
      if (removals.length > 0) {
        // Build reverse rewrite map for collapsed groups (app + user)
        const reverseRewrite = new Map<string, Set<string>>();
        for (const [memberId, anchorId] of combinedRewrites) {
          const anchors = reverseRewrite.get(anchorId) ?? new Set();
          anchors.add(memberId);
          reverseRewrite.set(anchorId, anchors);
        }

        // Track which display edge keys were explicitly removed by the user.
        // Any removed edge that involves a collapsed group member (source or target
        // is in the rewrite map) is a summary edge whose underlying originals should
        // also be removed, not preserved as hidden.
        const removedDisplayKeys = new Set<string>();
        for (const removal of removals) {
          const rfEdge = rfEdgeState.find((e) => e.id === removal.id);
          if (rfEdge) {
            const involvesGroup = combinedRewrites.has(rfEdge.source)
              || combinedRewrites.has(rfEdge.target);
            if (involvesGroup) {
              removedDisplayKeys.add(rfEdge.id);
            }
          }
        }

        const updated = applyEdgeChanges(removals, rfEdgeState);
        const visibleEdges: Edge[] = updated.map((rfe) => {
          const handle = rfe.sourceHandle ?? undefined;
          let original = workflow.edges.find(
            (e) =>
              e.from === rfe.source &&
              e.to === rfe.target &&
              edgeOutputToHandle(e.output) === handle,
          );
          if (!original) {
            const sourceMembers = reverseRewrite.get(rfe.source);
            const targetMembers = reverseRewrite.get(rfe.target);
            if (sourceMembers || targetMembers) {
              original = workflow.edges.find((e) => {
                const fromMatch = e.from === rfe.source || sourceMembers?.has(e.from);
                const toMatch = e.to === rfe.target || targetMembers?.has(e.to);
                return fromMatch && toMatch && edgeOutputToHandle(e.output) === handle;
              });
            }
          }
          // Use original edge endpoints (not rewritten display endpoints) to preserve topology
          return original
            ? { from: original.from, to: original.to, output: original.output }
            : { from: rfe.source, to: rfe.target, output: null };
        });
        // Build set of original edge keys that were recovered into visibleEdges
        const recoveredKeys = new Set(
          visibleEdges.map((e) => `${e.from}-${e.to}-${edgeOutputToHandle(e.output) ?? "default"}`),
        );
        const hiddenEdges = workflow.edges.filter((e) => {
          if (hiddenNodeIds.has(e.from) || hiddenNodeIds.has(e.to)) return true;
          if (e.output?.type === "LoopBody") return true;
          // Internal edges within a collapsed group
          if (combinedRewrites.has(e.from) && combinedRewrites.has(e.to)) {
            const anchorFrom = combinedRewrites.get(e.from);
            const anchorTo = combinedRewrites.get(e.to);
            if (anchorFrom === anchorTo) return true;
          }
          // Edges whose display summary was explicitly deleted by the user — exclude them
          if (combinedRewrites.has(e.from) || combinedRewrites.has(e.to)) {
            const rewrittenFrom = combinedRewrites.get(e.from) ?? e.from;
            const rewrittenTo = combinedRewrites.get(e.to) ?? e.to;
            const displayId = `${rewrittenFrom}-${rewrittenTo}-${edgeOutputToHandle(e.output) ?? "default"}`;
            if (removedDisplayKeys.has(displayId)) return false; // user deleted this — don't preserve
            // Edges deduped away during rewriting (not explicitly deleted)
            const key = `${e.from}-${e.to}-${edgeOutputToHandle(e.output) ?? "default"}`;
            if (!recoveredKeys.has(key)) return true;
          }
          return false;
        });
        onEdgesChange([...visibleEdges, ...hiddenEdges]);
      }
      setRfEdgeState((prev) => applyEdgeChanges(changes, prev));
    },
    [workflow.edges, onEdgesChange, onRemoveExtraEdges, hiddenNodeIds, combinedRewrites, rfEdgeState],
  );

  const handleConnect: OnConnect = useCallback(
    (connection: Connection) => {
      if (connection.source && connection.target) {
        onConnect(connection.source, connection.target, connection.sourceHandle ?? undefined);
      }
    },
    [onConnect],
  );

  return {
    rfEdges: rfEdgeState,
    handleEdgesChange,
    handleConnect,
  };
}
