import { useEffect, useMemo, useRef } from "react";
import { useStore } from "../store/useAppStore";
import type {
  AgentPhase,
  RunTrace,
  TerminalFrame,
  TraceMilestone,
  TraceStep,
  WorldModelDelta,
} from "../store/slices/assistantSlice";

type TraceEntry =
  | { type: "milestone"; value: TraceMilestone }
  | { type: "step"; value: TraceStep }
  | { type: "delta"; value: WorldModelDelta };

const phaseTone: Record<AgentPhase, string> = {
  exploring: "border-sky-500/40 bg-sky-500/10 text-sky-300",
  executing: "border-emerald-500/40 bg-emerald-500/10 text-emerald-300",
  recovering: "border-amber-500/40 bg-amber-500/10 text-amber-300",
};

const phaseLabel: Record<AgentPhase, string> = {
  exploring: "Exploring",
  executing: "Executing",
  recovering: "Recovering",
};

const terminalTone: Record<TerminalFrame["kind"], string> = {
  complete: "border-emerald-500/40 bg-emerald-500/10 text-emerald-200",
  stopped: "border-zinc-500/40 bg-zinc-500/10 text-zinc-200",
  error: "border-red-500/40 bg-red-500/10 text-red-200",
  disagreement_cancelled: "border-orange-500/40 bg-orange-500/10 text-orange-200",
};

const terminalLabel: Record<TerminalFrame["kind"], string> = {
  complete: "Complete",
  stopped: "Stopped",
  error: "Error",
  disagreement_cancelled: "Cancelled",
};

export function RunTraceView({ runId }: { runId: string }) {
  const trace = useStore((s) => s.runTraces[runId]);
  const messagesEndRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    messagesEndRef.current?.scrollIntoView?.({ behavior: "smooth" });
  }, [trace?.steps.length, trace?.terminalFrame]);

  const entries = useMemo(
    () =>
      trace
        ? interleaveStepsAndMilestones(
            trace.steps,
            trace.milestones,
            trace.worldModelDeltas,
          )
        : [],
    [trace],
  );

  if (!trace) {
    return (
      <div className="agent-running-fallback mx-3 mb-2 rounded border border-[var(--border)] bg-[var(--bg-hover)] px-3 py-2 text-xs text-[var(--text-secondary)]">
        Agent running...
      </div>
    );
  }

  return (
    <div className="run-trace-view mx-3 mb-2 min-w-0 rounded border border-[var(--border)] bg-[var(--bg-dark)]">
      <div className="flex min-w-0 items-center gap-2 border-b border-[var(--border)] px-3 py-2">
        <PhaseChip phase={trace.phase} />
        <span className="min-w-0 flex-1 truncate text-xs text-[var(--text-secondary)]">
          {trace.activeSubgoal || "No active subgoal"}
        </span>
      </div>

      <ol className="trace-steps max-h-72 min-w-0 space-y-2 overflow-y-auto px-3 py-2">
        {entries.length === 0 ? (
          <li className="text-xs text-[var(--text-muted)]">Waiting for first step</li>
        ) : (
          entries.map((entry, index) => {
            const inProgress =
              !trace.terminalFrame &&
              entry.type === "step" &&
              !entry.value.failed &&
              index === entries.length - 1;
            return renderEntry(entry, index, inProgress);
          })
        )}
      </ol>

      {trace.terminalFrame && <TerminalFrameBlock frame={trace.terminalFrame} />}
      <div ref={messagesEndRef} />
    </div>
  );
}

export function interleaveStepsAndMilestones(
  steps: TraceStep[],
  milestones: TraceMilestone[],
  worldModelDeltas: WorldModelDelta[],
): TraceEntry[] {
  const entries: TraceEntry[] = [
    ...milestones.map((value) => ({ type: "milestone" as const, value })),
    ...steps.map((value) => ({ type: "step" as const, value })),
    ...worldModelDeltas.map((value) => ({ type: "delta" as const, value })),
  ];

  const order: Record<TraceEntry["type"], number> = {
    milestone: 0,
    step: 1,
    delta: 2,
  };

  return entries.sort((a, b) => {
    const stepDiff = a.value.stepIndex - b.value.stepIndex;
    if (stepDiff !== 0) return stepDiff;
    return order[a.type] - order[b.type];
  });
}

function PhaseChip({ phase }: { phase: AgentPhase }) {
  return (
    <span
      className={`shrink-0 rounded border px-1.5 py-0.5 text-[10px] font-medium ${phaseTone[phase]}`}
    >
      {phaseLabel[phase]}
    </span>
  );
}

function TerminalFrameBlock({ frame }: { frame: TerminalFrame }) {
  return (
    <div className={`border-t px-3 py-2 text-xs ${terminalTone[frame.kind]}`}>
      <span className="font-medium">{terminalLabel[frame.kind]}</span>
      <span className="ml-2 break-words text-[var(--text-secondary)]">
        {frame.detail}
      </span>
    </div>
  );
}

function renderEntry(entry: TraceEntry, index: number, inProgress = false) {
  if (entry.type === "milestone") {
    return (
      <li
        key={`milestone-${entry.value.stepIndex}-${index}`}
        className="rounded border border-[var(--accent-blue)]/40 bg-[var(--accent-blue)]/10 px-2 py-1.5 text-xs"
      >
        <div className="flex items-center gap-2">
          <span className="font-medium text-[var(--accent-blue)]">
            {entry.value.kind === "subgoal_completed" ? "Milestone" : "Recovered"}
          </span>
          <span className="text-[10px] text-[var(--text-muted)]">
            Step {entry.value.stepIndex}
          </span>
        </div>
        <p className="mt-0.5 break-words text-[var(--text-secondary)]">
          {entry.value.text}
        </p>
      </li>
    );
  }

  if (entry.type === "delta") {
    return (
      <li
        key={`delta-${entry.value.stepIndex}-${index}`}
        className="rounded border border-[var(--border)] bg-[var(--bg-panel)] px-2 py-1.5 text-xs"
      >
        <div className="flex items-center gap-2">
          <span className="font-medium text-[var(--text-primary)]">World model</span>
          <span className="text-[10px] text-[var(--text-muted)]">
            Step {entry.value.stepIndex}
          </span>
        </div>
        <p className="mt-0.5 break-words font-mono text-[11px] text-[var(--text-muted)]">
          {entry.value.changedFields.join(", ") || "No field changes"}
        </p>
      </li>
    );
  }

  return (
    <li
      key={`step-${entry.value.stepIndex}-${index}`}
      className={`rounded border px-2 py-1.5 text-xs ${
        entry.value.failed
          ? "border-red-500/40 bg-red-500/10"
          : "border-[var(--border)] bg-[var(--bg-panel)]"
      } ${inProgress ? "cw-bloom-pulse" : ""}`}
    >
      <div className="flex items-center gap-2">
        <span className="font-mono text-[10px] text-[var(--text-muted)]">
          #{entry.value.stepIndex}
        </span>
        <span className="min-w-0 flex-1 truncate font-medium text-[var(--text-primary)]">
          {entry.value.toolName}
        </span>
        <PhaseChip phase={entry.value.phase} />
      </div>
      <p className="mt-1 break-words text-[var(--text-secondary)]">
        {entry.value.body}
      </p>
    </li>
  );
}
