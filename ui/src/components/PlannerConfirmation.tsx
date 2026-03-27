import { useStore } from "../store/useAppStore";

export function PlannerConfirmation() {
  const confirmation = useStore((s) => s.plannerConfirmation);
  const respond = useStore((s) => s.respondToPlannerConfirmation);

  if (!confirmation) return null;

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/60">
      <div className="w-[420px] rounded-lg border border-[var(--border)] bg-[var(--bg-panel)] p-5 shadow-2xl">
        <div className="mb-3 flex items-center gap-2">
          <span className="flex h-5 w-5 items-center justify-center rounded-full bg-blue-500/20 text-[10px] text-blue-400">
            ?
          </span>
          <h3 className="text-sm font-medium text-[var(--text-primary)]">
            Planning: Confirm Action
          </h3>
        </div>

        <div className="mb-1 text-xs text-[var(--text-secondary)]">
          Tool: <span className="font-mono text-[var(--text-primary)]">{confirmation.toolName}</span>
        </div>

        <div className="mb-4 rounded bg-[var(--bg-dark)] px-3 py-2 text-xs leading-relaxed text-[var(--text-secondary)]">
          {confirmation.message}
        </div>

        <div className="flex justify-end gap-2">
          <button
            onClick={() => respond(false)}
            className="rounded px-3 py-1.5 text-xs text-[var(--text-secondary)] hover:bg-[var(--bg-dark)] hover:text-[var(--text-primary)] transition-colors"
          >
            Decline
          </button>
          <button
            onClick={() => respond(true)}
            className="rounded bg-blue-600 px-3 py-1.5 text-xs text-white hover:bg-blue-500 transition-colors"
          >
            Allow
          </button>
        </div>
      </div>
    </div>
  );
}
