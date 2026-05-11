import { useState } from "react";
import { useStore } from "../store/useAppStore";

interface AgentRunSaveSheetProps {
  defaultName: string;
  onSaved: () => void;
  onDiscard: () => void;
}

/**
 * Post-run review sheet. Mounted by `AppShell` when `pendingRunSave` is set
 * (i.e. an `IntentEmptyState` "Run & save as skill" run has just terminated).
 *
 * Lets the user edit the skill name and uncheck steps that should not become
 * part of the saved skill — typically wrong-path tool calls from mid-run
 * corrections. On Save, calls `saveRunAsSkill(name, selectedIndices)`. On
 * Discard, drops the run buffer without materialising a skill.
 */
export function AgentRunSaveSheet({
  defaultName,
  onSaved,
  onDiscard,
}: AgentRunSaveSheetProps) {
  const agentSteps = useStore((s) => s.agentSteps);
  const storeTraces = useStore((s) => s.storeTraces);
  const skillsEnabled = useStore((s) => s.skillsEnabled);
  const saveRunAsSkill = useStore((s) => s.saveRunAsSkill);

  const [name, setName] = useState(defaultName);
  const [included, setIncluded] = useState<boolean[]>(
    () => agentSteps.map(() => true),
  );
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const refused = !storeTraces || !skillsEnabled;
  const includedCount = included.filter(Boolean).length;

  const toggle = (i: number) => {
    setIncluded((prev) => {
      const next = prev.slice();
      next[i] = !next[i];
      return next;
    });
  };

  const handleSave = async () => {
    if (refused || includedCount === 0) return;
    setSaving(true);
    setError(null);
    try {
      const indices = included
        .map((on, i) => (on ? i : -1))
        .filter((i) => i >= 0);
      const result = await saveRunAsSkill(
        name.trim() || defaultName || "Untitled skill",
        indices,
      );
      if (!result.ok) {
        setError(result.error);
        return;
      }
      onSaved();
    } catch (err) {
      setError(String(err));
    } finally {
      setSaving(false);
    }
  };

  const handleKeyDown = (e: React.KeyboardEvent) => {
    // Mirror the disabled Discard button: while a save is in flight we
    // must keep the sheet mounted so the user sees the result (and can
    // retry on `{ ok: false }`). Otherwise an Escape mid-save unmounts
    // the modal and silently drops the staged pendingRunSave.
    if (e.key === "Escape" && !saving) onDiscard();
  };

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/40 px-4">
      <div
        className="w-full max-w-lg rounded-xl border border-[var(--border)] bg-[var(--bg-panel)] p-5 shadow-2xl"
        onKeyDown={handleKeyDown}
      >
        <h2 className="mb-1 text-sm font-semibold text-[var(--text-primary)]">
          Save run as skill
        </h2>
        <p className="mb-4 text-xs text-[var(--text-muted)]">
          Review the steps below and uncheck any that don't belong in the
          reusable skill.
        </p>

        {refused && (
          <p className="mb-3 rounded-lg bg-[var(--bg-hover)] px-3 py-2 text-xs text-[var(--text-muted)]">
            {!storeTraces
              ? "Skill saving is disabled while trace persistence is off. Enable it in Privacy settings."
              : "Skill saving is disabled in settings."}
          </p>
        )}

        <label className="mb-3 block">
          <span className="mb-1 block text-[11px] uppercase tracking-wide text-[var(--text-muted)]">
            Skill name
          </span>
          <input
            type="text"
            value={name}
            onChange={(e) => setName(e.target.value)}
            placeholder="Skill name"
            autoFocus
            disabled={saving || refused}
            className="w-full rounded-lg border border-[var(--border)] bg-[var(--bg-input)] px-3 py-2 text-sm text-[var(--text-primary)] placeholder:text-[var(--text-muted)] outline-none focus:border-[var(--accent-coral)] disabled:opacity-50"
          />
        </label>

        <div className="mb-4">
          <div className="mb-1 flex items-baseline justify-between">
            <span className="text-[11px] uppercase tracking-wide text-[var(--text-muted)]">
              Steps to include
            </span>
            <span className="text-[11px] text-[var(--text-muted)]">
              {includedCount} of {agentSteps.length} selected
            </span>
          </div>
          <ul className="max-h-64 overflow-y-auto rounded-lg border border-[var(--border)] bg-[var(--bg-input)]">
            {agentSteps.length === 0 ? (
              <li className="px-3 py-2 text-xs italic text-[var(--text-muted)]">
                No steps recorded.
              </li>
            ) : (
              agentSteps.map((step, i) => (
                <li
                  key={i}
                  className="flex items-start gap-2 border-b border-[var(--border)] px-3 py-2 last:border-b-0"
                >
                  <input
                    type="checkbox"
                    checked={included[i]}
                    onChange={() => toggle(i)}
                    disabled={saving || refused}
                    className="mt-0.5 shrink-0 accent-[var(--accent-coral)]"
                    aria-label={`Include step ${i + 1}`}
                  />
                  <div className="min-w-0 flex-1">
                    <div className="truncate text-xs font-medium text-[var(--text-primary)]">
                      {step.toolName}
                    </div>
                    {step.summary && (
                      <div className="truncate text-[11px] text-[var(--text-secondary)]">
                        {step.summary}
                      </div>
                    )}
                  </div>
                </li>
              ))
            )}
          </ul>
        </div>

        {error && <p className="mb-3 text-xs text-red-400">{error}</p>}

        <div className="flex items-center justify-end gap-2">
          <button
            type="button"
            onClick={onDiscard}
            disabled={saving}
            className="rounded-lg px-3 py-1.5 text-xs text-[var(--text-secondary)] hover:bg-[var(--bg-hover)] hover:text-[var(--text-primary)] disabled:opacity-50"
          >
            Discard
          </button>
          <button
            type="button"
            onClick={handleSave}
            disabled={saving || refused || includedCount === 0}
            className="rounded-lg bg-[var(--accent-coral)] px-4 py-1.5 text-xs font-medium text-white hover:opacity-90 disabled:opacity-50"
          >
            {saving ? "Saving…" : "Save skill"}
          </button>
        </div>
      </div>
    </div>
  );
}
