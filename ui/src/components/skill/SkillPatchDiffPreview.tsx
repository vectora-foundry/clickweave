/**
 * `SkillPatchDiffPreview` — read-only four-pane diff preview for a pending
 * `SkillPatch` before the user confirms or cancels the change.
 *
 * Panes:
 *   1. Markdown diff (SKILL.md body prose)
 *   2. action_sketch JSON diff
 *   3. variables diff
 *   4. replay.json sidecar diff
 *
 * The component is purely presentational — it receives the before/after text
 * for each layer and a pair of `onConfirm` / `onCancel` callbacks. The caller
 * is responsible for calling `apply_skill_patch` on confirm.
 */

import { useMemo, type ReactElement } from "react";
import * as Diff from "diff";
import type { Change } from "diff";

// ── Diff types ────────────────────────────────────────────────────────────

export interface SkillPatchDiffInput {
  /** Before/after SKILL.md prose body (excluding fenced action_sketch block). */
  markdownBefore: string;
  markdownAfter: string;
  /** Before/after action_sketch JSON (pretty-printed). */
  actionSketchBefore: string;
  actionSketchAfter: string;
  /** Before/after variables JSON (pretty-printed list). */
  variablesBefore: string;
  variablesAfter: string;
  /** Before/after replay.json (pretty-printed), or empty string when absent. */
  replayBefore: string;
  replayAfter: string;
}

// ── Internal diff renderer ────────────────────────────────────────────────

function renderLineDiff(changes: Change[]): ReactElement {
  return (
    <pre className="overflow-auto rounded bg-[var(--bg-dark)] p-2 text-xs leading-5">
      {changes.map((change, i) => {
        const cls = change.added
          ? "bg-green-900/40 text-green-300"
          : change.removed
            ? "bg-red-900/40 text-red-300"
            : "text-[var(--text-secondary)]";
        const prefix = change.added ? "+" : change.removed ? "-" : " ";
        const lines = change.value.split("\n");
        // splitLines includes a trailing empty string from the final \n.
        const significant = lines.at(-1) === "" ? lines.slice(0, -1) : lines;
        return (
          <span key={i} className={cls}>
            {significant.map((line, j) => (
              <span key={j} className="block">
                {prefix}
                {line}
              </span>
            ))}
          </span>
        );
      })}
    </pre>
  );
}

// ── Pane component ────────────────────────────────────────────────────────

interface DiffPaneProps {
  title: string;
  before: string;
  after: string;
}

function DiffPane({ title, before, after }: DiffPaneProps) {
  const changes = useMemo(() => Diff.diffLines(before, after), [before, after]);
  const hasChanges = changes.some((c) => c.added || c.removed);

  return (
    <div className="flex flex-col gap-1">
      <div className="flex items-center gap-2">
        <span className="text-xs font-semibold text-[var(--text-secondary)] uppercase tracking-wide">
          {title}
        </span>
        {!hasChanges && (
          <span className="text-xs text-[var(--text-muted)]">(no changes)</span>
        )}
      </div>
      {hasChanges ? (
        renderLineDiff(changes)
      ) : (
        <pre className="overflow-auto rounded bg-[var(--bg-dark)] p-2 text-xs text-[var(--text-muted)]">
          {before || "(empty)"}
        </pre>
      )}
    </div>
  );
}

// ── Main component ────────────────────────────────────────────────────────

export interface SkillPatchDiffPreviewProps {
  diff: SkillPatchDiffInput;
  /** Called when the user clicks Confirm (caller dispatches apply_skill_patch). */
  onConfirm: () => void;
  /** Called when the user clicks Cancel (no changes applied). */
  onCancel: () => void;
  /** When true, disables the Confirm button and shows a loading indicator. */
  confirming?: boolean;
  /** Error from the last apply attempt, shown above the buttons. */
  applyError?: string | null;
}

export function SkillPatchDiffPreview({
  diff,
  onConfirm,
  onCancel,
  confirming = false,
  applyError = null,
}: SkillPatchDiffPreviewProps) {
  return (
    <div className="flex h-full flex-col gap-4 overflow-auto p-4">
      {/* Header */}
      <div className="shrink-0">
        <h3 className="text-sm font-semibold text-[var(--text-primary)]">
          Review skill patch
        </h3>
        <p className="mt-0.5 text-xs text-[var(--text-secondary)]">
          Confirm to apply all changes atomically, or Cancel to discard.
        </p>
      </div>

      {/* Four diff panes */}
      <div className="flex flex-1 flex-col gap-4 overflow-auto">
        <DiffPane
          title="Markdown (prose body)"
          before={diff.markdownBefore}
          after={diff.markdownAfter}
        />
        <DiffPane
          title="action_sketch (JSON)"
          before={diff.actionSketchBefore}
          after={diff.actionSketchAfter}
        />
        <DiffPane
          title="variables"
          before={diff.variablesBefore}
          after={diff.variablesAfter}
        />
        <DiffPane
          title="replay.json (sidecar)"
          before={diff.replayBefore}
          after={diff.replayAfter}
        />
      </div>

      {/* Error banner */}
      {applyError && (
        <div className="shrink-0 rounded border border-red-700/50 bg-red-900/20 px-3 py-2 text-xs text-red-400">
          {applyError}
        </div>
      )}

      {/* Action buttons */}
      <div className="shrink-0 flex justify-end gap-2">
        <button
          type="button"
          onClick={onCancel}
          disabled={confirming}
          className="rounded border border-[var(--border)] px-3 py-1.5 text-xs text-[var(--text-secondary)] hover:bg-[var(--bg-hover)] disabled:opacity-50"
        >
          Cancel
        </button>
        <button
          type="button"
          onClick={onConfirm}
          disabled={confirming}
          className="rounded bg-blue-600 px-3 py-1.5 text-xs font-medium text-white hover:bg-blue-700 disabled:opacity-50"
        >
          {confirming ? "Applying…" : "Confirm"}
        </button>
      </div>
    </div>
  );
}
