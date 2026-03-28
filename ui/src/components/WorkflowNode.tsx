import { memo } from "react";
import { Handle, Position, type NodeProps } from "@xyflow/react";
import type { NodeRole } from "../bindings";
import { InlineRenameInput } from "./InlineRenameInput";
import type { OutputFieldInfo } from "../utils/outputSchema";
import { typeColor } from "../utils/typeColors";

interface WiredInput {
  key: string;
  fieldType: string;
}

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
  autoId?: string;
  bodyCount?: number;
  onToggleCollapse?: () => void;
  subtitle?: string;
  isRenaming?: boolean;
  hideSourceHandle?: boolean;
  onRenameConfirm?: (newName: string) => void;
  onRenameCancel?: () => void;
  outputFields?: OutputFieldInfo[];
  wiredInputs?: WiredInput[];
  availableInputs?: WiredInput[];
  [key: string]: unknown;
}

const CONTROL_FLOW_TYPES = new Set(["If", "Switch", "Loop", "EndLoop"]);
const EXEC_HANDLE_CLS = "!h-2 !w-2 !rounded-full !border !bg-[var(--bg-panel)]";
const EXEC_STYLE = { borderColor: "var(--text-muted)" };

function SourceHandles({ data }: { data: WorkflowNodeData }) {
  const { nodeType, switchCases } = data;

  if (nodeType === "If") {
    return (
      <>
        <Handle type="source" position={Position.Right} id="IfTrue"
          className={EXEC_HANDLE_CLS} style={{ borderColor: "#10b981", top: "30%" }} />
        <span className="absolute right-5 text-[8px] text-[var(--text-muted)]"
          style={{ top: "26%" }}>T</span>
        <Handle type="source" position={Position.Right} id="IfFalse"
          className={EXEC_HANDLE_CLS} style={{ borderColor: "#ef4444", top: "70%" }} />
        <span className="absolute right-5 text-[8px] text-[var(--text-muted)]"
          style={{ top: "66%" }}>F</span>
      </>
    );
  }

  if (nodeType === "Loop") {
    return (
      <Handle type="source" position={Position.Right} id="LoopDone"
        className={EXEC_HANDLE_CLS} style={{ borderColor: "#f59e0b" }} />
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
                className={EXEC_HANDLE_CLS} style={{ borderColor: "#10b981", top: `${pct}%` }} />
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
                className={EXEC_HANDLE_CLS} style={{ borderColor: "#666", top: `${pct}%` }} />
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
      className={EXEC_HANDLE_CLS} style={EXEC_STYLE}
      isConnectable={!data.hideSourceHandle} />
  );
}

const DATA_PORT_CLS = "!h-2.5 !w-2.5 !rounded-full !border-0";

/** Data port handles — renders colored dots for output fields or wired input refs. */
function PortHandles({ items, type, position, idPrefix, sideOffset }: {
  items: { key: string; color: string }[];
  type: "source" | "target";
  position: typeof Position.Left | typeof Position.Right;
  idPrefix: string;
  sideOffset: "left" | "right";
}) {
  if (items.length === 0) return null;
  return (
    <>
      {items.map((item, i) => {
        const pct = 30 + ((i + 1) / (items.length + 1)) * 60;
        return (
          <Handle
            key={`${idPrefix}${item.key}`}
            type={type}
            position={position}
            id={`${idPrefix}${item.key}`}
            className={DATA_PORT_CLS}
            style={{ top: `${pct}%`, backgroundColor: item.color, [sideOffset]: -10 }}
          />
        );
      })}
    </>
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
    autoId,
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
        className={EXEC_HANDLE_CLS}
        style={EXEC_STYLE}
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
          {autoId && (
            <span className="text-[9px] font-mono text-[var(--text-muted)] opacity-60">
              {autoId}
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
      {d.outputFields && d.outputFields.length > 0 && (
        <PortHandles
          items={d.outputFields.map((f) => ({ key: f.name, color: typeColor(f.type) }))}
          type="source" position={Position.Right} idPrefix="data-" sideOffset="right"
        />
      )}
      {/* Input port dots for all ref-capable params (wired and unwired) */}
      {d.availableInputs && d.availableInputs.length > 0 && (
        <PortHandles
          items={d.availableInputs.map((a) => {
            const wired = d.wiredInputs?.find((w) => w.key === a.key);
            return { key: a.key, color: typeColor(wired?.fieldType ?? a.fieldType) };
          })}
          type="target" position={Position.Left} idPrefix="data-input-" sideOffset="left"
        />
      )}
    </div>
  );
});
