interface HeaderProps {
  workflowName: string;
  executorState: "idle" | "running";
  lastRunStatus: "completed" | "failed" | null;
  onSave: () => void;
  onSettings: () => void;
  onNameChange: (name: string) => void;
}

export function Header({
  workflowName,
  executorState,
  lastRunStatus,
  onSave,
  onSettings,
  onNameChange,
}: HeaderProps) {
  const isRunning = executorState === "running";
  const statusColor = lastRunStatus === "completed"
    ? "var(--accent-green)"
    : lastRunStatus === "failed"
      ? "var(--accent-coral)"
      : undefined;

  return (
    <div
      className="flex h-10 items-center justify-between border-b border-[var(--border)] bg-[var(--bg-panel)] pr-3"
      data-tauri-drag-region
      style={{ paddingLeft: 78 } as React.CSSProperties}
    >
      {/* Left: logo + name + status */}
      <div className="flex items-center gap-2.5 min-w-0" data-tauri-drag-region>
        <span className="text-base font-bold text-[var(--accent-coral)] leading-none shrink-0" data-tauri-drag-region>
          C
        </span>
        <div className="h-4 w-px bg-[var(--border)] shrink-0" data-tauri-drag-region />
        <input
          type="text"
          value={workflowName}
          onChange={(e) => onNameChange(e.target.value)}
          spellCheck={false}
          className="border border-transparent bg-transparent text-[13px] font-medium text-[var(--text-primary)] outline-none rounded px-1.5 py-0.5 hover:border-[var(--border)] hover:bg-[var(--bg-input)] focus:border-[var(--accent-coral)] focus:bg-[var(--bg-input)] min-w-[80px] max-w-[200px]"
        />

        {/* Status */}
        {isRunning ? (
          <span className="flex items-center gap-1.5 text-[11px] text-[var(--accent-green)] shrink-0" data-tauri-drag-region>
            <span className="h-1.5 w-1.5 rounded-full bg-[var(--accent-green)] animate-pulse" data-tauri-drag-region />
            Running
            <span className="text-[var(--text-muted)] text-[10px]" data-tauri-drag-region>
              (⌘⇧Esc to stop)
            </span>
          </span>
        ) : lastRunStatus ? (
          <span className="flex items-center gap-1.5 text-[11px] shrink-0" data-tauri-drag-region style={{ color: statusColor }}>
            <span className="h-1.5 w-1.5 rounded-full" data-tauri-drag-region style={{ backgroundColor: statusColor }} />
            Last run: {lastRunStatus === "completed" ? "Completed" : "Failed"}
          </span>
        ) : null}
      </div>

      {/* Right: gear + save icons */}
      <div className="flex items-center gap-0.5 shrink-0">
        <button
          onClick={onSettings}
          title="Settings"
          className="flex h-7 w-7 items-center justify-center rounded-md text-[var(--text-secondary)] hover:bg-[var(--bg-hover)] hover:text-[var(--text-primary)]"
        >
          <svg
            width="15"
            height="15"
            viewBox="0 0 16 16"
            fill="none"
            stroke="currentColor"
            strokeWidth="1.3"
            strokeLinecap="round"
            strokeLinejoin="round"
          >
            <circle cx="8" cy="8" r="2.5" />
            <path d="M8 1.5v1.7M8 12.8v1.7M1.5 8h1.7M12.8 8h1.7M3.4 3.4l1.2 1.2M11.4 11.4l1.2 1.2M3.4 12.6l1.2-1.2M11.4 4.6l1.2-1.2" />
          </svg>
        </button>
        <button
          onClick={onSave}
          title="Save (⌘S)"
          className="flex h-7 w-7 items-center justify-center rounded-md text-[var(--accent-coral)] hover:bg-[color-mix(in_srgb,var(--accent-coral)_12%,transparent)]"
        >
          <svg
            width="15"
            height="15"
            viewBox="0 0 16 16"
            fill="none"
            stroke="currentColor"
            strokeWidth="1.3"
            strokeLinecap="round"
            strokeLinejoin="round"
          >
            <path d="M13.5 14.5h-11a1 1 0 01-1-1v-11a1 1 0 011-1h8.5l3.5 3.5v8.5a1 1 0 01-1 1z" />
            <path d="M11.5 14.5v-4h-7v4" />
            <path d="M4.5 1.5v3h5" />
          </svg>
        </button>
      </div>
    </div>
  );
}
