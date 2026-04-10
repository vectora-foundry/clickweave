import { useCallback, useEffect, useMemo, useState } from "react";
import {
  type Edge as RFEdge,
  type OnEdgesChange,
  type OnConnect,
  type Connection,
  applyEdgeChanges,
} from "@xyflow/react";
import type { Workflow, Edge } from "../bindings";

interface UseEdgeSyncParams {
  workflow: Workflow;
  hiddenNodeIds: Set<string>;
  collapsedAppEdgeRewrites: Map<string, string>;
  collapsedUserGroupEdgeRewrites: Map<string, string>;
  deletedNodeIdsRef: React.MutableRefObject<Set<string> | null>;
  onEdgesChange: (edges: Edge[]) => void;
  onRemoveExtraEdges: (edges: Edge[]) => void;
  onConnect: (from: string, to: string, sourceHandle?: string) => void;
  onDataConnect?: (sourceNodeId: string, targetNodeId: string, sourceField: string, targetInputKey: string) => void;
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
  onDataConnect,
}: UseEdgeSyncParams) {
  const combinedRewrites = useMemo(() => {
    const map = new Map<string, string>();
    for (const [k, v] of collapsedAppEdgeRewrites) map.set(k, v);
    for (const [k, v] of collapsedUserGroupEdgeRewrites) map.set(k, v);
    return map;
  }, [collapsedAppEdgeRewrites, collapsedUserGroupEdgeRewrites]);

  const rfEdges: RFEdge[] = useMemo(() => {
    const rewritten: { from: string; to: string }[] = [];
    const seen = new Set<string>();

    for (const e of workflow.edges) {
      const from = combinedRewrites.get(e.from) ?? e.from;
      const to = combinedRewrites.get(e.to) ?? e.to;
      if (from === to && combinedRewrites.has(e.from)) continue;

      const key = `${from}-${to}`;
      if (seen.has(key)) continue;
      seen.add(key);

      rewritten.push({ from, to });
    }

    return rewritten
      .filter((e) => !hiddenNodeIds.has(e.from) && !hiddenNodeIds.has(e.to))
      .map((e) => ({
        id: `${e.from}-${e.to}`,
        source: e.from,
        target: e.to,
      }));
  }, [workflow.edges, hiddenNodeIds, combinedRewrites]);

  const [rfEdgeState, setRfEdgeState] = useState<RFEdge[]>([]);
  useEffect(() => {
    setRfEdgeState(rfEdges);
  }, [rfEdges]);

  const handleEdgesChange: OnEdgesChange = useCallback(
    (changes) => {
      const removals = changes.filter((c) => c.type === "remove");

      if (deletedNodeIdsRef.current) {
        const deletedIds = deletedNodeIdsRef.current;
        deletedNodeIdsRef.current = null;

        if (removals.length > 0) {
          const extraEdges: Edge[] = [];
          for (const removal of removals) {
            const rfEdge = rfEdgeState.find((e) => e.id === removal.id);
            if (rfEdge && !deletedIds.has(rfEdge.source) && !deletedIds.has(rfEdge.target)) {
              const original = workflow.edges.find(
                (e) => e.from === rfEdge.source && e.to === rfEdge.target,
              );
              if (original) extraEdges.push(original);
            }
          }
          if (extraEdges.length > 0) onRemoveExtraEdges(extraEdges);
        }
        setRfEdgeState((prev) => applyEdgeChanges(changes, prev));
        return;
      }

      if (removals.length > 0) {
        const reverseRewrite = new Map<string, Set<string>>();
        for (const [memberId, anchorId] of combinedRewrites) {
          const anchors = reverseRewrite.get(anchorId) ?? new Set();
          anchors.add(memberId);
          reverseRewrite.set(anchorId, anchors);
        }

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
          let original = workflow.edges.find(
            (e) => e.from === rfe.source && e.to === rfe.target,
          );
          if (!original) {
            const sourceMembers = reverseRewrite.get(rfe.source);
            const targetMembers = reverseRewrite.get(rfe.target);
            if (sourceMembers || targetMembers) {
              original = workflow.edges.find((e) => {
                const fromMatch = e.from === rfe.source || sourceMembers?.has(e.from);
                const toMatch = e.to === rfe.target || targetMembers?.has(e.to);
                return fromMatch && toMatch;
              });
            }
          }
          return original
            ? { from: original.from, to: original.to }
            : { from: rfe.source, to: rfe.target };
        });
        const recoveredKeys = new Set(
          visibleEdges.map((e) => `${e.from}-${e.to}`),
        );
        const hiddenEdges = workflow.edges.filter((e) => {
          if (hiddenNodeIds.has(e.from) || hiddenNodeIds.has(e.to)) return true;
          if (combinedRewrites.has(e.from) && combinedRewrites.has(e.to)) {
            const anchorFrom = combinedRewrites.get(e.from);
            const anchorTo = combinedRewrites.get(e.to);
            if (anchorFrom === anchorTo) return true;
          }
          if (combinedRewrites.has(e.from) || combinedRewrites.has(e.to)) {
            const rewrittenFrom = combinedRewrites.get(e.from) ?? e.from;
            const rewrittenTo = combinedRewrites.get(e.to) ?? e.to;
            const displayId = `${rewrittenFrom}-${rewrittenTo}`;
            if (removedDisplayKeys.has(displayId)) return false;
            const key = `${e.from}-${e.to}`;
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
      if (!connection.source || !connection.target) return;
      const sh = connection.sourceHandle ?? "";
      const th = connection.targetHandle ?? "";
      if (sh.startsWith("data-") && th.startsWith("data-input-") && onDataConnect) {
        const sourceField = sh.slice("data-".length);
        const targetInputKey = th.slice("data-input-".length);
        onDataConnect(connection.source, connection.target, sourceField, targetInputKey);
      } else if (sh.startsWith("data-")) {
        return;
      } else {
        onConnect(connection.source, connection.target, sh || undefined);
      }
    },
    [onConnect, onDataConnect],
  );

  return {
    rfEdges: rfEdgeState,
    handleEdgesChange,
    handleConnect,
  };
}
