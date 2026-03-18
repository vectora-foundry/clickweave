import { memo } from "react";
import { type NodeProps } from "@xyflow/react";
import { hexToRgb } from "../utils/color";
import { InlineRenameInput } from "./InlineRenameInput";

interface UserGroupData {
  name: string;
  color: string;
  memberCount: number;
  isActive: boolean;
  isRenaming?: boolean;
  onRenameConfirm?: (newName: string) => void;
  onRenameCancel?: () => void;
  onToggleCollapse: () => void;
  [key: string]: unknown;
}

export const UserGroupNode = memo(function UserGroupNode({ data, selected }: NodeProps) {
  const d = data as unknown as UserGroupData;
  const { name, color, memberCount, isActive, isRenaming, onRenameConfirm, onRenameCancel, onToggleCollapse } = d;
  const rgb = hexToRgb(color);

  return (
    <div
      className="relative rounded-[12px] transition-all duration-150"
      style={{
        border: `2px solid rgba(${rgb}, ${selected ? 1 : 0.5})`,
        backgroundColor: `rgba(${rgb}, 0.06)`,
        width: "100%",
        height: "100%",
        minWidth: 300,
        minHeight: 150,
        boxShadow: selected ? `0 0 12px rgba(${rgb}, 0.2)` : "none",
      }}
    >
      {isActive && (
        <span className="absolute -right-1 -top-1 h-3 w-3 animate-pulse rounded-full"
          style={{ backgroundColor: color }} />
      )}

      <div className="flex items-center gap-2 px-3 py-1.5"
        style={{ borderBottom: `1px solid rgba(${rgb}, 0.15)` }}>
        <span className="text-xs">📁</span>
        {isRenaming && onRenameConfirm && onRenameCancel ? (
          <InlineRenameInput label={name} onConfirm={onRenameConfirm} onCancel={onRenameCancel} />
        ) : (
          <span className="text-xs font-medium text-[var(--text-primary)]">{name}</span>
        )}
        <span className="ml-auto text-[10px] text-[var(--text-muted)]">
          {memberCount} step{memberCount !== 1 ? "s" : ""}
        </span>
        <button onClick={(e) => { e.stopPropagation(); onToggleCollapse(); }}
          className="flex h-5 w-5 items-center justify-center rounded text-[10px] text-[var(--text-muted)] hover:bg-[rgba(255,255,255,0.1)] hover:text-[var(--text-primary)]"
          title="Collapse group">
          &#x25BC;
        </button>
      </div>
    </div>
  );
});
