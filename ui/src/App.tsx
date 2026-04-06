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
import { PatchReviewDialog } from "./components/PatchReviewDialog";
import { PlannerConfirmation } from "./components/PlannerConfirmation";
import { CdpAppSelectModal } from "./components/CdpAppSelectModal";
import { AutoApproveBanner } from "./components/AutoApproveBanner";
import { useEffect, useMemo } from "react";
import { useEscapeKey } from "./hooks/useEscapeKey";
import { useUndoRedoKeyboard } from "./hooks/useUndoRedoKeyboard";
import { useWorkflowActions } from "./hooks/useWorkflowActions";
import { useExecutorEvents } from "./hooks/useExecutorEvents";
import { usePlannerEvents } from "./hooks/usePlannerEvents";
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

  const { executorState, lastRunStatus, executionMode, supervisionPause, resolutionProposal, activeNode, autoApprovedCount } = useStore(
    useShallow((s) => ({
      executorState: s.executorState,
      lastRunStatus: s.lastRunStatus,
      executionMode: s.executionMode,
      supervisionPause: s.supervisionPause,
      resolutionProposal: s.resolutionProposal,
      activeNode: s.activeNode,
      autoApprovedCount: s.autoApprovedCount,
    })),
  );

  const autoApproveResolutions = useStore((s) => s.workflow.auto_approve_resolutions ?? false);

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

  const { assistantOpen, assistantLoading, assistantRetrying, assistantError, messages, pendingPatch, pendingPatchWarnings, contextUsage } = useStore(
    useShallow((s) => ({
      assistantOpen: s.assistantOpen,
      assistantLoading: s.assistantLoading,
      assistantRetrying: s.assistantRetrying,
      assistantError: s.assistantError,
      messages: s.messages,
      pendingPatch: s.pendingPatch,
      pendingPatchWarnings: s.pendingPatchWarnings,
      contextUsage: s.contextUsage,
    })),
  );

  const { plannerConfig, agentConfig, fastConfig, fastEnabled, maxRepairAttempts, hoverDwellThreshold, toolPermissions } = useStore(
    useShallow((s) => ({
      plannerConfig: s.plannerConfig,
      agentConfig: s.agentConfig,
      fastConfig: s.fastConfig,
      fastEnabled: s.fastEnabled,
      maxRepairAttempts: s.maxRepairAttempts,
      hoverDwellThreshold: s.hoverDwellThreshold,
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
  const setPlannerConfig = useStore((s) => s.setPlannerConfig);
  const setAgentConfig = useStore((s) => s.setAgentConfig);
  const setFastConfig = useStore((s) => s.setFastConfig);
  const setFastEnabled = useStore((s) => s.setFastEnabled);
  const setMaxRepairAttempts = useStore((s) => s.setMaxRepairAttempts);
  const setHoverDwellThreshold = useStore((s) => s.setHoverDwellThreshold);
  const setToolPermissions = useStore((s) => s.setToolPermissions);
  const setToolPermission = useStore((s) => s.setToolPermission);
  const setExecutionMode = useStore((s) => s.setExecutionMode);
  const supervisionRespond = useStore((s) => s.supervisionRespond);
  const resolveResolution = useStore((s) => s.resolveResolution);
  const runWorkflow = useStore((s) => s.runWorkflow);
  const stopWorkflow = useStore((s) => s.stopWorkflow);
  const setAssistantOpen = useStore((s) => s.setAssistantOpen);
  const toggleAssistant = useStore((s) => s.toggleAssistant);
  const setWalkthroughPanelOpen = useStore((s) => s.setWalkthroughPanelOpen);
  const skipIntentEntry = useStore((s) => s.skipIntentEntry);
  const sendAssistantMessage = useStore((s) => s.sendAssistantMessage);
  const cancelAssistantChat = useStore((s) => s.cancelAssistantChat);
  const applyApprovedPatch = useStore((s) => s.applyApprovedPatch);
  const discardPendingPatch = useStore((s) => s.discardPendingPatch);
  const clearConversation = useStore((s) => s.clearConversation);
  const undo = useStore((s) => s.undo);
  const redo = useStore((s) => s.redo);
  const setAutoApproveResolutions = useStore((s) => s.setAutoApproveResolutions);
  const dismissAutoApproveBanner = useStore((s) => s.dismissAutoApproveBanner);

  // ── Workflow mutations ───────────────────────────────────────────
  const {
    addNode, removeNodes, removeEdgesOnly, updateNodePositions, updateNode, addEdge, dataConnect,
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
  usePlannerEvents();

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
        {executorState === "idle" && lastRunStatus === "completed" && (
          <AutoApproveBanner
            count={autoApprovedCount}
            onDismiss={dismissAutoApproveBanner}
            onViewLogs={() => {
              dismissAutoApproveBanner();
              if (!logsDrawerOpen) toggleLogsDrawer();
            }}
          />
        )}

        <div className="relative flex flex-1 overflow-hidden">
          {isWalkthroughBusy(walkthroughStatus) && (
            <div className="absolute inset-0 z-10" />
          )}
          {isNewWorkflow && workflow.nodes.length === 0 ? (
            <IntentEmptyState
              onGenerate={(intent) => {
                setAssistantOpen(true);
                skipIntentEntry();
                sendAssistantMessage(intent);
              }}
              onSkip={skipIntentEntry}
              onRecordWalkthrough={() => {
                skipIntentEntry();
                useStore.getState().openCdpModal();
              }}
              loading={assistantLoading}
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
                  onDataConnect={dataConnect}
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
                  autoApproveResolutions={autoApproveResolutions}
                  onToggleAutoApprove={setAutoApproveResolutions}
                />
              </div>

              <AssistantPanel
                open={assistantOpen}
                loading={assistantLoading}
                retrying={assistantRetrying}
                error={assistantError}
                messages={messages}
                pendingPatch={pendingPatch}
                pendingPatchWarnings={pendingPatchWarnings}
                contextUsage={contextUsage}
                onSendMessage={sendAssistantMessage}
                onCancel={cancelAssistantChat}
                onApplyPatch={applyApprovedPatch}
                onDiscardPatch={discardPendingPatch}
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
        plannerConfig={plannerConfig}
        agentConfig={agentConfig}
        fastConfig={fastConfig}
        fastEnabled={fastEnabled}
        maxRepairAttempts={maxRepairAttempts}
        hoverDwellThreshold={hoverDwellThreshold}
        toolPermissions={toolPermissions}
        onClose={() => setShowSettings(false)}
        onPlannerConfigChange={setPlannerConfig}
        onAgentConfigChange={setAgentConfig}
        onFastConfigChange={setFastConfig}
        onFastEnabledChange={setFastEnabled}
        onMaxRepairAttemptsChange={setMaxRepairAttempts}
        onHoverDwellThresholdChange={setHoverDwellThreshold}
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

      {resolutionProposal && (
        <PatchReviewDialog
          patch={resolutionProposal.patch}
          reason={resolutionProposal.reason}
          screenshot={resolutionProposal.screenshot}
          onApprove={() => resolveResolution(true)}
          onReject={() => resolveResolution(false)}
        />
      )}

      {pendingPatch && !resolutionProposal && (
        <PatchReviewDialog
          patch={pendingPatch}
          reason="Assistant proposed workflow changes"
          onApprove={() => applyApprovedPatch()}
          onReject={() => discardPendingPatch()}
        />
      )}

      <PlannerConfirmation />

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
