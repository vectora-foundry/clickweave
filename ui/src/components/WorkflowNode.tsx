import { memo } from "react";
import { Handle, Position, type NodeProps } from "@xyflow/react";
import type { NodeRole } from "../bindings";
import { InlineRenameInput } from "./InlineRenameInput";

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
  isRenaming?: boolean;
  hideSourceHandle?: boolean;
  onRenameConfirm?: (newName: string) => void;
  onRenameCancel?: () => void;
  [key: string]: unknown;
}

const CONTROL_FLOW_TYPES = new Set(["If", "Switch", "Loop", "EndLoop"]);
const HANDLE_CLS = "!h-3 !w-3 !rounded-full !border-2 !bg-[var(--bg-panel)]";

function SourceHandles({ data }: { data: WorkflowNodeData }) {
  const { nodeType, switchCases } = data;

  if (nodeType === "If") {
    return (
      <>
        <Handle type="source" position={Position.Right} id="IfTrue"
          className={HANDLE_CLS} style={{ borderColor: "#10b981", top: "30%" }} />
        <span className="absolute right-5 text-[8px] text-[var(--text-muted)]"
          style={{ top: "26%" }}>T</span>
        <Handle type="source" position={Position.Right} id="IfFalse"
          className={HANDLE_CLS} style={{ borderColor: "#ef4444", top: "70%" }} />
        <span className="absolute right-5 text-[8px] text-[var(--text-muted)]"
          style={{ top: "66%" }}>F</span>
      </>
    );
  }

  if (nodeType === "Loop") {
    return (
      <Handle type="source" position={Position.Right} id="LoopDone"
        className={HANDLE_CLS} style={{ borderColor: "#f59e0b" }} />
    );
  }

  if (nodeType === "Switch") {
    const totalHandles = switchCases.length + 1;
    return (
      <>
        {switchCases.map((caseName, i) => {
          const pct = ((i + 1) / (totalHandles + 1)) * 100;
          return (
            <span key={caseName}>
              <Handle type="source" position={Position.Right} id={`SwitchCase:${caseName}`}
                className={HANDLE_CLS} style={{ borderColor: "#10b981", top: `${pct}%` }} />
              <span className="absolute right-5 text-[8px] text-[var(--text-muted)] whitespace-nowrap"
                style={{ top: `${pct - 4}%` }}>{caseName}</span>
            </span>
          );
        })}
        {(() => {
          const pct = (totalHandles / (totalHandles + 1)) * 100;
          return (
            <span>
              <Handle type="source" position={Position.Right} id="SwitchDefault"
                className={HANDLE_CLS} style={{ borderColor: "#666", top: `${pct}%` }} />
              <span className="absolute right-5 text-[8px] text-[var(--text-muted)]"
                style={{ top: `${pct - 4}%` }}>default</span>
            </span>
          );
        })()}
      </>
    );
  }

  return (
    <Handle type="source" position={Position.Right}
      className={HANDLE_CLS} style={{ borderColor: "var(--accent-coral)" }}
      isConnectable={!data.hideSourceHandle} />
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
    isRenaming,
    onRenameConfirm,
    onRenameCancel,
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
        className={HANDLE_CLS}
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
          {isRenaming && onRenameConfirm && onRenameCancel ? (
            <InlineRenameInput label={label} onConfirm={onRenameConfirm} onCancel={onRenameCancel} />
          ) : (
            <span className="text-xs font-medium text-[var(--text-primary)] truncate">
              {label}
            </span>
          )}
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
