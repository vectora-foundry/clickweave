import { useStore } from "./store/useAppStore";
import { useShallow } from "zustand/react/shallow";
import { Header } from "./components/Header";
import { NodePalette } from "./components/NodePalette";
import { LogsDrawer } from "./components/LogsDrawer";
import { FloatingToolbar } from "./components/FloatingToolbar";
import { SettingsModal } from "./components/SettingsModal";
import { GraphCanvas } from "./components/GraphCanvas";
import { NodeDetailModal } from "./components/node-detail/NodeDetailModal";
import { AssistantPanel } from "./components/AssistantPanel";
import { WalkthroughPanel } from "./components/WalkthroughPanel";
import { IntentEmptyState } from "./components/IntentEmptyState";
import { VerdictBar } from "./components/VerdictBar";
import { VerdictModal } from "./components/VerdictModal";
import { SupervisionModal } from "./components/SupervisionModal";
import { SupervisorConfirmation } from "./components/SupervisorConfirmation";
import { CdpAppSelectModal } from "./components/CdpAppSelectModal";
import { useEffect, useMemo } from "react";
import { useEscapeKey } from "./hooks/useEscapeKey";
import { useUndoRedoKeyboard } from "./hooks/useUndoRedoKeyboard";
import { useWorkflowActions } from "./hooks/useWorkflowActions";
import { useExecutorEvents } from "./hooks/useExecutorEvents";
import { buildAppKindMap } from "./hooks/useNodeSync";
import { isWalkthroughBusy } from "./store/slices/walkthroughSlice";

function App() {
  // ── One-time loaders ─────────────────────────────────────────────
  useEffect(() => {
    useStore.getState().loadSettingsFromDisk();
    useStore.getState().loadNodeTypes();
  }, []);

  // ── State selectors (grouped with useShallow) ───────────────────
  const { workflow, projectPath, nodeTypes, isNewWorkflow } = useStore(
    useShallow((s) => ({
      workflow: s.workflow,
      projectPath: s.projectPath,
      nodeTypes: s.nodeTypes,
      isNewWorkflow: s.isNewWorkflow,
    })),
  );

  const { executorState, lastRunStatus, executionMode, supervisionPause, activeNode } = useStore(
    useShallow((s) => ({
      executorState: s.executorState,
      lastRunStatus: s.lastRunStatus,
      executionMode: s.executionMode,
      supervisionPause: s.supervisionPause,
      activeNode: s.activeNode,
    })),
  );

  const { selectedNode, sidebarCollapsed, logsDrawerOpen, nodeSearch, showSettings, detailTab, logs } = useStore(
    useShallow((s) => ({
      selectedNode: s.selectedNode,
      sidebarCollapsed: s.sidebarCollapsed,
      logsDrawerOpen: s.logsDrawerOpen,
      nodeSearch: s.nodeSearch,
      showSettings: s.showSettings,
      detailTab: s.detailTab,
      logs: s.logs,
    })),
  );

  const { assistantOpen, assistantLoading, assistantRetrying, assistantError, messages, contextUsage, agentStatus } = useStore(
    useShallow((s) => ({
      assistantOpen: s.assistantOpen,
      assistantLoading: s.assistantLoading,
      assistantRetrying: s.assistantRetrying,
      assistantError: s.assistantError,
      messages: s.messages,
      contextUsage: s.contextUsage,
      agentStatus: s.agentStatus,
    })),
  );

  const { supervisorConfig, agentConfig, fastConfig, fastEnabled, maxRepairAttempts, hoverDwellThreshold, supervisionDelayMs, toolPermissions } = useStore(
    useShallow((s) => ({
      supervisorConfig: s.supervisorConfig,
      agentConfig: s.agentConfig,
      fastConfig: s.fastConfig,
      fastEnabled: s.fastEnabled,
      maxRepairAttempts: s.maxRepairAttempts,
      hoverDwellThreshold: s.hoverDwellThreshold,
      supervisionDelayMs: s.supervisionDelayMs,
      toolPermissions: s.toolPermissions,
    })),
  );

  const { walkthroughStatus, walkthroughPanelOpen, cdpModalOpen, cdpProgress } = useStore(
    useShallow((s) => ({
      walkthroughStatus: s.walkthroughStatus,
      walkthroughPanelOpen: s.walkthroughPanelOpen,
      cdpModalOpen: s.walkthroughCdpModalOpen,
      cdpProgress: s.walkthroughCdpProgress,
    })),
  );

  const walkthroughEventCount = useStore((s) => s.walkthroughEvents.length);

  // ── Action selectors ─────────────────────────────────────────────
  const setWorkflow = useStore((s) => s.setWorkflow);
  const selectNode = useStore((s) => s.selectNode);
  const setDetailTab = useStore((s) => s.setDetailTab);
  const toggleSidebar = useStore((s) => s.toggleSidebar);
  const toggleLogsDrawer = useStore((s) => s.toggleLogsDrawer);
  const setNodeSearch = useStore((s) => s.setNodeSearch);
  const setShowSettings = useStore((s) => s.setShowSettings);
  const clearLogs = useStore((s) => s.clearLogs);
  const pushHistory = useStore((s) => s.pushHistory);
  const saveProject = useStore((s) => s.saveProject);
  const setSupervisorConfig = useStore((s) => s.setSupervisorConfig);
  const setAgentConfig = useStore((s) => s.setAgentConfig);
  const setFastConfig = useStore((s) => s.setFastConfig);
  const setFastEnabled = useStore((s) => s.setFastEnabled);
  const setMaxRepairAttempts = useStore((s) => s.setMaxRepairAttempts);
  const setHoverDwellThreshold = useStore((s) => s.setHoverDwellThreshold);
  const setToolPermissions = useStore((s) => s.setToolPermissions);
  const setToolPermission = useStore((s) => s.setToolPermission);
  const setExecutionMode = useStore((s) => s.setExecutionMode);
  const supervisionRespond = useStore((s) => s.supervisionRespond);
  const runWorkflow = useStore((s) => s.runWorkflow);
  const stopWorkflow = useStore((s) => s.stopWorkflow);
  const setAssistantOpen = useStore((s) => s.setAssistantOpen);
  const toggleAssistant = useStore((s) => s.toggleAssistant);
  const setWalkthroughPanelOpen = useStore((s) => s.setWalkthroughPanelOpen);
  const skipIntentEntry = useStore((s) => s.skipIntentEntry);
  const startAgent = useStore((s) => s.startAgent);
  const cancelAssistantChat = useStore((s) => s.cancelAssistantChat);
  const clearConversation = useStore((s) => s.clearConversation);
  const undo = useStore((s) => s.undo);
  const redo = useStore((s) => s.redo);
  const setSupervisionDelayMs = useStore((s) => s.setSupervisionDelayMs);

  // ── Workflow mutations ───────────────────────────────────────────
  const {
    addNode, removeNodes, removeEdgesOnly, updateNodePositions, updateNode, addEdge,
    createGroup, removeGroup, deleteGroupWithContents,
    renameGroup, recolorGroup, addNodesToGroup, removeNodesFromGroup,
  } = useWorkflowActions();

  // ── Derived state ────────────────────────────────────────────────
  const selectedNodeData = useMemo(
    () =>
      selectedNode
        ? workflow.nodes.find((n) => n.id === selectedNode) ?? null
        : null,
    [selectedNode, workflow.nodes],
  );

  const appKindMap = useMemo(() => buildAppKindMap(workflow), [workflow]);

  const selectedNodeAppKind = useMemo(() => {
    if (!selectedNode) return undefined;
    const kind = appKindMap.get(selectedNode);
    return kind && kind !== "Native" ? kind : undefined;
  }, [selectedNode, appKindMap]);

  useEscapeKey();
  useUndoRedoKeyboard(undo, redo);

  const hasAiNodes = useMemo(
    () => workflow.nodes.some((n) => n.node_type.type === "AiStep"),
    [workflow.nodes],
  );

  // ── Tauri event listeners (use getState() to avoid stale closures) ──
  useExecutorEvents();

  return (
    <div className="flex h-screen flex-col overflow-hidden bg-[var(--bg-dark)]">
      <Header
        workflowName={workflow.name}
        executorState={executorState}
        lastRunStatus={lastRunStatus}
        onSave={saveProject}
        onSettings={() => setShowSettings(true)}
        onNameChange={(name) => {
          pushHistory("Rename Workflow");
          setWorkflow({ ...workflow, name });
        }}
      />

      <div className="flex flex-1 flex-col overflow-hidden">
        <VerdictBar />

        <div className="relative flex flex-1 overflow-hidden">
          {isWalkthroughBusy(walkthroughStatus) && (
            <div className="absolute inset-0 z-10" />
          )}
          {isNewWorkflow && workflow.nodes.length === 0 ? (
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
          ) : (
            <>
              <NodePalette
                nodeTypes={nodeTypes}
                search={nodeSearch}
                collapsed={sidebarCollapsed}
                onSearchChange={setNodeSearch}
                onAdd={addNode}
                onToggle={toggleSidebar}
              />

              <div className="relative flex-1 overflow-hidden bg-[var(--bg-dark)]">
                <GraphCanvas
                  workflow={workflow}
                  selectedNode={selectedNode}
                  activeNode={activeNode}
                  onSelectNode={selectNode}
                  onNodePositionsChange={updateNodePositions}
                  onEdgesChange={(edges) => {
                    pushHistory("Remove Edge");
                    setWorkflow({ ...workflow, edges });
                  }}
                  onConnect={addEdge}
                  onDeleteNodes={removeNodes}
                  onRemoveExtraEdges={removeEdgesOnly}
                  onBeforeNodeDrag={() => pushHistory("Move Nodes")}
                  onCreateGroup={createGroup}
                  onRemoveGroup={removeGroup}
                  onDeleteGroupWithContents={deleteGroupWithContents}
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
                  onToggleLogs={toggleLogsDrawer}
                  onRunStop={
                    executorState === "running"
                      ? stopWorkflow
                      : runWorkflow
                  }
                  onAssistant={toggleAssistant}
                  onSetExecutionMode={setExecutionMode}
                  walkthroughEventCount={walkthroughEventCount}
                  onOpenWalkthroughPanel={() => setWalkthroughPanelOpen(true)}
                  onRecord={() => useStore.getState().openCdpModal()}
                />
              </div>

              <AssistantPanel
                open={assistantOpen}
                loading={assistantLoading}
                retrying={assistantRetrying}
                error={assistantError}
                messages={messages}
                contextUsage={contextUsage}
                onSendMessage={startAgent}
                onCancel={cancelAssistantChat}
                onClearConversation={clearConversation}
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
            </>
          )}
        </div>

        <LogsDrawer
          open={logsDrawerOpen}
          logs={logs}
          onToggle={toggleLogsDrawer}
          onClear={clearLogs}
        />
      </div>

      <SettingsModal
        open={showSettings}
        supervisorConfig={supervisorConfig}
        agentConfig={agentConfig}
        fastConfig={fastConfig}
        fastEnabled={fastEnabled}
        maxRepairAttempts={maxRepairAttempts}
        hoverDwellThreshold={hoverDwellThreshold}
        supervisionDelayMs={supervisionDelayMs}
        toolPermissions={toolPermissions}
        onClose={() => setShowSettings(false)}
        onSupervisorConfigChange={setSupervisorConfig}
        onAgentConfigChange={setAgentConfig}
        onFastConfigChange={setFastConfig}
        onFastEnabledChange={setFastEnabled}
        onMaxRepairAttemptsChange={setMaxRepairAttempts}
        onHoverDwellThresholdChange={setHoverDwellThreshold}
        onSupervisionDelayMsChange={setSupervisionDelayMs}
        onToolPermissionsChange={setToolPermissions}
        onToolPermissionChange={setToolPermission}
      />

      <VerdictModal />

      {supervisionPause && (
        <SupervisionModal
          pause={supervisionPause}
          onRespond={supervisionRespond}
        />
      )}

      <SupervisorConfirmation />

      <CdpAppSelectModal
        open={cdpModalOpen}
        cdpProgress={cdpProgress}
        onStart={(cdpApps) => useStore.getState().startWalkthrough(cdpApps)}
        onSkip={() => { useStore.getState().closeCdpModal(); useStore.getState().startWalkthrough([]); }}
        onCancel={() => useStore.getState().closeCdpModal()}
      />

    </div>
  );
}

export default App;
