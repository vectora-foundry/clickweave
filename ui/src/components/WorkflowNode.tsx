import { memo } from "react";
import { Handle, Position, type NodeProps } from "@xyflow/react";
import type { NodeRole } from "../bindings";

interface WorkflowNodeData {
  label: string;
  nodeType: string;
  icon: string;
  color: string;
  isActive: boolean;
  enabled: boolean;
  onDelete: () => void;
  switchCases: string[];
  role: NodeRole;
  bodyCount?: number;
  onToggleCollapse?: () => void;
  subtitle?: string;
  [key: string]: unknown;
}

const CONTROL_FLOW_TYPES = new Set(["If", "Switch", "Loop", "EndLoop"]);

function SourceHandles({ data }: { data: WorkflowNodeData }) {
  const { nodeType, switchCases } = data;

  if (nodeType === "If") {
    return (
      <>
        <Handle
          type="source"
          position={Position.Right}
          id="IfTrue"
          className="!h-3 !w-3 !rounded-full !border-2 !bg-[var(--bg-panel)]"
          style={{ borderColor: "#10b981", top: "30%" }}
        />
        <span
          className="absolute right-5 text-[8px] text-[var(--text-muted)]"
          style={{ top: "26%" }}
        >
          T
        </span>
        <Handle
          type="source"
          position={Position.Right}
          id="IfFalse"
          className="!h-3 !w-3 !rounded-full !border-2 !bg-[var(--bg-panel)]"
          style={{ borderColor: "#ef4444", top: "70%" }}
        />
        <span
          className="absolute right-5 text-[8px] text-[var(--text-muted)]"
          style={{ top: "66%" }}
        >
          F
        </span>
      </>
    );
  }

  // Collapsed loops render here; expanded loops use LoopGroupNode
  if (nodeType === "Loop") {
    return (
      <Handle
        type="source"
        position={Position.Right}
        id="LoopDone"
        className="!h-3 !w-3 !rounded-full !border-2 !bg-[var(--bg-panel)]"
        style={{ borderColor: "#f59e0b" }}
      />
    );
  }

  if (nodeType === "Switch") {
    const totalHandles = switchCases.length + 1; // cases + default
    return (
      <>
        {switchCases.map((caseName, i) => {
          const pct = ((i + 1) / (totalHandles + 1)) * 100;
          return (
            <span key={caseName}>
              <Handle
                type="source"
                position={Position.Right}
                id={`SwitchCase:${caseName}`}
                className="!h-3 !w-3 !rounded-full !border-2 !bg-[var(--bg-panel)]"
                style={{ borderColor: "#10b981", top: `${pct}%` }}
              />
              <span
                className="absolute right-5 text-[8px] text-[var(--text-muted)] whitespace-nowrap"
                style={{ top: `${pct - 4}%` }}
              >
                {caseName}
              </span>
            </span>
          );
        })}
        {/* Default handle */}
        {(() => {
          const pct = (totalHandles / (totalHandles + 1)) * 100;
          return (
            <span>
              <Handle
                type="source"
                position={Position.Right}
                id="SwitchDefault"
                className="!h-3 !w-3 !rounded-full !border-2 !bg-[var(--bg-panel)]"
                style={{ borderColor: "#666", top: `${pct}%` }}
              />
              <span
                className="absolute right-5 text-[8px] text-[var(--text-muted)]"
                style={{ top: `${pct - 4}%` }}
              >
                default
              </span>
            </span>
          );
        })()}
      </>
    );
  }

  // EndLoop and regular nodes: single source handle
  return (
    <Handle
      type="source"
      position={Position.Right}
      className="!h-3 !w-3 !rounded-full !border-2 !bg-[var(--bg-panel)]"
      style={{ borderColor: "var(--accent-coral)" }}
      isConnectable={!data.hideSourceHandle}
    />
  );
}

export const WorkflowNode = memo(function WorkflowNode({
  data,
  selected,
}: NodeProps) {
  const d = data as unknown as WorkflowNodeData;
  const {
    label,
    icon,
    color,
    isActive,
    enabled,
    onDelete,
    nodeType,
    role,
    bodyCount,
    onToggleCollapse,
    subtitle,
  } = d;
  const isVerification = role === "Verification";
  const isCollapsedGroup = bodyCount != null;
  const isControlFlow = CONTROL_FLOW_TYPES.has(nodeType);
  const needsTallNode = nodeType === "If";
  const needsExtraTallNode = nodeType === "Switch" && d.switchCases.length > 1;

  return (
    <div
      className={`group relative min-w-[140px] rounded-lg border-2 bg-[var(--bg-panel)] transition-shadow ${
        !enabled ? "opacity-50" : ""
      } ${needsTallNode ? "min-h-[60px]" : ""} ${needsExtraTallNode ? "min-h-[80px]" : ""}`}
      style={{
        borderColor: selected ? color : isVerification ? "#f59e0b" : isControlFlow ? "#10b98144" : "var(--border)",
        boxShadow: selected ? `0 0 12px ${color}33` : "none",
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

      {isVerification && (
        <span className="absolute -left-1 -top-1 flex h-4 w-4 items-center justify-center rounded-full bg-amber-500 text-[8px] font-bold text-white">
          ✓
        </span>
      )}

      <button
        onClick={(e) => {
          e.stopPropagation();
          onDelete();
        }}
        className="absolute -right-2 -top-2 hidden h-5 w-5 items-center justify-center rounded-full bg-red-500 text-[10px] text-white group-hover:flex"
      >
        x
      </button>

      <div className="flex items-center gap-2 px-3 py-2">
        <div
          className="flex h-7 w-7 items-center justify-center rounded text-[10px] font-bold text-white"
          style={{ backgroundColor: color }}
        >
          {icon}
        </div>
        <div className="flex flex-col min-w-0 max-w-[180px]">
          <span className="text-xs font-medium text-[var(--text-primary)] truncate">
            {label}
          </span>
          {subtitle && (
            <span className="text-[10px] text-[var(--text-muted)] truncate max-w-full">
              {subtitle}
            </span>
          )}
        </div>
        {isCollapsedGroup && bodyCount != null && (
          <span className="text-[10px] text-[var(--text-muted)] transition-opacity duration-150">
            {bodyCount} {bodyCount === 1 ? "step" : "steps"}
          </span>
        )}
        {isCollapsedGroup && onToggleCollapse && (
          <button
            onClick={(e) => {
              e.stopPropagation();
              onToggleCollapse();
            }}
            className="ml-auto flex h-5 w-5 items-center justify-center rounded text-[10px] text-[var(--text-muted)] transition-opacity duration-150 hover:bg-[var(--bg-surface)] hover:text-[var(--text-primary)]"
            title="Expand loop"
          >
            &#x25B6;
          </button>
        )}
      </div>

      <SourceHandles data={d} />
    </div>
  );
});
