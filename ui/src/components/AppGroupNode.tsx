import { memo } from "react";
import { type NodeProps } from "@xyflow/react";

interface AppGroupData {
  appName: string;
  color: string;
  memberCount: number;
  isActive: boolean;
  onToggleCollapse: () => void;
  [key: string]: unknown;
}

export const AppGroupNode = memo(function AppGroupNode({
  data,
  selected,
}: NodeProps) {
  const d = data as unknown as AppGroupData;
  const { appName, color, memberCount, isActive, onToggleCollapse } = d;
  const rgb = hexToRgb(color);

  return (
    <div
      className="relative rounded-[10px] transition-all duration-150"
      style={{
        border: `1px solid rgba(${rgb}, 0.3)`,
        backgroundColor: `rgba(${rgb}, 0.06)`,
        width: "100%",
        height: "100%",
        minWidth: 300,
        minHeight: 150,
        boxShadow: selected ? `0 0 12px rgba(${rgb}, 0.2)` : "none",
      }}
    >
      {isActive && (
        <span
          className="absolute -right-1 -top-1 h-3 w-3 animate-pulse rounded-full"
          style={{ backgroundColor: color }}
        />
      )}

      {/* Header bar */}
      <div
        className="flex items-center gap-2 px-3 py-1.5"
        style={{ borderBottom: `1px solid rgba(${rgb}, 0.15)` }}
      >
        <div
          className="h-4 w-1 rounded-sm"
          style={{ backgroundColor: color }}
        />
        <span className="text-xs font-medium text-[var(--text-primary)]">
          {appName}
        </span>
        <span className="ml-auto text-[10px] text-[var(--text-muted)]">
          {memberCount} step{memberCount !== 1 ? "s" : ""}
        </span>
        <button
          onClick={(e) => {
            e.stopPropagation();
            onToggleCollapse();
          }}
          className="flex h-5 w-5 items-center justify-center rounded text-[10px] text-[var(--text-muted)] hover:bg-[rgba(255,255,255,0.1)] hover:text-[var(--text-primary)]"
          title="Collapse group"
        >
          &#x25BC;
        </button>
      </div>

    </div>
  );
});

/** Convert hex color like "#6366f1" to "99, 102, 241" for rgba(). */
function hexToRgb(hex: string): string {
  const r = parseInt(hex.slice(1, 3), 16);
  const g = parseInt(hex.slice(3, 5), 16);
  const b = parseInt(hex.slice(5, 7), 16);
  return `${r}, ${g}, ${b}`;
}
