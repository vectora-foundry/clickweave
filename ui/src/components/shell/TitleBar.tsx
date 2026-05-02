import { useStore } from "../../store/useAppStore";

export function TitleBar() {
  const saveProject = useStore((s) => s.saveProject);
  const setShowSettings = useStore((s) => s.setShowSettings);

  return (
    <div
      data-tauri-drag-region
      className="flex h-14 items-center justify-between border-b border-[var(--hairline-strong)] bg-[var(--ink)] px-3"
    >
      {/* Left spacer (matches gear/save button cluster width for centering) */}
      <div className="w-16" data-tauri-drag-region />
      {/* Centered wordmark */}
      <div
        data-tauri-drag-region
        className="select-none text-[13px] font-medium tracking-[0.18em] text-[var(--text-primary)]"
      >
        CLICKWEAVE
      </div>
      {/* Right: settings + save */}
      <div className="flex w-16 items-center justify-end gap-0.5">
        <button
          onClick={() => setShowSettings(true)}
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
          onClick={saveProject}
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
