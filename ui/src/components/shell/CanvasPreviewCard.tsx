import { useStore } from "../../store/useAppStore";
import { CanvasPreviewCanvas } from "./CanvasPreviewCanvas";

/**
 * Overview chrome around `CanvasPreviewCanvas`. The header provides a
 * single "open in canvas" affordance that switches `currentView` to
 * `"canvas"`; full-screen / settings / zoom % controls are deferred.
 *
 * The preview body stays read-only per D12 — it does NOT mount the
 * editor's listeners.
 */
export function CanvasPreviewCard() {
  const setCurrentView = useStore((s) => s.setCurrentView);
  return (
    <section className="flex flex-col overflow-hidden rounded-[var(--radius-card)] border border-[var(--hairline)] bg-[var(--oxide)]">
      <header className="flex items-center justify-between border-b border-[var(--hairline)] px-4 py-2.5">
        <h2 className="text-[12px] font-medium tracking-[0.06em] text-[var(--text-primary)]">
          Canvas Preview
        </h2>
        <button
          onClick={() => setCurrentView("canvas")}
          title="Open in canvas"
          aria-label="Open in canvas"
          className="rounded p-1 text-[var(--text-muted)] hover:bg-[var(--bg-hover)] hover:text-[var(--text-primary)]"
        >
          <svg
            width="14"
            height="14"
            viewBox="0 0 16 16"
            fill="none"
            stroke="currentColor"
            strokeWidth="1.3"
            strokeLinecap="round"
            strokeLinejoin="round"
          >
            <path d="M9 2h5v5M14 2L8.5 7.5M7 14H2V9M2 14l5.5-5.5" />
          </svg>
        </button>
      </header>
      <div className="min-h-0 flex-1">
        <CanvasPreviewCanvas />
      </div>
    </section>
  );
}
