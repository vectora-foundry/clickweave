import { memo } from "react";
import { Handle, Position, type NodeProps } from "@xyflow/react";

interface LoopGroupData {
  label: string;
  bodyCount: number;
  isActive: boolean;
  enabled: boolean;
  onToggleCollapse: () => void;
  [key: string]: unknown;
}

export const LoopGroupNode = memo(function LoopGroupNode({
  data,
  selected,
}: NodeProps) {
  const d = data as unknown as LoopGroupData;
  const { label, bodyCount, isActive, enabled, onToggleCollapse } = d;

  return (
    <div
      className={`relative rounded-lg border-2 border-dashed transition-all duration-150 ${
        !enabled ? "opacity-50" : ""
      }`}
      style={{
        borderColor: selected ? "#10b981" : "#10b98166",
        backgroundColor: "rgba(16, 185, 129, 0.03)",
        width: "100%",
        height: "100%",
        minWidth: 300,
        minHeight: 150,
        boxShadow: selected ? "0 0 12px #10b98133" : "none",
      }}
    >
      <Handle
        type="target"
        position={Position.Left}
        className="!h-3 !w-3 !rounded-full !border-2 !bg-[var(--bg-panel)]"
        style={{ borderColor: "var(--accent-green)" }}
      />

      {isActive && (
        <span className="absolute -right-1 -top-1 h-3 w-3 animate-pulse rounded-full bg-[var(--accent-green)]" />
      )}

      {/* Header bar */}
      <div className="flex items-center gap-2 rounded-t-md bg-[rgba(16,185,129,0.08)] px-3 py-1.5">
        <div className="flex h-5 w-5 items-center justify-center rounded text-[9px] font-bold text-white bg-[#10b981]">
          LP
        </div>
        <span className="text-xs font-medium text-[var(--text-primary)]">
          {label}
        </span>
        <span className="ml-auto text-[10px] text-[var(--text-muted)]">
          {bodyCount} step{bodyCount !== 1 ? "s" : ""}
        </span>
        <button
          onClick={(e) => {
            e.stopPropagation();
            onToggleCollapse();
          }}
          className="flex h-5 w-5 items-center justify-center rounded text-[10px] text-[var(--text-muted)] hover:bg-[rgba(255,255,255,0.1)] hover:text-[var(--text-primary)]"
          title="Collapse loop"
        >
          &#x25BC;
        </button>
      </div>

      {/* LoopDone exit handle — body handle removed since containment communicates it */}
      <Handle
        type="source"
        position={Position.Right}
        id="LoopDone"
        className="!h-3 !w-3 !rounded-full !border-2 !bg-[var(--bg-panel)]"
        style={{ borderColor: "#f59e0b" }}
      />
    </div>
  );
});
