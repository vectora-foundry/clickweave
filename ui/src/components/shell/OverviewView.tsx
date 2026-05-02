import { useState } from "react";
import { useShallow } from "zustand/react/shallow";
import { useStore } from "../../store/useAppStore";
import { CanvasPreviewCard } from "./CanvasPreviewCard";
import { IntentEmptyState } from "../IntentEmptyState";
import { LiveRuntimeCard } from "./LiveRuntimeCard";
import { OverviewAssistantCard } from "./OverviewAssistantCard";
import { StatsStrip } from "./StatsStrip";
import { WorkflowRow } from "./WorkflowRow";
import { SkillsPanel } from "../skills/SkillsPanel";

/**
 * Overview view body composition. The Overview is the new primary
 * cockpit: assistant thread on the left (7 columns), live runtime +
 * canvas preview on the right (5 columns).
 *
 * Branches on `IntentEmptyState` per D22 when the workflow is fresh
 * and empty. Phase 4 inserts `<StatsStrip />` between `WorkflowRow`
 * and the body grid.
 */
export function OverviewView() {
  const [skillsDrawerOpen, setSkillsDrawerOpen] = useState(false);

  const { workflow, isNewWorkflow, agentStatus } = useStore(
    useShallow((s) => ({
      workflow: s.workflow,
      isNewWorkflow: s.isNewWorkflow,
      agentStatus: s.agentStatus,
    })),
  );
  const setAssistantSurface = useStore((s) => s.setAssistantSurface);
  const setCurrentView = useStore((s) => s.setCurrentView);
  const skipIntentEntry = useStore((s) => s.skipIntentEntry);
  const startAgent = useStore((s) => s.startAgent);

  if (isNewWorkflow && workflow.nodes.length === 0) {
    return (
      <IntentEmptyState
        onGenerate={(intent) => {
          setAssistantSurface("overview-card");
          skipIntentEntry();
          startAgent(intent);
        }}
        onSkip={skipIntentEntry}
        onRecordWalkthrough={() => {
          skipIntentEntry();
          setCurrentView("canvas");
          useStore.getState().openCdpModal();
        }}
        loading={agentStatus === "running"}
      />
    );
  }

  return (
    <div className="flex flex-1 flex-col overflow-hidden">
      <WorkflowRow />
      <StatsStrip onOpenSkillsManager={() => setSkillsDrawerOpen(true)} />
      {skillsDrawerOpen && (
        <div className="fixed inset-y-0 right-0 z-30 flex w-[420px] flex-col border-l border-[var(--hairline-strong)] bg-[var(--oxide)] shadow-2xl">
          <div className="flex items-center justify-between border-b border-[var(--hairline)] px-4 py-2">
            <h3 className="text-[12px] font-medium text-[var(--text-primary)]">
              Skills
            </h3>
            <button
              onClick={() => setSkillsDrawerOpen(false)}
              className="rounded p-1 text-[var(--text-muted)] hover:bg-[var(--bg-hover)] hover:text-[var(--text-primary)]"
            >
              ×
            </button>
          </div>
          <div className="min-h-0 flex-1 overflow-y-auto">
            <SkillsPanel />
          </div>
        </div>
      )}
      <div className="grid min-h-0 flex-1 grid-cols-1 gap-3 overflow-y-auto px-6 pb-3 min-[900px]:grid-cols-12 min-[900px]:overflow-hidden">
        <div className="cw-card-stagger cw-card-stagger-1 h-[340px] min-h-0 min-[900px]:col-span-7 min-[900px]:h-auto">
          <OverviewAssistantCard />
        </div>
        <div className="grid h-[460px] min-h-0 grid-rows-2 gap-3 min-[900px]:col-span-5 min-[900px]:h-auto">
          <div className="cw-card-stagger cw-card-stagger-2 min-h-0">
            <LiveRuntimeCard />
          </div>
          <div className="cw-card-stagger cw-card-stagger-3 min-h-0">
            <CanvasPreviewCard />
          </div>
        </div>
      </div>
    </div>
  );
}
