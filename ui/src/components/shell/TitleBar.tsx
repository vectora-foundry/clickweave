import { Save, Settings } from "lucide-react";
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
          <Settings size={15} strokeWidth={1.5} />
        </button>
        <button
          onClick={saveProject}
          title="Save (⌘S)"
          className="flex h-7 w-7 items-center justify-center rounded-md text-[var(--accent-coral)] hover:bg-[color-mix(in_srgb,var(--accent-coral)_12%,transparent)]"
        >
          <Save size={15} strokeWidth={1.5} />
        </button>
      </div>
    </div>
  );
}
