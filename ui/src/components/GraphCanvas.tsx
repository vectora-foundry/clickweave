import { useCallback, useMemo } from "react";
import {
  ReactFlow,
  Background,
  Controls,
  SelectionMode,
  type NodeTypes,
  MarkerType,
} from "@xyflow/react";
import "@xyflow/react/dist/style.css";
import type { Workflow, Edge } from "../bindings";
import { useLoopGrouping } from "../hooks/useLoopGrouping";
import { useAppGrouping } from "../hooks/useAppGrouping";
import { useNodeSync } from "../hooks/useNodeSync";
import { useEdgeSync } from "../hooks/useEdgeSync";
import { AppGroupNode } from "./AppGroupNode";
import { LoopGroupNode } from "./LoopGroupNode";
import { WorkflowNode } from "./WorkflowNode";

interface GraphCanvasProps {
  workflow: Workflow;
  selectedNode: string | null;
  activeNode: string | null;
  onSelectNode: (id: string | null) => void;
  onNodePositionsChange: (updates: Map<string, { x: number; y: number }>) => void;
  onEdgesChange: (edges: Edge[]) => void;
  onConnect: (from: string, to: string, sourceHandle?: string) => void;
  onDeleteNodes: (ids: string[]) => void;
  onRemoveExtraEdges: (edges: Edge[]) => void;
  onBeforeNodeDrag?: () => void;
}

export function GraphCanvas({
  workflow,
  selectedNode,
  activeNode,
  onSelectNode,
  onNodePositionsChange,
  onEdgesChange,
  onConnect,
  onDeleteNodes,
  onRemoveExtraEdges,
  onBeforeNodeDrag,
}: GraphCanvasProps) {
  const nodeTypes: NodeTypes = useMemo(
    () => ({ workflow: WorkflowNode, loopGroup: LoopGroupNode, appGroup: AppGroupNode }),
    [],
  );

  const loopState = useLoopGrouping(workflow);
  const appState = useAppGrouping(workflow);

  const { rfNodes, handleNodesChange, handleNodeDragStart, deletedNodeIdsRef } = useNodeSync({
    workflow,
    selectedNode,
    activeNode,
    ...loopState,
    collapsedApps: appState.collapsedApps,
    appGroups: appState.appGroups,
    nodeToAppGroup: appState.nodeToAppGroup,
    appGroupMeta: appState.appGroupMeta,
    toggleAppCollapse: appState.toggleAppCollapse,
    onSelectNode,
    onNodePositionsChange,
    onDeleteNodes,
    onBeforeNodeDrag,
  });

  const { rfEdges, handleEdgesChange, handleConnect } = useEdgeSync({
    workflow,
    hiddenNodeIds: loopState.hiddenNodeIds,
    collapsedLoops: loopState.collapsedLoops,
    collapsedAppEdgeRewrites: appState.collapsedAppEdgeRewrites,
    deletedNodeIdsRef,
    onEdgesChange,
    onRemoveExtraEdges,
    onConnect,
  });

  const handlePaneClick = useCallback(() => onSelectNode(null), [onSelectNode]);

  return (
    <div className="h-full w-full">
      <ReactFlow
        nodes={rfNodes}
        edges={rfEdges}
        nodeTypes={nodeTypes}
        onNodesChange={handleNodesChange}
        onEdgesChange={handleEdgesChange}
        onConnect={handleConnect}
        onNodeDragStart={handleNodeDragStart}
        onPaneClick={handlePaneClick}
        deleteKeyCode={["Backspace", "Delete"]}
        selectionOnDrag
        selectionMode={SelectionMode.Partial}
        panOnDrag={[1]}
        panOnScroll
        fitView
        fitViewOptions={{ maxZoom: 1 }}
        snapToGrid
        snapGrid={[20, 20]}
        defaultEdgeOptions={{
          type: "default",
          selectable: true,
          markerEnd: { type: MarkerType.ArrowClosed, color: "#666" },
          style: { stroke: "#555", strokeWidth: 2 },
        }}
        proOptions={{ hideAttribution: true }}
        style={{ background: "var(--bg-dark)" }}
      >
        <Background color="#333" gap={20} />
        <Controls
          showInteractive={false}
          style={{ background: "var(--bg-panel)", borderColor: "var(--border)" }}
        />
      </ReactFlow>
    </div>
  );
}
