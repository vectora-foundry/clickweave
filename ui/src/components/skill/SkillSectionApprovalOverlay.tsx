/**
 * `SkillSectionApprovalOverlay` — inline Allow/Deny block rendered on a
 * `SkillSectionCard` when an `ApprovalRequired` or `SupervisionPaused`
 * event targets that section.
 *
 * Rendered by `SkillSectionCard` when
 * `store.sectionApproval.scope.section_id === card.section_id`.
 *
 * Calls `supervision_respond` for executor-paused events (skill runner
 * pauses) and `approve_agent_action` for agent approval gates.
 */

import { commands } from "../../bindings";
import { useStore } from "../../store/useAppStore";
import type { SectionApprovalPause } from "../../store/slices/executionSlice";
import { errorMessage } from "../../utils/commandError";

interface SkillSectionApprovalOverlayProps {
  approval: SectionApprovalPause;
}

export function SkillSectionApprovalOverlay({
  approval,
}: SkillSectionApprovalOverlayProps) {
  const setSectionApproval = useStore((s) => s.setSectionApproval);
  const pushLog = useStore((s) => s.pushLog);

  const handleAllow = async () => {
    setSectionApproval(null);
    // Try executor supervision_respond first (skill runner pauses), fall back
    // to approve_agent_action for agent approval gates.
    const result = await commands.supervisionRespond("retry");
    if (result.status === "error") {
      // Not an executor pause — dispatch as agent approval.
      const agentResult = await commands.approveAgentAction(true);
      if (agentResult.status === "error") {
        pushLog(`Approval failed: ${errorMessage(agentResult.error)}`);
      }
    }
  };

  const handleDeny = async () => {
    setSectionApproval(null);
    const result = await commands.supervisionRespond("abort");
    if (result.status === "error") {
      const agentResult = await commands.approveAgentAction(false);
      if (agentResult.status === "error") {
        pushLog(`Deny failed: ${errorMessage(agentResult.error)}`);
      }
    }
  };

  return (
    <div
      className="border-t border-amber-500/40 bg-amber-950/30 px-3 py-2"
      data-testid="approval-overlay"
    >
      <p className="mb-2 text-[11px] text-amber-200">
        {approval.finding}
      </p>
      {approval.screenshot && (
        <img
          src={`data:image/png;base64,${approval.screenshot}`}
          alt="Screenshot at approval gate"
          className="mb-2 max-h-32 w-auto rounded border border-amber-500/20"
        />
      )}
      <div className="flex gap-2">
        <button
          type="button"
          data-testid="approval-allow"
          onClick={handleAllow}
          className="rounded bg-emerald-600 px-3 py-1 text-xs font-medium text-white hover:bg-emerald-500"
        >
          Allow
        </button>
        <button
          type="button"
          data-testid="approval-deny"
          onClick={handleDeny}
          className="rounded bg-red-700 px-3 py-1 text-xs font-medium text-white hover:bg-red-600"
        >
          Deny
        </button>
      </div>
    </div>
  );
}
