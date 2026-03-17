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
  collapsedLoops: Set<string>;
  collapsedAppEdgeRewrites: Map<string, string>;
  deletedNodeIdsRef: React.MutableRefObject<Set<string> | null>;
  onEdgesChange: (edges: Edge[]) => void;
  onRemoveExtraEdges: (edges: Edge[]) => void;
  onConnect: (from: string, to: string, sourceHandle?: string) => void;
}

export function useEdgeSync({
  workflow,
  hiddenNodeIds,
  collapsedLoops,
  collapsedAppEdgeRewrites,
  deletedNodeIdsRef,
  onEdgesChange,
  onRemoveExtraEdges,
  onConnect,
}: UseEdgeSyncParams) {
  const rfEdges: RFEdge[] = useMemo(() => {
    // Step 1: Rewrite edges for collapsed app groups
    const rewritten: { from: string; to: string; output: EdgeOutput | null }[] = [];
    const seen = new Set<string>();

    for (const e of workflow.edges) {
      const from = collapsedAppEdgeRewrites.get(e.from) ?? e.from;
      const to = collapsedAppEdgeRewrites.get(e.to) ?? e.to;

      // Internal edge (both endpoints in same collapsed group) — skip
      if (from === to && collapsedAppEdgeRewrites.has(e.from)) continue;

      // Deduplicate rewritten edges
      const key = `${from}-${to}-${edgeOutputToHandle(e.output) ?? "default"}`;
      if (seen.has(key)) continue;
      seen.add(key);

      rewritten.push({ from, to, output: e.output });
    }

    // Step 2: Apply existing hidden-node and collapsed-loop filters
    return rewritten
      .filter((e) => {
        if (hiddenNodeIds.has(e.from) || hiddenNodeIds.has(e.to)) return false;
        if (e.output?.type === "LoopBody" && collapsedLoops.has(e.from)) return false;
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
  }, [workflow.edges, hiddenNodeIds, collapsedLoops, collapsedAppEdgeRewrites]);

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
        // Build reverse rewrite map for collapsed app groups
        const reverseRewrite = new Map<string, Set<string>>();
        for (const [memberId, anchorId] of collapsedAppEdgeRewrites) {
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
            const involvesGroup = collapsedAppEdgeRewrites.has(rfEdge.source)
              || collapsedAppEdgeRewrites.has(rfEdge.target);
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
          if (e.output?.type === "LoopBody" && collapsedLoops.has(e.from)) return true;
          // Internal edges within a collapsed group
          if (collapsedAppEdgeRewrites.has(e.from) && collapsedAppEdgeRewrites.has(e.to)) {
            const anchorFrom = collapsedAppEdgeRewrites.get(e.from);
            const anchorTo = collapsedAppEdgeRewrites.get(e.to);
            if (anchorFrom === anchorTo) return true;
          }
          // Edges whose display summary was explicitly deleted by the user — exclude them
          if (collapsedAppEdgeRewrites.has(e.from) || collapsedAppEdgeRewrites.has(e.to)) {
            const rewrittenFrom = collapsedAppEdgeRewrites.get(e.from) ?? e.from;
            const rewrittenTo = collapsedAppEdgeRewrites.get(e.to) ?? e.to;
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
    [workflow.edges, onEdgesChange, onRemoveExtraEdges, hiddenNodeIds, collapsedLoops, collapsedAppEdgeRewrites, rfEdgeState],
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
