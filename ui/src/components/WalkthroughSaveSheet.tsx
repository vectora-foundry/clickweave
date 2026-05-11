import { useState } from "react";
import { commands } from "../bindings";
import { useStore } from "../store/useAppStore";
import { errorMessage } from "../utils/commandError";
import { OnboardingTipForRecorder } from "./OnboardingTipForRecorder";

interface WalkthroughSaveSheetProps {
  sessionId: string;
  onSaved: () => void;
  onDiscard: () => void;
}

/**
 * Small overlay anchored to the recording-stop affordance.
 * Shows a name field + Save / Cancel buttons. On Save, calls
 * `save_walkthrough_as_skill` and emits the onSaved callback.
 * On Cancel / Discard, calls onDiscard.
 */
export function WalkthroughSaveSheet({
  sessionId,
  onSaved,
  onDiscard,
}: WalkthroughSaveSheetProps) {
  const [name, setName] = useState("");
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [showTip, setShowTip] = useState(false);

  const { projectId, projectName, projectPath, storeTraces, skillsEnabled } = useStore((s) => ({
    projectId: s.projectId,
    projectName: s.projectName,
    projectPath: s.projectPath,
    storeTraces: s.storeTraces,
    skillsEnabled: s.skillsEnabled,
  }));

  const refused = !storeTraces || !skillsEnabled;

  const handleSave = async () => {
    if (refused) return;
    setSaving(true);
    setError(null);
    try {
      const result = await commands.saveWalkthroughAsSkill({
        session_id: sessionId,
        project_path: projectPath ?? null,
        project_name: projectName,
        project_id: projectId,
        name: name.trim() || "Recorded Walkthrough",
        store_traces: storeTraces,
      });
      if (result.status === "error") {
        setError(errorMessage(result.error));
        return;
      }
      setShowTip(true);
    } catch (err) {
      setError(String(err));
    } finally {
      setSaving(false);
    }
  };

  const handleKeyDown = (e: React.KeyboardEvent) => {
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      handleSave();
    }
    if (e.key === "Escape") {
      onDiscard();
    }
  };

  if (showTip) {
    return (
      <OnboardingTipForRecorder
        onDismiss={() => {
          setShowTip(false);
          onSaved();
        }}
      />
    );
  }

  return (
    <div className="fixed inset-0 z-50 flex items-end justify-center pb-8 px-4">
      <div
        className="w-full max-w-sm rounded-xl border border-[var(--border)] bg-[var(--bg-panel)] p-4 shadow-2xl"
        onKeyDown={handleKeyDown}
      >
        <h2 className="text-sm font-semibold text-[var(--text-primary)] mb-3">
          Save as skill
        </h2>

        {refused && (
          <p className="mb-3 rounded-lg bg-[var(--bg-hover)] px-3 py-2 text-xs text-[var(--text-muted)]">
            {!storeTraces
              ? "Skill saving is disabled while trace persistence is off. Enable it in Privacy settings."
              : "Skill saving is disabled in settings."}
          </p>
        )}

        <div className="mb-3">
          <input
            type="text"
            value={name}
            onChange={(e) => setName(e.target.value)}
            placeholder="Skill name (optional)"
            autoFocus
            disabled={saving || refused}
            className="w-full rounded-lg border border-[var(--border)] bg-[var(--bg-input)] px-3 py-2 text-sm text-[var(--text-primary)] placeholder:text-[var(--text-muted)] outline-none focus:border-[var(--accent-coral)] disabled:opacity-50"
          />
        </div>

        {error && (
          <p className="mb-3 text-xs text-red-400">{error}</p>
        )}

        <div className="flex items-center justify-end gap-2">
          <button
            onClick={onDiscard}
            disabled={saving}
            className="rounded-lg px-3 py-1.5 text-xs text-[var(--text-secondary)] hover:bg-[var(--bg-hover)] hover:text-[var(--text-primary)] disabled:opacity-50"
          >
            Discard
          </button>
          <button
            onClick={handleSave}
            disabled={saving || refused}
            className="rounded-lg bg-[var(--accent-coral)] px-4 py-1.5 text-xs font-medium text-white hover:opacity-90 disabled:opacity-50"
          >
            {saving ? "Saving…" : "Save"}
          </button>
        </div>
      </div>
    </div>
  );
}
