/**
 * One-time onboarding tip shown after the first recording is saved as a skill.
 * Explains to the user that they can review by clicking sections and editing in chat.
 * Dismissible; per-project flag tracked in the parent (walkthroughSaveSheetOpen cleared on dismiss).
 */
interface OnboardingTipForRecorderProps {
  onDismiss: () => void;
}

export function OnboardingTipForRecorder({ onDismiss }: OnboardingTipForRecorderProps) {
  return (
    <div className="fixed inset-0 z-50 flex items-end justify-center pb-8 px-4">
      <div className="w-full max-w-sm rounded-xl border border-[var(--border)] bg-[var(--bg-panel)] p-4 shadow-2xl">
        <div className="flex items-start gap-3">
          <div className="flex-shrink-0 mt-0.5 h-5 w-5 rounded-full bg-[var(--accent-coral)] flex items-center justify-center">
            <span className="text-[10px] font-bold text-white">i</span>
          </div>
          <div className="flex-1">
            <p className="text-sm font-medium text-[var(--text-primary)] mb-1">
              Skill saved
            </p>
            <p className="text-xs text-[var(--text-muted)] leading-relaxed">
              Recording done — review by clicking sections in the skill view
              and editing steps in chat. The mechanical draft is a starting
              point; use the chat to rename steps, add conditions, or remove
              unwanted actions.
            </p>
          </div>
        </div>
        <div className="mt-3 flex justify-end">
          <button
            onClick={onDismiss}
            className="rounded-lg bg-[var(--accent-coral)] px-4 py-1.5 text-xs font-medium text-white hover:opacity-90"
          >
            Got it
          </button>
        </div>
      </div>
    </div>
  );
}
