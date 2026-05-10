import type { SupervisionPause } from "../store/slices/executionSlice";

interface SupervisionModalProps {
  pause: SupervisionPause;
  onRespond: (action: "retry" | "skip" | "abort") => void;
}

export function SupervisionModal({ pause, onRespond }: SupervisionModalProps) {
  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/60">
      <div className="w-[480px] rounded-lg border border-[var(--border)] bg-[var(--bg-panel)] p-5 shadow-2xl">
        <div className="mb-3 flex items-center gap-2">
          <span className="flex h-5 w-5 items-center justify-center rounded-full bg-amber-500/20 text-[10px] text-amber-400">
            !
          </span>
          <h3 className="text-sm font-medium text-[var(--text-primary)]">
            Supervision Check Failed
          </h3>
        </div>

        <div className="mb-1 text-xs text-[var(--text-secondary)]">
          Step:{" "}
          <span className="font-medium text-[var(--text-primary)]">
            {pause.scope.kind === "skill"
              ? pause.scope.step_id
              : pause.scope.run_id}
          </span>
        </div>

        <div className="mb-4 rounded bg-[var(--bg-dark)] px-3 py-2 text-xs leading-relaxed text-[var(--text-secondary)]">
          {pause.finding}
        </div>

        {pause.screenshot && (
          <div className="mb-4">
            <img
              src={`data:image/png;base64,${pause.screenshot}`}
              alt="Verification screenshot"
              className="w-full rounded border border-[var(--border)]"
            />
          </div>
        )}

        <div className="flex justify-end gap-2">
          <button
            onClick={() => onRespond("abort")}
            className="rounded px-3 py-1.5 text-xs text-red-400 hover:bg-red-500/10"
          >
            Abort
          </button>
          <button
            onClick={() => onRespond("skip")}
            className="rounded px-3 py-1.5 text-xs text-[var(--text-secondary)] hover:bg-[var(--bg-hover)]"
          >
            Skip
          </button>
          <button
            onClick={() => onRespond("retry")}
            className="rounded bg-[var(--accent-green)] px-4 py-1.5 text-xs font-medium text-white hover:opacity-90"
          >
            Retry
          </button>
        </div>
      </div>
    </div>
  );
}
