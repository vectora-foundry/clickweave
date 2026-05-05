import { memo } from "react";
import { Handle, Position, type NodeProps } from "@xyflow/react";
import type { AgentPhase } from "../store/slices/assistantSlice";

export interface TraceStepNodeData extends Record<string, unknown> {
  stepIndex: number;
  toolName: string;
  phase: AgentPhase;
  body: string;
  failed: boolean;
  expanded: boolean;
  changedFields: string[];
  milestoneText: string | null;
  onToggle: (stepIndex: number) => void;
}

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

function TraceStepNodeImpl({ data }: NodeProps) {
  const d = data as TraceStepNodeData;

  const containerClass = d.failed
    ? "border-red-500/60 bg-red-500/10"
    : "border-[var(--border)] bg-[var(--bg-panel)]";

  return (
    <div
      className={`min-w-[260px] max-w-[320px] rounded-md border ${containerClass} text-xs shadow-sm`}
    >
      <Handle type="target" position={Position.Top} className="!bg-zinc-500" />
      <div className="flex w-full cursor-pointer items-center gap-2 px-2.5 py-1.5 text-left">
        <span className="font-mono text-[10px] text-[var(--text-muted)]">
          #{d.stepIndex}
        </span>
        <span className="min-w-0 flex-1 truncate font-medium text-[var(--text-primary)]">
          {d.toolName}
        </span>
        <span
          className={`shrink-0 rounded border px-1.5 py-0.5 text-[10px] font-medium ${phaseTone[d.phase]}`}
        >
          {phaseLabel[d.phase]}
        </span>
        <button
          type="button"
          aria-label={d.expanded ? "Collapse step" : "Expand step"}
          onClick={(e) => {
            e.stopPropagation();
            d.onToggle(d.stepIndex);
          }}
          className="rounded px-1 text-[10px] text-[var(--text-muted)] hover:bg-[var(--bg-hover)] hover:text-[var(--text-primary)]"
        >
          {d.expanded ? "▾" : "▸"}
        </button>
      </div>

      {d.expanded && (
        <div className="border-t border-[var(--border)] px-2.5 py-1.5">
          <p className="break-words text-[var(--text-secondary)]">{d.body}</p>

          {d.changedFields.length > 0 && (
            <div className="mt-1.5 rounded border border-[var(--border)] bg-[var(--bg-dark)] px-2 py-1">
              <div className="text-[10px] font-medium text-[var(--text-primary)]">
                World model
              </div>
              <p className="mt-0.5 break-words font-mono text-[10px] text-[var(--text-muted)]">
                {d.changedFields.join(", ")}
              </p>
            </div>
          )}

          {d.milestoneText && (
            <div className="mt-1.5 rounded border border-[var(--accent-blue)]/40 bg-[var(--accent-blue)]/10 px-2 py-1">
              <div className="text-[10px] font-medium text-[var(--accent-blue)]">
                Milestone
              </div>
              <p className="mt-0.5 break-words text-[10px] text-[var(--text-secondary)]">
                {d.milestoneText}
              </p>
            </div>
          )}
        </div>
      )}
      <Handle type="source" position={Position.Bottom} className="!bg-zinc-500" />
    </div>
  );
}

export const TraceStepNode = memo(TraceStepNodeImpl);
