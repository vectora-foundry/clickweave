const inputClass =
  "w-full rounded bg-[var(--bg-input)] px-2.5 py-1.5 text-xs text-[var(--text-primary)] outline-none focus:ring-1 focus:ring-[var(--accent-coral)]";

interface ExecutionTabProps {
  maxRepairAttempts: number;
  supervisionDelayMs: number;
  episodicEnabled: boolean;
  retrievedEpisodesK: number;
  episodicGlobalParticipation: boolean;
  onMaxRepairAttemptsChange: (n: number) => void;
  onSupervisionDelayMsChange: (ms: number) => void;
  onEpisodicEnabledChange: (enabled: boolean) => void;
  onRetrievedEpisodesKChange: (n: number) => void;
  onEpisodicGlobalParticipationChange: (enabled: boolean) => void;
}

export function ExecutionTab({
  maxRepairAttempts,
  supervisionDelayMs,
  episodicEnabled,
  retrievedEpisodesK,
  episodicGlobalParticipation,
  onMaxRepairAttemptsChange,
  onSupervisionDelayMsChange,
  onEpisodicEnabledChange,
  onRetrievedEpisodesKChange,
  onEpisodicGlobalParticipationChange,
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
          Agent Memory
        </h3>
        <p className="mb-2 text-[10px] text-[var(--text-muted)]">
          Episodic memory lets the agent recall how it recovered from similar stuck states in past runs.
        </p>

        <div className="mb-3">
          <label className="flex items-center gap-2 text-xs text-[var(--text-secondary)]">
            <input
              type="checkbox"
              checked={episodicEnabled}
              onChange={(e) => onEpisodicEnabledChange(e.target.checked)}
              className="accent-[var(--accent-coral)]"
            />
            Enable episodic memory
          </label>
        </div>

        <div className="mb-3">
          <label className="mb-1 block text-xs text-[var(--text-secondary)]">
            Memory depth (episodes to retrieve per trigger)
          </label>
          <input
            type="number"
            min={1}
            max={10}
            value={retrievedEpisodesK}
            onChange={(e) => {
              const n = Number(e.target.value);
              const clamped = Number.isFinite(n)
                ? Math.max(1, Math.min(10, Math.floor(n)))
                : 2;
              onRetrievedEpisodesKChange(clamped);
            }}
            disabled={!episodicEnabled}
            className={inputClass}
          />
        </div>

        <div className="mb-3">
          <label className="flex items-center gap-2 text-xs text-[var(--text-secondary)]">
            <input
              type="checkbox"
              checked={episodicGlobalParticipation}
              onChange={(e) =>
                onEpisodicGlobalParticipationChange(e.target.checked)
              }
              disabled={!episodicEnabled}
              className="accent-[var(--accent-coral)]"
            />
            Share recoveries across workflows
          </label>
          <p className="ml-5 text-[10px] text-[var(--text-muted)]">
            When on, recovery episodes from one workflow can be surfaced in another.
            Default off keeps workflows isolated.
          </p>
        </div>
      </div>
    </div>
  );
}
