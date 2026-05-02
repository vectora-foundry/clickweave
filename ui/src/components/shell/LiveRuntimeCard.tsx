import { useEffect, useState } from "react";
import { useShallow } from "zustand/react/shallow";
import { useStore } from "../../store/useAppStore";
import { isAgentActive } from "../../store/slices/agentSlice";
import { RunTraceView } from "../RunTraceView";

/**
 * Live Runtime card. Renders phase chip, step N, elapsed time, active
 * tool, and the embedded `RunTraceView` for the active run.
 *
 * Display rules:
 *  - **D11**: render `Step N` only — there is no `totalSteps` field on
 *    the trace. Never fabricate a denominator.
 *  - **D24**: Elapsed = `agentRunFinishedAt ? (finishedAt - startedAt)
 *    : (Date.now() - startedAt)`. Ticks once per second while the agent
 *    is live; freezes when finished. Both fields zero together on the
 *    next `startAgent` (or `clearConversationFlow`).
 *  - **D29**: in the header, show the `Last run: …` pill when idle
 *    (with the lastRunStatus color), or the `⌘⇧Esc to stop` hint while
 *    live.
 *  - **D6 / D28**: coral is the only chromatic accent.
 */
export function LiveRuntimeCard() {
  const {
    agentStatus,
    completionDisagreement,
    pendingApproval,
    activeRunId,
    runTrace,
    agentRunStartedAt,
    agentRunFinishedAt,
    lastRunStatus,
  } = useStore(
    useShallow((s) => ({
      agentStatus: s.agentStatus,
      completionDisagreement: s.completionDisagreement,
      pendingApproval: s.pendingApproval,
      activeRunId: s.agentRunId,
      runTrace: s.agentRunId ? s.runTraces[s.agentRunId] : undefined,
      agentRunStartedAt: s.agentRunStartedAt,
      agentRunFinishedAt: s.agentRunFinishedAt,
      lastRunStatus: s.lastRunStatus,
    })),
  );

  const live = isAgentActive(agentStatus, completionDisagreement);
  const phase = runTrace?.phase ?? null;
  const stepN = runTrace?.steps.length ?? 0;
  const lastStep = runTrace?.steps.at(-1);
  const activeTool = pendingApproval?.toolName ?? lastStep?.toolName ?? null;

  // Tick once per second while live; freezes when finished.
  const elapsed = useElapsed(agentRunStartedAt, agentRunFinishedAt, live);

  return (
    <section className="flex h-full flex-col overflow-hidden rounded-[var(--radius-card)] border border-[var(--hairline)] bg-[var(--oxide)]">
      <header className="flex items-center justify-between border-b border-[var(--hairline)] px-4 py-2.5">
        <div className="flex items-center gap-2">
          <span
            className={`h-1.5 w-1.5 rounded-full ${live ? "bg-[var(--accent-coral)] animate-pulse" : "bg-[var(--text-muted)]"}`}
          />
          <h2 className="text-[12px] font-medium tracking-[0.06em] text-[var(--text-primary)]">
            Live Runtime
          </h2>
        </div>
        {/* D29 — Overview placement of the run-status pill / Esc hint. */}
        {live ? (
          <span className="text-[10px] text-[var(--text-muted)] font-mono">
            ⌘⇧Esc to stop
          </span>
        ) : lastRunStatus ? (
          <span
            className="font-mono text-[10px]"
            style={{
              color:
                lastRunStatus === "completed"
                  ? "var(--accent-green)"
                  : "var(--accent-coral)",
            }}
          >
            Last run:{" "}
            {lastRunStatus === "completed" ? "Completed" : "Failed"}
          </span>
        ) : null}
      </header>

      <dl className="grid grid-cols-[repeat(auto-fit,minmax(88px,1fr))] gap-px border-b border-[var(--hairline)] bg-[var(--hairline)]">
        <Stat label="Phase" value={phase ?? "—"} />
        <Stat label="Step" value={stepN > 0 ? `Step ${stepN}` : "—"} />
        <Stat label="Elapsed" value={formatElapsed(elapsed)} mono />
        <Stat
          label="Active Tool"
          value={activeTool ?? "—"}
          mono
          accent={!!activeTool}
        />
      </dl>

      <div className="min-h-0 flex-1 overflow-y-auto">
        {activeRunId ? (
          <RunTraceView runId={activeRunId} />
        ) : (
          <div className="px-4 py-3 text-[11px] text-[var(--text-muted)]">
            No active run.
          </div>
        )}
      </div>
    </section>
  );
}

interface StatProps {
  label: string;
  value: string;
  mono?: boolean;
  accent?: boolean;
}

function Stat({ label, value, mono, accent }: StatProps) {
  return (
    <div className="flex flex-col gap-0.5 bg-[var(--oxide)] px-3 py-2">
      <div className="text-[9px] font-medium uppercase tracking-[0.18em] text-[var(--text-muted)]">
        {label}
      </div>
      <div
        className={`truncate text-[12px] ${mono ? "font-mono" : ""} ${
          accent ? "text-[var(--accent-coral)]" : "text-[var(--text-primary)]"
        }`}
      >
        {value}
      </div>
    </div>
  );
}

function useElapsed(
  startedAt: number | null,
  finishedAt: number | null,
  live: boolean,
): number | null {
  const [now, setNow] = useState(() => Date.now());
  // Re-anchor `now` whenever a new run begins so the first paint after
  // `startAgent` shows `0:00` instead of the stale `Date.now()` captured
  // by the useState initializer (which can be from a much earlier
  // mount). The 1Hz interval below keeps it advancing while live.
  useEffect(() => {
    if (live && startedAt != null) {
      setNow(Date.now());
    }
  }, [live, startedAt]);
  useEffect(() => {
    if (!live) return;
    const id = window.setInterval(() => setNow(Date.now()), 1000);
    return () => window.clearInterval(id);
  }, [live]);
  if (startedAt == null) return null;
  if (finishedAt != null) return finishedAt - startedAt;
  return now - startedAt;
}

function formatElapsed(ms: number | null): string {
  if (ms == null) return "—";
  const total = Math.max(0, Math.floor(ms / 1000));
  const m = Math.floor(total / 60);
  const s = total % 60;
  return `${m}:${s.toString().padStart(2, "0")}`;
}
