const inputClass =
  "w-full rounded bg-[var(--bg-input)] px-2.5 py-1.5 text-xs text-[var(--text-primary)] outline-none focus:ring-1 focus:ring-[var(--accent-coral)]";

interface ExecutionTabProps {
  maxRepairAttempts: number;
  supervisionDelayMs: number;
  outcomeDelayMs: number;
  onMaxRepairAttemptsChange: (n: number) => void;
  onSupervisionDelayMsChange: (ms: number) => void;
  onOutcomeDelayMsChange: (ms: number) => void;
}

export function ExecutionTab({
  maxRepairAttempts,
  supervisionDelayMs,
  outcomeDelayMs,
  onMaxRepairAttemptsChange,
  onSupervisionDelayMsChange,
  onOutcomeDelayMsChange,
}: ExecutionTabProps) {
  return (
    <div className="space-y-4 p-4">
      <div>
        <h3 className="mb-2 text-xs font-semibold uppercase tracking-wider text-[var(--text-muted)]">
          Assistant
        </h3>
        <p className="mb-2 text-[10px] text-[var(--text-muted)]">
          Controls how the assistant validates and retries generated patches.
        </p>
        <div>
          <label className="mb-1 block text-xs text-[var(--text-secondary)]">
            Max repair attempts
          </label>
          <input
            type="number"
            min={0}
            max={10}
            value={maxRepairAttempts}
            onChange={(e) => {
              const clamped = Math.max(0, Math.min(10, Math.floor(Number(e.target.value) || 0)));
              onMaxRepairAttemptsChange(clamped);
            }}
            className={inputClass}
          />
          <p className="mt-1 text-[10px] text-[var(--text-muted)]">
            Validate patches and retry on failure. 0 = skip validation, 1 = validate only, 2+ = validate and retry.
          </p>
        </div>
      </div>

      <div>
        <h3 className="mb-2 text-xs font-semibold uppercase tracking-wider text-[var(--text-muted)]">
          Supervision
        </h3>
        <p className="mb-2 text-[10px] text-[var(--text-muted)]">
          Controls per-step verification during workflow execution.
        </p>
        <div>
          <label className="mb-1 block text-xs text-[var(--text-secondary)]">
            Screenshot Delay (ms)
          </label>
          <input
            type="number"
            min={0}
            max={10000}
            step={100}
            value={supervisionDelayMs}
            onChange={(e) => {
              const clamped = Math.max(0, Math.min(10000, Math.floor(Number(e.target.value) || 0)));
              onSupervisionDelayMsChange(clamped);
            }}
            className={inputClass}
          />
          <p className="mt-1 text-[10px] text-[var(--text-muted)]">
            How long to wait before capturing the per-step supervision screenshot, giving the UI time to settle (0-10000ms).
          </p>
        </div>
      </div>

      <div>
        <h3 className="mb-2 text-xs font-semibold uppercase tracking-wider text-[var(--text-muted)]">
          Outcome Verification
        </h3>
        <p className="mb-2 text-[10px] text-[var(--text-muted)]">
          Controls the final verification after a workflow completes.
        </p>
        <div>
          <label className="mb-1 block text-xs text-[var(--text-secondary)]">
            Screenshot Delay (ms)
          </label>
          <input
            type="number"
            min={0}
            max={10000}
            step={100}
            value={outcomeDelayMs}
            onChange={(e) => {
              const clamped = Math.max(0, Math.min(10000, Math.floor(Number(e.target.value) || 0)));
              onOutcomeDelayMsChange(clamped);
            }}
            className={inputClass}
          />
          <p className="mt-1 text-[10px] text-[var(--text-muted)]">
            How long to wait before capturing the outcome verification screenshot (0-10000ms).
          </p>
        </div>
      </div>
    </div>
  );
}
