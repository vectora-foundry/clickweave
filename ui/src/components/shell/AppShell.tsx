import { useEffect } from "react";
import { useShallow } from "zustand/react/shallow";
import { useStore } from "../../store/useAppStore";
import { AmbiguityResolutionModal } from "../AmbiguityResolutionModal";
import { CdpAppSelectModal } from "../CdpAppSelectModal";
import { ConfirmClearConversationModal } from "../ConfirmClearConversationModal";
import { LogsDrawer } from "../LogsDrawer";
import { SettingsModal } from "../SettingsModal";
import { SupervisionModal } from "../SupervisionModal";
import { VerdictBar } from "../VerdictBar";
import { VerdictModal } from "../VerdictModal";
import { CanvasView } from "./CanvasView";
import { OverviewView } from "./OverviewView";
import { Sidebar } from "./Sidebar";
import { TitleBar } from "./TitleBar";
import { useEscapeKey } from "../../hooks/useEscapeKey";
import { useUndoRedoKeyboard } from "../../hooks/useUndoRedoKeyboard";
import { useExecutorEvents } from "../../hooks/useExecutorEvents";

export function AppShell() {
  const currentView = useStore((s) => s.currentView);
  const showSettings = useStore((s) => s.showSettings);
  const setShowSettings = useStore((s) => s.setShowSettings);
  const supervisionPause = useStore((s) => s.supervisionPause);
  const supervisionRespond = useStore((s) => s.supervisionRespond);

  const {
    cdpModalOpen,
    cdpProgress,
    logs,
    logsDrawerOpen,
    activeAmbiguityId,
    ambiguityResolutions,
    confirmClearOpen,
  } = useStore(
    useShallow((s) => ({
      cdpModalOpen: s.walkthroughCdpModalOpen,
      cdpProgress: s.walkthroughCdpProgress,
      logs: s.logs,
      logsDrawerOpen: s.logsDrawerOpen,
      activeAmbiguityId: s.activeAmbiguityId,
      ambiguityResolutions: s.ambiguityResolutions,
      confirmClearOpen: s.confirmClearOpen,
    })),
  );

  const toggleLogsDrawer = useStore((s) => s.toggleLogsDrawer);
  const clearLogs = useStore((s) => s.clearLogs);
  const closeAmbiguityModal = useStore((s) => s.closeAmbiguityModal);
  const setConfirmClearOpen = useStore((s) => s.setConfirmClearOpen);
  const clearConversationFlow = useStore((s) => s.clearConversationFlow);
  const agentNodeCount = useStore(
    (s) => s.workflow.nodes.filter((n) => n.source_run_id != null).length,
  );

  const activeAmbiguity =
    ambiguityResolutions.find((r) => r.id === activeAmbiguityId) ?? null;

  // Settings + skills panel data loading (lifted from App.tsx)
  const projectPath = useStore((s) => s.projectPath);
  const workflowId = useStore((s) => s.workflow.id);
  const workflowName = useStore((s) => s.workflow.name);
  const storeTraces = useStore((s) => s.storeTraces);
  const skillsEnabled = useStore((s) => s.skillsEnabled);
  const skillsGlobalParticipation = useStore(
    (s) => s.skillsGlobalParticipation,
  );
  const loadSkillsForPanel = useStore((s) => s.loadSkillsForPanel);
  const setSkillsList = useStore((s) => s.setSkillsList);
  const clearSelectedSkill = useStore((s) => s.clearSelectedSkill);
  const undo = useStore((s) => s.undo);
  const redo = useStore((s) => s.redo);

  const skillsAvailable = skillsEnabled && storeTraces;

  // One-time loaders
  useEffect(() => {
    useStore.getState().loadSettingsFromDisk();
    useStore.getState().loadNodeTypes();
  }, []);

  useEffect(() => {
    if (!skillsAvailable) {
      setSkillsList([]);
      clearSelectedSkill();
      return;
    }
    loadSkillsForPanel({
      projectPath,
      workflowName,
      workflowId,
      includeGlobal: skillsGlobalParticipation,
      storeTraces,
    }).catch((e) => console.error("Failed to load skills panel", e));
  }, [
    clearSelectedSkill,
    loadSkillsForPanel,
    projectPath,
    setSkillsList,
    skillsAvailable,
    skillsGlobalParticipation,
    storeTraces,
    workflowId,
    workflowName,
  ]);

  useEscapeKey();
  useUndoRedoKeyboard(undo, redo);
  useExecutorEvents();

  // SettingsModal needs the same wiring App.tsx uses; lift the
  // selectors here.
  const settingsProps = useSettingsModalProps();

  return (
    <div className="cw-shell-root flex h-screen flex-col overflow-hidden bg-[var(--bg-dark)]">
      <TitleBar />
      <VerdictBar />
      <div className="flex flex-1 overflow-hidden">
        <Sidebar />
        <main className="flex flex-1 flex-col overflow-hidden">
          {currentView === "overview" ? <OverviewView /> : <CanvasView />}
        </main>
      </div>
      {/* Phase 1–4: legacy LogsDrawer; Phase 5 swaps for <LogsBar /> (P1.H3 — keep
          the logs surface mounted continuously so menu://toggle-logs and the
          FloatingToolbar logs button never point at a missing element). */}
      <LogsDrawer
        open={logsDrawerOpen}
        logs={logs}
        onToggle={toggleLogsDrawer}
        onClear={clearLogs}
      />

      <SettingsModal
        open={showSettings}
        {...settingsProps}
        onClose={() => setShowSettings(false)}
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
        cdpProgress={cdpProgress}
        onStart={(cdpApps) => useStore.getState().startWalkthrough(cdpApps)}
        onSkip={() => {
          useStore.getState().closeCdpModal();
          useStore.getState().startWalkthrough([]);
        }}
        onCancel={() => useStore.getState().closeCdpModal()}
      />
      {/* D15 root-mounted modals (P1.H1 — hoisted out of AssistantThread). */}
      {activeAmbiguity && (
        <AmbiguityResolutionModal
          resolution={activeAmbiguity}
          onClose={closeAmbiguityModal}
        />
      )}
      <ConfirmClearConversationModal
        open={confirmClearOpen}
        agentNodeCount={agentNodeCount}
        onConfirm={async () => {
          setConfirmClearOpen(false);
          await clearConversationFlow();
        }}
        onCancel={() => setConfirmClearOpen(false)}
      />
    </div>
  );
}

function useSettingsModalProps() {
  // Mirror today's App.tsx 96-194 selector block + the action selectors
  // SettingsModal needs. Returns the prop set passed into <SettingsModal/>.
  return useStore(
    useShallow((s) => ({
      supervisorConfig: s.supervisorConfig,
      agentConfig: s.agentConfig,
      fastConfig: s.fastConfig,
      fastEnabled: s.fastEnabled,
      maxRepairAttempts: s.maxRepairAttempts,
      hoverDwellThreshold: s.hoverDwellThreshold,
      supervisionDelayMs: s.supervisionDelayMs,
      toolPermissions: s.toolPermissions,
      traceRetentionDays: s.traceRetentionDays,
      storeTraces: s.storeTraces,
      episodicEnabled: s.episodicEnabled,
      retrievedEpisodesK: s.retrievedEpisodesK,
      episodicGlobalParticipation: s.episodicGlobalParticipation,
      skillsEnabled: s.skillsEnabled,
      applicableSkillsK: s.applicableSkillsK,
      skillsGlobalParticipation: s.skillsGlobalParticipation,
      onSupervisorConfigChange: s.setSupervisorConfig,
      onAgentConfigChange: s.setAgentConfig,
      onFastConfigChange: s.setFastConfig,
      onFastEnabledChange: s.setFastEnabled,
      onMaxRepairAttemptsChange: s.setMaxRepairAttempts,
      onHoverDwellThresholdChange: s.setHoverDwellThreshold,
      onSupervisionDelayMsChange: s.setSupervisionDelayMs,
      onToolPermissionsChange: s.setToolPermissions,
      onToolPermissionChange: s.setToolPermission,
      onTraceRetentionDaysChange: s.setTraceRetentionDays,
      onStoreTracesChange: s.setStoreTraces,
      onEpisodicEnabledChange: s.setEpisodicEnabled,
      onRetrievedEpisodesKChange: s.setRetrievedEpisodesK,
      onEpisodicGlobalParticipationChange: s.setEpisodicGlobalParticipation,
      onSkillsEnabledChange: s.setSkillsEnabled,
      onApplicableSkillsKChange: s.setApplicableSkillsK,
      onSkillsGlobalParticipationChange: s.setSkillsGlobalParticipation,
    })),
  );
}
