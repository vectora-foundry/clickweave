const inputClass =
  "bg-[var(--bg-input)] text-[var(--text-primary)] border border-[var(--border)] rounded-md px-2.5 py-1 text-[11px]";

const settingRowClass =
  "flex items-center justify-between gap-3 rounded-lg bg-[var(--bg-dark)] px-3.5 py-2.5";

interface SettingRowProps {
  title: string;
  description: string;
  control: React.ReactNode;
}

function SettingRow({ title, description, control }: SettingRowProps) {
  return (
    <div className={settingRowClass}>
      <div>
        <div className="text-xs font-semibold text-[var(--text-primary)]">
          {title}
        </div>
        <div className="mt-0.5 text-[10px] text-[var(--text-muted)]">
          {description}
        </div>
      </div>
      {control}
    </div>
  );
}

interface ToggleProps {
  checked: boolean;
  onChange: (next: boolean) => void;
}

function Toggle({ checked, onChange }: ToggleProps) {
  return (
    <button
      role="switch"
      aria-checked={checked}
      onClick={() => onChange(!checked)}
      className={`relative h-[22px] w-10 flex-shrink-0 rounded-full transition-colors ${
        checked ? "bg-[var(--accent-coral)]" : "bg-[var(--bg-input)]"
      }`}
    >
      <span
        className={`absolute top-[3px] h-4 w-4 rounded-full bg-white transition-[left] ${
          checked ? "left-[21px]" : "left-[3px]"
        }`}
      />
    </button>
  );
}

interface PrivacyTabProps {
  traceRetentionDays: number;
  storeTraces: boolean;
  onTraceRetentionDaysChange: (days: number) => void;
  onStoreTracesChange: (enabled: boolean) => void;
}

/** Clamp a user-typed retention value to the allowed range (0..3650 days). */
function clampRetention(raw: unknown): number {
  const n = Number(raw);
  if (!Number.isFinite(n)) return 30;
  return Math.max(0, Math.min(3650, Math.floor(n)));
}

export function PrivacyTab({
  traceRetentionDays,
  storeTraces,
  onTraceRetentionDaysChange,
  onStoreTracesChange,
}: PrivacyTabProps) {
  return (
    <div className="space-y-4 p-4">
      <SettingRow
        title="Store run traces"
        description="Persist agent and workflow run traces to disk. When off, runs execute entirely in memory and nothing is written under the runs directory for this session."
        control={
          <Toggle checked={storeTraces} onChange={onStoreTracesChange} />
        }
      />

      <SettingRow
        title="Trace retention (days)"
        description="Delete run traces older than this many days at app startup. 0 disables cleanup and keeps all traces indefinitely."
        control={
          <input
            type="number"
            min={0}
            max={3650}
            value={traceRetentionDays}
            onChange={(e) =>
              onTraceRetentionDaysChange(clampRetention(e.target.value))
            }
            aria-label="Trace retention in days"
            className={`${inputClass} w-20 text-center`}
          />
        }
      />
    </div>
  );
}
