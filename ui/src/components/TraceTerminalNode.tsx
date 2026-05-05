import { memo } from "react";
import { Handle, Position, type NodeProps } from "@xyflow/react";
import type { TerminalFrame } from "../store/slices/assistantSlice";

export interface TraceTerminalNodeData extends Record<string, unknown> {
  frame: TerminalFrame;
}

const tone: Record<TerminalFrame["kind"], string> = {
  complete: "border-emerald-500/40 bg-emerald-500/10 text-emerald-200",
  stopped: "border-zinc-500/40 bg-zinc-500/10 text-zinc-200",
  error: "border-red-500/40 bg-red-500/10 text-red-200",
  disagreement_cancelled:
    "border-orange-500/40 bg-orange-500/10 text-orange-200",
};

const label: Record<TerminalFrame["kind"], string> = {
  complete: "Complete",
  stopped: "Stopped",
  error: "Error",
  disagreement_cancelled: "Cancelled",
};

function TraceTerminalNodeImpl({ data }: NodeProps) {
  const { frame } = data as TraceTerminalNodeData;
  return (
    <div
      className={`min-w-[260px] max-w-[320px] rounded-md border px-2.5 py-1.5 text-xs shadow-sm ${tone[frame.kind]}`}
    >
      <Handle type="target" position={Position.Top} className="!bg-zinc-500" />
      <div className="font-medium">{label[frame.kind]}</div>
      <p className="mt-0.5 break-words text-[var(--text-secondary)]">
        {frame.detail}
      </p>
    </div>
  );
}

export const TraceTerminalNode = memo(TraceTerminalNodeImpl);
