import { useMemo } from "react";
import type { RunTrace } from "../store/slices/assistantSlice";

interface Props {
  trace: RunTrace;
  selectedStepIndex: number | null;
  onClose: () => void;
}

export function TraceSidePanel({ trace, selectedStepIndex, onClose }: Props) {
  const view = useMemo(() => {
    if (selectedStepIndex == null) return null;
    const step = trace.steps.find((s) => s.stepIndex === selectedStepIndex);
    if (!step) return null;
    const milestone = trace.milestones.find(
      (m) => m.stepIndex === selectedStepIndex,
    );
    const delta = trace.worldModelDeltas.find(
      (d) => d.stepIndex === selectedStepIndex,
    );
    const raw = {
      run_id: trace.runId,
      step_index: step.stepIndex,
      tool_name: step.toolName,
      phase: step.phase,
      body: step.body,
      failed: step.failed,
      milestone: milestone
        ? { kind: milestone.kind, text: milestone.text }
        : null,
      world_model_delta: delta ? { changed_fields: delta.changedFields } : null,
    };
    return { step, milestone, delta, raw };
  }, [selectedStepIndex, trace]);

  if (!view) return null;

  return (
    <aside
      className="absolute right-0 top-0 z-20 flex h-full w-[360px] flex-col border-l border-[var(--border)] bg-[var(--bg-panel)] text-xs shadow-lg"
      data-testid="trace-side-panel"
    >
      <header className="flex items-center justify-between border-b border-[var(--border)] px-3 py-2">
        <div className="flex items-center gap-2">
          <span className="font-mono text-[10px] text-[var(--text-muted)]">
            #{view.step.stepIndex}
          </span>
          <span className="font-medium text-[var(--text-primary)]">
            {view.step.toolName}
          </span>
        </div>
        <button
          type="button"
          onClick={onClose}
          aria-label="Close trace panel"
          className="rounded px-2 py-0.5 text-[var(--text-muted)] hover:bg-[var(--bg-hover)] hover:text-[var(--text-primary)]"
        >
          ✕
        </button>
      </header>

      <div className="flex-1 overflow-y-auto px-3 py-2">
        <section>
          <h4 className="mb-1 text-[10px] font-medium uppercase tracking-wide text-[var(--text-muted)]">
            Body
          </h4>
          <p
            className={`break-words rounded border px-2 py-1 ${
              view.step.failed
                ? "border-red-500/40 bg-red-500/10 text-red-200"
                : "border-[var(--border)] bg-[var(--bg-dark)] text-[var(--text-secondary)]"
            }`}
          >
            {view.step.body}
          </p>
        </section>

        {view.delta && view.delta.changedFields.length > 0 && (
          <section className="mt-3">
            <h4 className="mb-1 text-[10px] font-medium uppercase tracking-wide text-[var(--text-muted)]">
              World model changes
            </h4>
            <ul className="rounded border border-[var(--border)] bg-[var(--bg-dark)] px-2 py-1 font-mono text-[11px] text-[var(--text-muted)]">
              {view.delta.changedFields.map((f) => (
                <li key={f}>{f}</li>
              ))}
            </ul>
          </section>
        )}

        {view.milestone && (
          <section className="mt-3">
            <h4 className="mb-1 text-[10px] font-medium uppercase tracking-wide text-[var(--text-muted)]">
              Milestone
            </h4>
            <p className="rounded border border-[var(--accent-blue)]/40 bg-[var(--accent-blue)]/10 px-2 py-1 text-[var(--text-secondary)]">
              <span className="font-medium text-[var(--accent-blue)]">
                {view.milestone.kind === "subgoal_completed"
                  ? "Subgoal completed"
                  : "Recovery succeeded"}
              </span>
              <span className="ml-2">{view.milestone.text}</span>
            </p>
          </section>
        )}

        <section className="mt-3">
          <h4 className="mb-1 text-[10px] font-medium uppercase tracking-wide text-[var(--text-muted)]">
            Raw event
          </h4>
          <pre className="overflow-x-auto rounded border border-[var(--border)] bg-[var(--bg-dark)] px-2 py-1 font-mono text-[10px] text-[var(--text-muted)]">
            {JSON.stringify(view.raw, null, 2)}
          </pre>
        </section>
      </div>
    </aside>
  );
}
