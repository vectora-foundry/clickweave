import { useCallback, useMemo, useState } from "react";
import {
  ReactFlow,
  Background,
  Controls,
  MarkerType,
  type Edge as RFEdge,
  type Node as RFNode,
  type NodeTypes,
} from "@xyflow/react";
import "@xyflow/react/dist/style.css";
import { useShallow } from "zustand/react/shallow";
import { useStore } from "../store/useAppStore";
import { TraceStepNode, type TraceStepNodeData } from "./TraceStepNode";
import {
  TraceTerminalNode,
  type TraceTerminalNodeData,
} from "./TraceTerminalNode";
import { TraceSidePanel } from "./TraceSidePanel";

const NODE_TYPES: NodeTypes = {
  traceStep: TraceStepNode,
  traceTerminal: TraceTerminalNode,
};

const NODE_VERTICAL_GAP = 100;
const NODE_X = 0;

export function TraceCanvas() {
  const { agentRunId, trace } = useStore(
    useShallow((s) => {
      const id = s.agentRunId;
      return {
        agentRunId: id,
        trace: id ? s.runTraces[id] : undefined,
      };
    }),
  );

  const [expanded, setExpanded] = useState<Set<number>>(new Set());
  const [selectedStepIndex, setSelectedStepIndex] = useState<number | null>(
    null,
  );

  const toggleExpanded = useCallback((stepIndex: number) => {
    setExpanded((prev) => {
      const next = new Set(prev);
      if (next.has(stepIndex)) {
        next.delete(stepIndex);
      } else {
        next.add(stepIndex);
      }
      return next;
    });
  }, []);

  const { rfNodes, rfEdges } = useMemo(() => {
    if (!trace) return { rfNodes: [] as RFNode[], rfEdges: [] as RFEdge[] };

    const milestonesByStep = new Map<number, string>();
    for (const m of trace.milestones) {
      milestonesByStep.set(m.stepIndex, m.text);
    }
    const deltasByStep = new Map<number, string[]>();
    for (const d of trace.worldModelDeltas) {
      deltasByStep.set(d.stepIndex, d.changedFields);
    }

    const sortedSteps = [...trace.steps].sort(
      (a, b) => a.stepIndex - b.stepIndex,
    );

    const nodes: RFNode[] = sortedSteps.map((step, i) => ({
      id: `step-${step.stepIndex}`,
      type: "traceStep",
      position: { x: NODE_X, y: i * NODE_VERTICAL_GAP },
      data: {
        stepIndex: step.stepIndex,
        toolName: step.toolName,
        phase: step.phase,
        body: step.body,
        failed: step.failed,
        expanded: expanded.has(step.stepIndex),
        changedFields: deltasByStep.get(step.stepIndex) ?? [],
        milestoneText: milestonesByStep.get(step.stepIndex) ?? null,
        onToggle: toggleExpanded,
      } as TraceStepNodeData,
      selectable: true,
      draggable: false,
      connectable: false,
    }));

    if (trace.terminalFrame) {
      nodes.push({
        id: "terminal",
        type: "traceTerminal",
        position: {
          x: NODE_X,
          y: sortedSteps.length * NODE_VERTICAL_GAP,
        },
        data: {
          frame: trace.terminalFrame,
        } as TraceTerminalNodeData,
        selectable: false,
        draggable: false,
        connectable: false,
      });
    }

    const edges: RFEdge[] = [];
    for (let i = 0; i < sortedSteps.length - 1; i++) {
      const from = sortedSteps[i];
      const to = sortedSteps[i + 1];
      edges.push({
        id: `e-${from.stepIndex}-${to.stepIndex}`,
        source: `step-${from.stepIndex}`,
        target: `step-${to.stepIndex}`,
        selectable: false,
      });
    }
    if (trace.terminalFrame && sortedSteps.length > 0) {
      const last = sortedSteps[sortedSteps.length - 1];
      edges.push({
        id: `e-${last.stepIndex}-terminal`,
        source: `step-${last.stepIndex}`,
        target: "terminal",
        selectable: false,
      });
    }

    return { rfNodes: nodes, rfEdges: edges };
  }, [trace, expanded, toggleExpanded]);

  const onNodeClick = useCallback((_: unknown, node: RFNode) => {
    if (node.type === "traceStep") {
      const data = node.data as TraceStepNodeData;
      setSelectedStepIndex(data.stepIndex);
    }
  }, []);

  if (
    !agentRunId ||
    !trace ||
    (trace.steps.length === 0 && !trace.terminalFrame)
  ) {
    return (
      <div className="flex h-full w-full items-center justify-center bg-[var(--bg-dark)] text-sm text-[var(--text-muted)]">
        No active run yet. Start the agent to see its trace here.
      </div>
    );
  }

  return (
    <div className="relative h-full w-full" data-trace-canvas-wrapper>
      <ReactFlow
        nodes={rfNodes}
        edges={rfEdges}
        nodeTypes={NODE_TYPES}
        nodesDraggable={false}
        nodesConnectable={false}
        elementsSelectable
        fitView
        fitViewOptions={{ maxZoom: 1 }}
        defaultEdgeOptions={{
          type: "smoothstep",
          selectable: false,
          markerEnd: { type: MarkerType.ArrowClosed, color: "#666" },
          style: { stroke: "#555", strokeWidth: 2 },
        }}
        onNodeClick={onNodeClick}
        proOptions={{ hideAttribution: true }}
        style={{ background: "var(--bg-dark)" }}
      >
        <Background color="#333" gap={20} />
        <Controls
          showInteractive={false}
          style={{ background: "var(--bg-panel)", borderColor: "var(--border)" }}
        />
      </ReactFlow>

      <TraceSidePanel
        trace={trace}
        selectedStepIndex={selectedStepIndex}
        onClose={() => setSelectedStepIndex(null)}
      />
    </div>
  );
}
