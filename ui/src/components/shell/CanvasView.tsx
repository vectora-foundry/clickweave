import { useStore } from "../../store/useAppStore";
import { useShallow } from "zustand/react/shallow";
import { useMemo } from "react";
import { AssistantPanel } from "../AssistantPanel";
import { FloatingToolbar } from "../FloatingToolbar";
import { GraphCanvas } from "../GraphCanvas";
import { IntentEmptyState } from "../IntentEmptyState";
import { NodeDetailModal } from "../node-detail/NodeDetailModal";
import { NodePalette } from "../NodePalette";
import { SkillsPanel } from "../skills/SkillsPanel";
import { SkillDetailView } from "../skills/SkillDetailView";
import { WalkthroughPanel } from "../WalkthroughPanel";
import { useWorkflowActions } from "../../hooks/useWorkflowActions";
import {
  useHandleDeleteGroupWithContents,
  useHandleDeleteNodes,
} from "../../hooks/useHandleDeleteNodes";
import { buildAppKindMap } from "../../hooks/useNodeSync";
import { isWalkthroughBusy } from "../../store/slices/walkthroughSlice";

export function CanvasView() {
  const {
    workflow,
    projectPath,
    nodeTypes,
    isNewWorkflow,
    selectedNode,
    activeNode,
    canvasSelectionResetTick,
    sidebarCollapsed,
    nodeSearch,
    detailTab,
    executorState,
    lastRunStatus,
    executionMode,
    logsDrawerOpen,
    walkthroughStatus,
    walkthroughPanelOpen,
    walkthroughEventCount,
    selectedSkill,
    skillsAvailable,
    drawerOpen,
    assistantError,
    messages,
    agentStatus,
    agentRunId,
    storeTraces,
    skillsGlobalParticipation,
  } = useStore(
    useShallow((s) => ({
      workflow: s.workflow,
      projectPath: s.projectPath,
      nodeTypes: s.nodeTypes,
      isNewWorkflow: s.isNewWorkflow,
      selectedNode: s.selectedNode,
      activeNode: s.activeNode,
      canvasSelectionResetTick: s.canvasSelectionResetTick,
      sidebarCollapsed: s.sidebarCollapsed,
      nodeSearch: s.nodeSearch,
      detailTab: s.detailTab,
      executorState: s.executorState,
      lastRunStatus: s.lastRunStatus,
      executionMode: s.executionMode,
      logsDrawerOpen: s.logsDrawerOpen,
      walkthroughStatus: s.walkthroughStatus,
      walkthroughPanelOpen: s.walkthroughPanelOpen,
      walkthroughEventCount: s.walkthroughEvents.length,
      selectedSkill: s.selectedSkill,
      skillsAvailable: s.skillsEnabled && s.storeTraces,
      drawerOpen: s.assistantSurface === "drawer",
      assistantError: s.assistantError,
      messages: s.messages,
      agentStatus: s.agentStatus,
      agentRunId: s.agentRunId,
      storeTraces: s.storeTraces,
      skillsGlobalParticipation: s.skillsGlobalParticipation,
    })),
  );

  const setWorkflow = useStore((s) => s.setWorkflow);
  const selectNode = useStore((s) => s.selectNode);
  const setHasCanvasSelection = useStore((s) => s.setHasCanvasSelection);
  const setDetailTab = useStore((s) => s.setDetailTab);
  const toggleSidebar = useStore((s) => s.toggleSidebar);
  const toggleLogsDrawer = useStore((s) => s.toggleLogsDrawer);
  const setNodeSearch = useStore((s) => s.setNodeSearch);
  const pushHistory = useStore((s) => s.pushHistory);
  const setExecutionMode = useStore((s) => s.setExecutionMode);
  const runWorkflow = useStore((s) => s.runWorkflow);
  const stopWorkflow = useStore((s) => s.stopWorkflow);
  const setAssistantOpen = useStore((s) => s.setAssistantOpen);
  const toggleAssistant = useStore((s) => s.toggleAssistant);
  const setWalkthroughPanelOpen = useStore((s) => s.setWalkthroughPanelOpen);
  const skipIntentEntry = useStore((s) => s.skipIntentEntry);
  const startAgent = useStore((s) => s.startAgent);
  const loadSkillsForPanel = useStore((s) => s.loadSkillsForPanel);

  const {
    addNode,
    removeNodes,
    removeEdgesOnly,
    updateNodePositions,
    updateNode,
    addEdge,
    createGroup,
    removeGroup,
    deleteGroupWithContents,
    renameGroup,
    recolorGroup,
    addNodesToGroup,
    removeNodesFromGroup,
  } = useWorkflowActions();

  const handleDeleteNodes = useHandleDeleteNodes(removeNodes);
  const handleDeleteGroupWithContents = useHandleDeleteGroupWithContents(
    deleteGroupWithContents,
  );

  const selectedNodeData = useMemo(
    () =>
      selectedNode
        ? (workflow.nodes.find((n) => n.id === selectedNode) ?? null)
        : null,
    [selectedNode, workflow.nodes],
  );
  const appKindMap = useMemo(() => buildAppKindMap(workflow), [workflow]);
  const selectedNodeAppKind = useMemo(() => {
    if (!selectedNode) return undefined;
    const kind = appKindMap.get(selectedNode);
    return kind && kind !== "Native" ? kind : undefined;
  }, [selectedNode, appKindMap]);
  const hasAiNodes = useMemo(
    () => workflow.nodes.some((n) => n.node_type.type === "AiStep"),
    [workflow.nodes],
  );

  if (isNewWorkflow && workflow.nodes.length === 0) {
    return (
      <IntentEmptyState
        onGenerate={(intent) => {
          setAssistantOpen(true);
          skipIntentEntry();
          startAgent(intent);
        }}
        onSkip={skipIntentEntry}
        onRecordWalkthrough={() => {
          skipIntentEntry();
          useStore.getState().openCdpModal();
        }}
        loading={agentStatus === "running"}
      />
    );
  }

  return (
    <div className="relative flex flex-1 overflow-hidden">
      {isWalkthroughBusy(walkthroughStatus) && (
        <div className="absolute inset-0 z-10" />
      )}
      <div
        className={
          drawerOpen
            ? "hidden h-full shrink-0 min-[1100px]:block"
            : "h-full shrink-0"
        }
      >
        <NodePalette
          nodeTypes={nodeTypes}
          search={nodeSearch}
          collapsed={sidebarCollapsed}
          onSearchChange={setNodeSearch}
          onAdd={addNode}
          onToggle={toggleSidebar}
        />
      </div>
      {skillsAvailable && (
        <div className="hidden h-full shrink-0 min-[980px]:flex">
          <SkillsPanel />
        </div>
      )}

      <div className="relative flex-1 overflow-hidden bg-[var(--bg-dark)]">
        <GraphCanvas
          workflow={workflow}
          selectedNode={selectedNode}
          activeNode={activeNode}
          canvasSelectionResetTick={canvasSelectionResetTick}
          onSelectNode={selectNode}
          onCanvasSelectionChange={setHasCanvasSelection}
          onNodePositionsChange={updateNodePositions}
          onEdgesChange={(edges) => {
            pushHistory("Remove Edge");
            setWorkflow({ ...workflow, edges });
          }}
          onConnect={addEdge}
          onDeleteNodes={handleDeleteNodes}
          onRemoveExtraEdges={removeEdgesOnly}
          onBeforeNodeDrag={() => pushHistory("Move Nodes")}
          onCreateGroup={createGroup}
          onRemoveGroup={removeGroup}
          onDeleteGroupWithContents={handleDeleteGroupWithContents}
          onRenameGroup={renameGroup}
          onRecolorGroup={recolorGroup}
          onAddNodesToGroup={addNodesToGroup}
          onRemoveNodesFromGroup={removeNodesFromGroup}
        />

        <FloatingToolbar
          executorState={executorState}
          executionMode={executionMode}
          logsOpen={logsDrawerOpen}
          hasAiNodes={hasAiNodes}
          hasNodes={workflow.nodes.length > 0}
          walkthroughStatus={walkthroughStatus}
          walkthroughPanelOpen={walkthroughPanelOpen}
          walkthroughEventCount={walkthroughEventCount}
          lastRunStatus={lastRunStatus}
          runningHint={true}
          onToggleLogs={toggleLogsDrawer}
          onRunStop={executorState === "running" ? stopWorkflow : runWorkflow}
          onAssistant={toggleAssistant}
          onSetExecutionMode={setExecutionMode}
          onOpenWalkthroughPanel={() => setWalkthroughPanelOpen(true)}
          onRecord={() => useStore.getState().openCdpModal()}
        />
      </div>

      {skillsAvailable && selectedSkill && (
        <div className="hidden w-[420px] shrink-0 border-l border-[var(--border)] bg-[var(--bg-panel)] min-[980px]:block">
          <SkillDetailView
            skillId={selectedSkill.id}
            version={selectedSkill.version}
            projectPath={projectPath}
            workflowName={workflow.name}
            workflowId={workflow.id}
            runId={agentRunId}
            storeTraces={storeTraces}
            onChanged={() =>
              loadSkillsForPanel({
                projectPath,
                workflowName: workflow.name,
                workflowId: workflow.id,
                includeGlobal: skillsGlobalParticipation,
                storeTraces,
              }).catch((e) => console.error("Failed to reload skills panel", e))
            }
          />
        </div>
      )}

      <AssistantPanel
        open={drawerOpen}
        error={assistantError}
        messages={messages}
        onSendMessage={startAgent}
        onClose={() => setAssistantOpen(false)}
      />

      <WalkthroughPanel />

      <NodeDetailModal
        node={selectedNodeData}
        nodes={workflow.nodes}
        projectPath={projectPath}
        workflowId={workflow.id}
        workflowName={workflow.name}
        tab={detailTab}
        onTabChange={setDetailTab}
        onUpdate={updateNode}
        onClose={() => selectNode(null)}
        appKind={selectedNodeAppKind}
      />
    </div>
  );
}
