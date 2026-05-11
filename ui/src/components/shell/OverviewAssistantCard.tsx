import { useRef, useState } from "react";
import { useShallow } from "zustand/react/shallow";
import { useStore } from "../../store/useAppStore";
import { isAgentActive } from "../../store/slices/agentSlice";
import { isWalkthroughCapturing } from "../../store/slices/walkthroughSlice";
import { AssistantThread } from "./AssistantThread";

/**
 * Overview-specific chrome around `AssistantThread`. Adds:
 *  - a Live dot in the header that pulses while the agent is active
 *  - a Current Goal banner sourced from the active run's `activeSubgoal`
 *    (with `workflow.intent` as fallback when no run is live)
 *  - the Clear-icon affordance inside the body via `showClearIcon` (D14)
 *
 * Per D6 / D28 the only chromatic accent in this net-new shell chrome
 * is coral; the embedded `AssistantThread` keeps its existing palette.
 */
export function OverviewAssistantCard() {
  // `activeSubgoal` is NOT a top-level store field; it lives on each
  // per-run `RunTrace`. Read through `runTraces[agentRunId]?.activeSubgoal`
  // and fall back to `workflow.intent`.
  const {
    assistantError,
    messages,
    agentStatus,
    completionDisagreement,
    traceSubgoal,
    intent,
  } = useStore(
    useShallow((s) => ({
      assistantError: s.assistantError,
      messages: s.messages,
      agentStatus: s.agentStatus,
      completionDisagreement: s.completionDisagreement,
      traceSubgoal: s.agentRunId
        ? (s.runTraces[s.agentRunId]?.activeSubgoal ?? null)
        : null,
      intent: s.projectIntent,
    })),
  );

  const live = isAgentActive(agentStatus, completionDisagreement);
  const goal = traceSubgoal || intent;
  const sendInFlightRef = useRef(false);
  const [sendInFlight, setSendInFlight] = useState(false);
  const handleSendMessage = async (message: string) => {
    if (sendInFlightRef.current) return;
    sendInFlightRef.current = true;
    setSendInFlight(true);
    try {
      const state = useStore.getState();
      if (isWalkthroughCapturing(state.walkthroughStatus)) {
        await state.cancelWalkthrough();
      }
      await useStore.getState().startAgent(message);
    } finally {
      sendInFlightRef.current = false;
      setSendInFlight(false);
    }
  };

  return (
    <section className="flex h-full min-w-0 flex-col overflow-hidden rounded-[var(--radius-card)] border border-[var(--hairline)] bg-[var(--oxide)]">
      <header className="flex items-center justify-between border-b border-[var(--hairline)] px-4 py-2.5">
        <div className="flex items-center gap-2">
          <span
            className={`h-1.5 w-1.5 rounded-full ${live ? "bg-[var(--accent-coral)] animate-pulse" : "bg-[var(--text-muted)]"}`}
          />
          <h2 className="text-[12px] font-medium tracking-[0.06em] text-[var(--text-primary)]">
            Assistant
          </h2>
        </div>
      </header>
      {goal && (
        <div className="flex min-w-0 items-center border-b border-[var(--hairline)] bg-[var(--bloom-coral)] px-4 py-2 text-[11px] text-[var(--text-secondary)]">
          <span className="shrink-0 font-mono text-[10px] uppercase tracking-[0.18em] text-[var(--text-muted)]">
            Current Goal&nbsp;
          </span>
          <span
            className="min-w-0 truncate text-[var(--text-primary)]"
            title={goal}
          >
            {goal}
          </span>
        </div>
      )}
      <div className="min-h-0 min-w-0 flex-1">
        <AssistantThread
          error={assistantError}
          messages={messages}
          onSendMessage={handleSendMessage}
          showHeader={false}
          showClearIcon={true}
          composerDisabled={sendInFlight}
        />
      </div>
    </section>
  );
}
