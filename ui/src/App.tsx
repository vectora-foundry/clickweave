import { useStore } from "./store/useAppStore";
import { Sidebar } from "./components/Sidebar";
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
import { useEffect, useMemo } from "react";
import { listen } from "@tauri-apps/api/event";
import { useEscapeKey } from "./hooks/useEscapeKey";
import { useUndoRedoKeyboard } from "./hooks/useUndoRedoKeyboard";
import { useWorkflowActions } from "./hooks/useWorkflowActions";

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
  const maxRepairAttempts = useStore((s) => s.maxRepairAttempts);
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
  const setExecutionMode = useStore((s) => s.setExecutionMode);
  const supervisionRespond = useStore((s) => s.supervisionRespond);
  const runWorkflow = useStore((s) => s.runWorkflow);
  const stopWorkflow = useStore((s) => s.stopWorkflow);
  const setAssistantOpen = useStore((s) => s.setAssistantOpen);
  const toggleAssistant = useStore((s) => s.toggleAssistant);
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

  useEscapeKey();
  useUndoRedoKeyboard(undo, redo);

  const hasAiNodes = useMemo(
    () => workflow.nodes.some((n) => n.node_type.type === "AiStep"),
    [workflow.nodes],
  );

  // ── Tauri event listeners (use getState() to avoid stale closures) ──
  useEffect(() => {
    const subscriptions = Promise.all([
      listen<{ message: string }>("executor://log", (e) => {
        useStore.getState().pushLog(e.payload.message);
      }),
      listen<{ state: string }>("executor://state", (e) => {
        const s = e.payload.state as "idle" | "running";
        useStore.getState().setExecutorState(s);
        if (s === "idle") useStore.getState().setActiveNode(null);
        if (s === "running") {
          useStore.getState().clearVerdicts();
          useStore.getState().setLastRunStatus(null);
        }
      }),
      listen<{ node_id: string }>("executor://node_started", (e) => {
        useStore.getState().setActiveNode(e.payload.node_id);
        useStore.getState().pushLog(`Node started: ${e.payload.node_id}`);
      }),
      listen<{ node_id: string }>("executor://node_completed", (e) => {
        useStore.getState().setActiveNode(null);
        useStore.getState().pushLog(`Node completed: ${e.payload.node_id}`);
      }),
      listen<{ node_id: string; error: string }>("executor://node_failed", (e) => {
        useStore.getState().setActiveNode(null);
        useStore.getState().pushLog(`Node failed: ${e.payload.node_id} - ${e.payload.error}`);
        useStore.getState().setLastRunStatus("failed");
      }),
      listen<import("./store/slices/verdictSlice").NodeVerdict[]>(
        "executor://checks_completed",
        (e) => {
          useStore.getState().setVerdicts(e.payload);
        },
      ),
      listen("executor://workflow_completed", () => {
        useStore.getState().pushLog("Workflow completed");
        useStore.getState().setExecutorState("idle");
        useStore.getState().setActiveNode(null);
        if (useStore.getState().lastRunStatus !== "failed") {
          useStore.getState().setLastRunStatus("completed");
        }
        useStore.getState().openVerdictModal();
      }),
      listen<{ node_id: string; node_name: string; summary: string }>(
        "executor://supervision_passed",
        (e) => {
          useStore.getState().pushLog(`Verified: ${e.payload.node_name} — ${e.payload.summary}`);
        },
      ),
      listen<{ node_id: string; node_name: string; finding: string; screenshot: string | null }>(
        "executor://supervision_paused",
        (e) => {
          useStore.getState().setSupervisionPause({
            nodeId: e.payload.node_id,
            nodeName: e.payload.node_name,
            finding: e.payload.finding,
            screenshot: e.payload.screenshot ?? null,
          });
        },
      ),
      listen("menu://new", () => useStore.getState().newProject()),
      listen("menu://open", () => useStore.getState().openProject()),
      listen("menu://save", () => useStore.getState().saveProject()),
      listen("menu://toggle-sidebar", () => useStore.getState().toggleSidebar()),
      listen("menu://toggle-logs", () => useStore.getState().toggleLogsDrawer()),
      listen("menu://run-workflow", () => useStore.getState().runWorkflow()),
      listen("menu://stop-workflow", () => useStore.getState().stopWorkflow()),
      listen("menu://toggle-assistant", () => useStore.getState().toggleAssistant()),
      listen("assistant://repairing", () => {
        useStore.setState({ assistantRetrying: true });
      }),
      listen<{ status: string }>("walkthrough://state", (e) => {
        useStore.getState().setWalkthroughStatus(
          e.payload.status as import("./store/slices/walkthroughSlice").WalkthroughStatus,
        );
      }),
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      listen<{ event: any }>("walkthrough://event", (e) => {
        useStore.getState().pushWalkthroughEvent(e.payload.event);
      }),
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      listen<{ actions: any[]; draft: any; warnings: string[]; action_node_map: any[]; used_fallback: boolean }>("walkthrough://draft_ready", (e) => {
        useStore.getState().setWalkthroughDraft({
          actions: e.payload.actions,
          draft: e.payload.draft,
          warnings: e.payload.warnings,
          action_node_map: e.payload.action_node_map ?? [],
          used_fallback: e.payload.used_fallback ?? true,
        });
      }),
    ]).catch((err) => {
      console.error("Failed to subscribe to Tauri events:", err);
      useStore.getState().pushLog(`Critical: event listeners failed to initialize: ${err}`);
      return [] as (() => void)[];
    });

    return () => {
      subscriptions.then((unlisteners) => unlisteners.forEach((u) => u()));
    };
  }, []);

  return (
    <div className="flex h-screen overflow-hidden bg-[var(--bg-dark)]">
      <Sidebar
        collapsed={sidebarCollapsed}
        onToggle={toggleSidebar}
      />

      <div className="flex flex-1 flex-col overflow-hidden">
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
                useStore.getState().startWalkthrough();
              }}
              loading={assistantLoading}
            />
          ) : (
            <>
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
                  onToggleLogs={toggleLogsDrawer}
                  onRunStop={
                    executorState === "running"
                      ? stopWorkflow
                      : runWorkflow
                  }
                  onAssistant={toggleAssistant}
                  onSetExecutionMode={setExecutionMode}
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

              <NodePalette
                nodeTypes={nodeTypes}
                search={nodeSearch}
                onSearchChange={setNodeSearch}
                onAdd={addNode}
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

      <NodeDetailModal
        node={selectedNodeData}
        projectPath={projectPath}
        workflowId={workflow.id}
        workflowName={workflow.name}
        tab={detailTab}
        onTabChange={setDetailTab}
        onUpdate={updateNode}
        onClose={() => selectNode(null)}
      />

      <SettingsModal
        open={showSettings}
        plannerConfig={plannerConfig}
        agentConfig={agentConfig}
        vlmConfig={vlmConfig}
        vlmEnabled={vlmEnabled}
        mcpCommand={mcpCommand}
        maxRepairAttempts={maxRepairAttempts}
        onClose={() => setShowSettings(false)}
        onPlannerConfigChange={setPlannerConfig}
        onAgentConfigChange={setAgentConfig}
        onVlmConfigChange={setVlmConfig}
        onVlmEnabledChange={setVlmEnabled}
        onMcpCommandChange={setMcpCommand}
        onMaxRepairAttemptsChange={setMaxRepairAttempts}
      />

      <VerdictModal />

      {supervisionPause && (
        <SupervisionModal
          pause={supervisionPause}
          onRespond={supervisionRespond}
        />
      )}
    </div>
  );
}

export default App;
