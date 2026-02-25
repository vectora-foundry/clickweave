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
  deletedNodeIdsRef: React.MutableRefObject<Set<string> | null>;
  onEdgesChange: (edges: Edge[]) => void;
  onRemoveExtraEdges: (edges: Edge[]) => void;
  onConnect: (from: string, to: string, sourceHandle?: string) => void;
}

export function useEdgeSync({
  workflow,
  hiddenNodeIds,
  collapsedLoops,
  deletedNodeIdsRef,
  onEdgesChange,
  onRemoveExtraEdges,
  onConnect,
}: UseEdgeSyncParams) {
  const rfEdges: RFEdge[] = useMemo(
    () =>
      workflow.edges
        .filter((edge) => {
          if (hiddenNodeIds.has(edge.from) || hiddenNodeIds.has(edge.to)) return false;
          if (edge.output?.type === "LoopBody" && collapsedLoops.has(edge.from)) return false;
          return true;
        })
        .map((edge) => ({
          id: `${edge.from}-${edge.to}-${edgeOutputToHandle(edge.output) ?? "default"}`,
          source: edge.from,
          target: edge.to,
          sourceHandle: edgeOutputToHandle(edge.output),
          label: getEdgeLabel(edge.output),
          labelStyle: { fill: "var(--text-muted)", fontSize: 10 },
          labelBgStyle: { fill: "var(--bg-panel)", opacity: 0.8 },
        })),
    [workflow.edges, hiddenNodeIds, collapsedLoops],
  );

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
        const updated = applyEdgeChanges(removals, rfEdgeState);
        const visibleEdges: Edge[] = updated.map((rfe) => {
          const handle = rfe.sourceHandle ?? undefined;
          const original = workflow.edges.find(
            (e) =>
              e.from === rfe.source &&
              e.to === rfe.target &&
              edgeOutputToHandle(e.output) === handle,
          );
          return { from: rfe.source, to: rfe.target, output: original?.output ?? null };
        });
        const hiddenEdges = workflow.edges.filter((edge) => {
          if (hiddenNodeIds.has(edge.from) || hiddenNodeIds.has(edge.to)) return true;
          if (edge.output?.type === "LoopBody" && collapsedLoops.has(edge.from)) return true;
          return false;
        });
        onEdgesChange([...visibleEdges, ...hiddenEdges]);
      }
      setRfEdgeState((prev) => applyEdgeChanges(changes, prev));
    },
    [workflow.edges, onEdgesChange, onRemoveExtraEdges, hiddenNodeIds, collapsedLoops, rfEdgeState],
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
