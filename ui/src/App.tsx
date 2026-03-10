import { useStore } from "./store/useAppStore";
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
import { CdpAppSelectModal } from "./components/CdpAppSelectModal";
import { useEffect, useMemo } from "react";
import { useEscapeKey } from "./hooks/useEscapeKey";
import { useUndoRedoKeyboard } from "./hooks/useUndoRedoKeyboard";
import { useWorkflowActions } from "./hooks/useWorkflowActions";
import { useExecutorEvents } from "./hooks/useExecutorEvents";
import { buildAppKindMap } from "./hooks/useNodeSync";

function App() {
  // ── One-time loaders ─────────────────────────────────────────────
  useEffect(() => {
    useStore.getState().loadSettingsFromDisk();
    useStore.getState().loadNodeTypes();
  }, []);

  // ── State selectors ──────────────────────────────────────────────
  const workflow = useStore((s) => s.workflow);
  const projectPath = useStore((s) => s.projectPath);
  const nodeTypes = useStore((s) => s.nodeTypes);
  const selectedNode = useStore((s) => s.selectedNode);
  const activeNode = useStore((s) => s.activeNode);
  const executorState = useStore((s) => s.executorState);
  const lastRunStatus = useStore((s) => s.lastRunStatus);
  const executionMode = useStore((s) => s.executionMode);
  const supervisionPause = useStore((s) => s.supervisionPause);
  const sidebarCollapsed = useStore((s) => s.sidebarCollapsed);
  const logsDrawerOpen = useStore((s) => s.logsDrawerOpen);
  const nodeSearch = useStore((s) => s.nodeSearch);
  const showSettings = useStore((s) => s.showSettings);
  const isNewWorkflow = useStore((s) => s.isNewWorkflow);
  const walkthroughStatus = useStore((s) => s.walkthroughStatus);
  const walkthroughPanelOpen = useStore((s) => s.walkthroughPanelOpen);
  const assistantOpen = useStore((s) => s.assistantOpen);
  const assistantLoading = useStore((s) => s.assistantLoading);
  const assistantRetrying = useStore((s) => s.assistantRetrying);
  const assistantError = useStore((s) => s.assistantError);
  const conversation = useStore((s) => s.conversation);
  const pendingPatch = useStore((s) => s.pendingPatch);
  const pendingPatchWarnings = useStore((s) => s.pendingPatchWarnings);
  const logs = useStore((s) => s.logs);
  const plannerConfig = useStore((s) => s.plannerConfig);
  const agentConfig = useStore((s) => s.agentConfig);
  const vlmConfig = useStore((s) => s.vlmConfig);
  const vlmEnabled = useStore((s) => s.vlmEnabled);
  const mcpCommand = useStore((s) => s.mcpCommand);
  const cdpModalOpen = useStore((s) => s.walkthroughCdpModalOpen);
  const cdpProgress = useStore((s) => s.walkthroughCdpProgress);
  const maxRepairAttempts = useStore((s) => s.maxRepairAttempts);
  const hoverDwellThreshold = useStore((s) => s.hoverDwellThreshold);
  const detailTab = useStore((s) => s.detailTab);

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
  const setVlmConfig = useStore((s) => s.setVlmConfig);
  const setVlmEnabled = useStore((s) => s.setVlmEnabled);
  const setMcpCommand = useStore((s) => s.setMcpCommand);
  const setMaxRepairAttempts = useStore((s) => s.setMaxRepairAttempts);
  const setHoverDwellThreshold = useStore((s) => s.setHoverDwellThreshold);
  const setExecutionMode = useStore((s) => s.setExecutionMode);
  const supervisionRespond = useStore((s) => s.supervisionRespond);
  const runWorkflow = useStore((s) => s.runWorkflow);
  const stopWorkflow = useStore((s) => s.stopWorkflow);
  const setAssistantOpen = useStore((s) => s.setAssistantOpen);
  const toggleAssistant = useStore((s) => s.toggleAssistant);
  const setWalkthroughPanelOpen = useStore((s) => s.setWalkthroughPanelOpen);
  const skipIntentEntry = useStore((s) => s.skipIntentEntry);
  const sendAssistantMessage = useStore((s) => s.sendAssistantMessage);
  const resendMessage = useStore((s) => s.resendMessage);
  const cancelAssistantChat = useStore((s) => s.cancelAssistantChat);
  const applyPendingPatch = useStore((s) => s.applyPendingPatch);
  const discardPendingPatch = useStore((s) => s.discardPendingPatch);
  const clearConversation = useStore((s) => s.clearConversation);
  const undo = useStore((s) => s.undo);
  const redo = useStore((s) => s.redo);

  // ── Workflow mutations ───────────────────────────────────────────
  const { addNode, removeNodes, removeEdgesOnly, updateNodePositions, updateNode, addEdge } =
    useWorkflowActions();

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

        <div className="flex flex-1 overflow-hidden">
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
                  onDeleteNodes={removeNodes}
                  onRemoveExtraEdges={removeEdgesOnly}
                  onBeforeNodeDrag={() => pushHistory("Move Nodes")}
                />

                <FloatingToolbar
                  executorState={executorState}
                  executionMode={executionMode}
                  logsOpen={logsDrawerOpen}
                  hasAiNodes={hasAiNodes}
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
                  onOpenWalkthroughPanel={() => setWalkthroughPanelOpen(true)}
                />
              </div>

              <AssistantPanel
                open={assistantOpen}
                loading={assistantLoading}
                retrying={assistantRetrying}
                error={assistantError}
                conversation={conversation}
                pendingPatch={pendingPatch}
                pendingPatchWarnings={pendingPatchWarnings}
                onSendMessage={sendAssistantMessage}
                onResendMessage={resendMessage}
                onCancel={cancelAssistantChat}
                onApplyPatch={applyPendingPatch}
                onDiscardPatch={discardPendingPatch}
                onClearConversation={clearConversation}
                onClose={() => setAssistantOpen(false)}
              />

              <WalkthroughPanel />

              <NodeDetailModal
                node={selectedNodeData}
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
        vlmConfig={vlmConfig}
        vlmEnabled={vlmEnabled}
        mcpCommand={mcpCommand}
        maxRepairAttempts={maxRepairAttempts}
        hoverDwellThreshold={hoverDwellThreshold}
        onClose={() => setShowSettings(false)}
        onPlannerConfigChange={setPlannerConfig}
        onAgentConfigChange={setAgentConfig}
        onVlmConfigChange={setVlmConfig}
        onVlmEnabledChange={setVlmEnabled}
        onMcpCommandChange={setMcpCommand}
        onMaxRepairAttemptsChange={setMaxRepairAttempts}
        onHoverDwellThresholdChange={setHoverDwellThreshold}
      />

      <VerdictModal />

      {supervisionPause && (
        <SupervisionModal
          pause={supervisionPause}
          onRespond={supervisionRespond}
        />
      )}

      <CdpAppSelectModal
        open={cdpModalOpen}
        mcpCommand={mcpCommand}
        cdpProgress={cdpProgress}
        onStart={(cdpApps) => useStore.getState().startWalkthrough(cdpApps)}
        onSkip={() => { useStore.getState().closeCdpModal(); useStore.getState().startWalkthrough([]); }}
        onCancel={() => useStore.getState().closeCdpModal()}
      />

    </div>
  );
}

export default App;
