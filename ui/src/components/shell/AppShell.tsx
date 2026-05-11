import { useEffect } from "react";
import { useShallow } from "zustand/react/shallow";
import { useStore } from "../../store/useAppStore";
import { AgentRunSaveSheet } from "../AgentRunSaveSheet";
import { AmbiguityResolutionModal } from "../AmbiguityResolutionModal";
import { CdpAppSelectModal } from "../CdpAppSelectModal";
import { ConfirmClearConversationModal } from "../ConfirmClearConversationModal";
import { LogsBar } from "./LogsBar";
import { SettingsModal } from "../SettingsModal";
import { VerdictBar } from "../VerdictBar";
import { VerdictModal } from "../VerdictModal";
import { OverviewView } from "./OverviewView";
import { Sidebar } from "./Sidebar";
import { TitleBar } from "./TitleBar";
import { SkillView } from "../skill/SkillView";
import { WalkthroughSaveSheet } from "../WalkthroughSaveSheet";
import { useEscapeKey } from "../../hooks/useEscapeKey";
import { useExecutorEvents } from "../../hooks/useExecutorEvents";
import { useSafetyEventRouter } from "../../hooks/useSafetyEventRouter";

export function AppShell() {
  const currentSurface = useStore((s) => s.currentSurface);
  const showSettings = useStore((s) => s.showSettings);
  const setShowSettings = useStore((s) => s.setShowSettings);
  const {
    cdpModalOpen,
    cdpProgress,
    activeAmbiguityId,
    ambiguityResolutions,
    confirmClearOpen,
    walkthroughSaveSheetOpen,
    walkthroughSessionId,
  } = useStore(
    useShallow((s) => ({
      cdpModalOpen: s.walkthroughCdpModalOpen,
      cdpProgress: s.walkthroughCdpProgress,
      activeAmbiguityId: s.activeAmbiguityId,
      ambiguityResolutions: s.ambiguityResolutions,
      confirmClearOpen: s.confirmClearOpen,
      walkthroughSaveSheetOpen: s.walkthroughSaveSheetOpen,
      walkthroughSessionId: s.walkthroughSessionId,
    })),
  );
  const closeAmbiguityModal = useStore((s) => s.closeAmbiguityModal);
  const setConfirmClearOpen = useStore((s) => s.setConfirmClearOpen);
  const clearConversationFlow = useStore((s) => s.clearConversationFlow);
  const onIntentEmptyState = useStore((s) => s.isNewWorkflow);
  const selectedSkill = useStore((s) => s.selectedSkill);
  const pendingRunSave = useStore((s) => s.pendingRunSave);
  const setPendingRunSave = useStore((s) => s.setPendingRunSave);

  const activeAmbiguity =
    ambiguityResolutions.find((r) => r.id === activeAmbiguityId) ?? null;

  // Settings + skills panel data loading (lifted from App.tsx)
  const projectPath = useStore((s) => s.projectPath);
  const projectId = useStore((s) => s.projectId);
  const projectName = useStore((s) => s.projectName);
  const storeTraces = useStore((s) => s.storeTraces);
  const skillsEnabled = useStore((s) => s.skillsEnabled);
  const skillsGlobalParticipation = useStore(
    (s) => s.skillsGlobalParticipation,
  );
  const loadSkillsForPanel = useStore((s) => s.loadSkillsForPanel);
  const setSkillsList = useStore((s) => s.setSkillsList);
  const clearSelectedSkill = useStore((s) => s.clearSelectedSkill);
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
      projectName,
      projectId,
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
    projectId,
    projectName,
  ]);

  useEscapeKey();
  useExecutorEvents();
  // 1.F.5: Mount the safety event router at AppShell root so it's always
  // active regardless of which surface is shown.
  useSafetyEventRouter();

  // SettingsModal needs the same wiring App.tsx uses; lift the
  // selectors here.
  const settingsProps = useSettingsModalProps();

  return (
    <div className="cw-shell-root flex h-screen flex-col overflow-hidden bg-[var(--bg-dark)]">
      <TitleBar />
      <VerdictBar />
      <div className="flex flex-1 overflow-hidden">
        {!onIntentEmptyState && <Sidebar />}
        <main className="flex flex-1 flex-col overflow-hidden">
          {currentSurface === "skill" && selectedSkill && !onIntentEmptyState ? (
            <SkillView />
          ) : (
            <OverviewView />
          )}
        </main>
      </div>
      {!onIntentEmptyState && <LogsBar />}

      <SettingsModal
        open={showSettings}
        {...settingsProps}
        onClose={() => setShowSettings(false)}
      />
      <VerdictModal />
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
      {/* D15 — root-mounted modals (hoisted out of AssistantThread). */}
      {activeAmbiguity && (
        <AmbiguityResolutionModal
          resolution={activeAmbiguity}
          onClose={closeAmbiguityModal}
        />
      )}
      <ConfirmClearConversationModal
        open={confirmClearOpen}
        agentNodeCount={0}
        onConfirm={async () => {
          setConfirmClearOpen(false);
          await clearConversationFlow();
        }}
        onCancel={() => setConfirmClearOpen(false)}
      />
      {pendingRunSave && (
        <AgentRunSaveSheet
          defaultName={pendingRunSave.summary}
          onSaved={() => setPendingRunSave(null)}
          onDiscard={() => setPendingRunSave(null)}
        />
      )}
      {walkthroughSaveSheetOpen && walkthroughSessionId && (
        <WalkthroughSaveSheet
          sessionId={walkthroughSessionId}
          onSaved={() => {
            useStore.getState().setWalkthroughSaveSheetOpen(false);
            useStore.getState().discardDraft();
          }}
          onDiscard={() => {
            useStore.getState().setWalkthroughSaveSheetOpen(false);
            useStore.getState().discardDraft();
          }}
        />
      )}
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
