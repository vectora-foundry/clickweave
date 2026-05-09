import { useStore } from "../../store/useAppStore";
import { useShallow } from "zustand/react/shallow";
import { useMemo } from "react";
import { AssistantPanel } from "../AssistantPanel";
import { FloatingToolbar } from "../FloatingToolbar";
import { IntentEmptyState } from "../IntentEmptyState";
import { SkillDetailView } from "../skills/SkillDetailView";
import { TraceCanvas } from "../TraceCanvas";
import { WalkthroughPanel } from "../WalkthroughPanel";
import { isWalkthroughBusy } from "../../store/slices/walkthroughSlice";

export function CanvasView() {
  const {
    workflow,
    projectPath,
    isNewWorkflow,
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
      isNewWorkflow: s.isNewWorkflow,
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

  const toggleLogsDrawer = useStore((s) => s.toggleLogsDrawer);
  const setExecutionMode = useStore((s) => s.setExecutionMode);
  const runWorkflow = useStore((s) => s.runWorkflow);
  const stopWorkflow = useStore((s) => s.stopWorkflow);
  const setAssistantOpen = useStore((s) => s.setAssistantOpen);
  const toggleAssistant = useStore((s) => s.toggleAssistant);
  const setWalkthroughPanelOpen = useStore((s) => s.setWalkthroughPanelOpen);
  const skipIntentEntry = useStore((s) => s.skipIntentEntry);
  const startAgent = useStore((s) => s.startAgent);
  const loadSkillsForPanel = useStore((s) => s.loadSkillsForPanel);

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
      <div className="relative flex-1 overflow-hidden bg-[var(--bg-dark)]">
        <TraceCanvas />

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
            projectName={workflow.name}
            projectId={workflow.id}
            runId={agentRunId}
            storeTraces={storeTraces}
            onChanged={() =>
              loadSkillsForPanel({
                projectPath,
                projectName: workflow.name,
                projectId: workflow.id,
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
    </div>
  );
}
