import { useMemo } from "react";
import type { ComponentType } from "react";
import { ReactFlow, Background } from "@xyflow/react";
import { useShallow } from "zustand/react/shallow";
import { useStore } from "../../store/useAppStore";
import { WorkflowNode } from "../WorkflowNode";
import { AppGroupNode } from "../AppGroupNode";
import { UserGroupNode } from "../UserGroupNode";
import { AgentRunGroupNode } from "../AgentRunGroupNode";
import { toRFNode, buildAppKindMap } from "../../hooks/node-sync/nodeBuilders";
import "@xyflow/react/dist/style.css";

/**
 * D12 — dedicated read-only React Flow renderer for the Overview's
 * Canvas Preview. Reuses `WorkflowNode` / `AppGroupNode` /
 * `UserGroupNode` / `AgentRunGroupNode` so the rounded-tile node
 * visual stays identical to the editor, but every interactive flag
 * is disabled. The custom node components themselves still mount
 * their internal `useEffect` hooks (no side-effect mutations
 * verified at write time); a `pointer-events: none` wrapper around
 * each prevents stray click handlers from firing.
 *
 * Critical: do NOT modify `GraphCanvas.tsx` to add a `readOnly` prop —
 * the editor and preview have diverged enough that a single component
 * gated on a flag would tangle the listener wiring.
 */

const nonInteractive = <P extends object>(C: ComponentType<P>) => {
  const Wrapped = (props: P) => (
    <div style={{ pointerEvents: "none" }}>
      <C {...props} />
    </div>
  );
  Wrapped.displayName = `NonInteractive(${C.displayName ?? C.name ?? "Component"})`;
  return Wrapped;
};

// MUST mirror the keys `GraphCanvas.tsx:154-158` registers. The
// `agent_run_group` key is snake_case (matches the `type` value
// emitted by `useRfNodeBuilder`); `appGroup` and `userGroup` are
// camelCase. Do NOT change either.
const PREVIEW_NODE_TYPES = {
  workflow: nonInteractive(WorkflowNode as unknown as ComponentType<object>),
  appGroup: nonInteractive(AppGroupNode as unknown as ComponentType<object>),
  userGroup: nonInteractive(UserGroupNode as unknown as ComponentType<object>),
  agent_run_group: nonInteractive(
    AgentRunGroupNode as unknown as ComponentType<object>,
  ),
};

export function CanvasPreviewCanvas() {
  const { workflow } = useStore(
    useShallow((s) => ({ workflow: s.workflow })),
  );
  const appKindMap = useMemo(() => buildAppKindMap(workflow), [workflow]);

  // Reuse the editor's projection so custom nodes receive the full
  // data payload they expect (label, app_kind, role, autoId, etc.).
  // Selection / activeNode / onDelete are wired to inert values:
  // nothing in the preview can be selected, set active, or deleted.
  const noop = () => {};
  const rfNodes = useMemo(
    () =>
      workflow.nodes.map((n) =>
        toRFNode(n, null, null, noop, appKindMap.get(n.id)),
      ),
    [workflow.nodes, appKindMap],
  );
  const rfEdges = useMemo(
    () =>
      workflow.edges.map((e) => ({
        id: `${e.from}-${e.to}`,
        source: e.from,
        target: e.to,
      })),
    [workflow.edges],
  );

  return (
    <div className="h-full w-full">
      <ReactFlow
        nodes={rfNodes}
        edges={rfEdges}
        nodeTypes={PREVIEW_NODE_TYPES}
        nodesDraggable={false}
        nodesConnectable={false}
        elementsSelectable={false}
        panOnDrag={false}
        zoomOnScroll={false}
        zoomOnPinch={false}
        zoomOnDoubleClick={false}
        fitView
        proOptions={{ hideAttribution: true }}
      >
        <Background gap={24} size={1} color="rgb(var(--bone) / 0.04)" />
      </ReactFlow>
    </div>
  );
}
