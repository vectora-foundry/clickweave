/**
 * `SkillSectionCard` — per-section card rendered by `SkillView`.
 *
 * Per D23, each card shows:
 * - Heading title (## level)
 * - One-line summary (first paragraph of section body, derived from heading)
 * - Fidelity dot (Phase 1: always no-data)
 * - Step count badge
 * - Run-state badge when applicable
 * - Hover-revealed "Edit with assistant" affordance
 * - Expandable ### sub-rows for level === 3 sections
 * - "Open raw markdown" affordance (shows modal)
 * - `SkillSectionApprovalOverlay` when the section has a pending approval
 */

import { useState } from "react";
import type { SkillSection } from "../../bindings";
import { SkillFidelityDot } from "./SkillFidelityDot";
import { SkillSectionApprovalOverlay } from "./SkillSectionApprovalOverlay";
import { useStore } from "../../store/useAppStore";
import type { SectionRunStatus } from "../../store/slices/skillsSlice";

interface SkillSectionCardProps {
  section: SkillSection;
  /** Raw markdown body of the full skill, sliced to this section's body_range. */
  sectionBody: string;
  /** Whether this card is within a selected range. */
  selected: boolean;
  onClick: (e: React.MouseEvent) => void;
  /** Optional override status for the resume button in failure handoff. */
  onResume?: (sectionId: string) => void;
}

/** Badge colors and labels for each run state. */
const RUN_STATE_BADGE: Record<SectionRunStatus, { label: string; className: string }> = {
  pending:   { label: "pending",   className: "bg-[var(--bg-dark)] text-[var(--text-muted)]" },
  running:   { label: "running…",  className: "bg-blue-500/20 text-blue-300 animate-pulse" },
  succeeded: { label: "succeeded", className: "bg-emerald-500/20 text-emerald-300" },
  repaired:  { label: "repaired",  className: "bg-yellow-500/20 text-yellow-300" },
  failed:    { label: "failed",    className: "bg-red-500/20 text-red-400" },
  skipped:   { label: "skipped",   className: "bg-[var(--bg-dark)] text-[var(--text-muted)] line-through" },
};

export function SkillSectionCard({
  section,
  sectionBody,
  selected,
  onClick,
  onResume,
}: SkillSectionCardProps) {
  const [expanded, setExpanded] = useState(false);
  const [rawOpen, setRawOpen] = useState(false);
  const [hovered, setHovered] = useState(false);

  const sectionApproval = useStore((s) => s.sectionApproval);
  const runStatus: SectionRunStatus | undefined = useStore(
    (s) => s.sectionRunState[section.id],
  );
  const hasApproval =
    sectionApproval !== null &&
    sectionApproval.scope.section_id === section.id;

  const isFailed = runStatus === "failed";

  const firstLine = sectionBody.trim().split("\n")[0] ?? "";
  const summary = firstLine.replace(/^#+\s*/, "").trim();

  const isSubSection = section.level >= 3;
  const stepCount = section.step_ids.length;

  return (
    <div
      className={`group relative rounded border transition-colors ${
        isFailed
          ? "border-red-500/60 bg-red-500/5"
          : selected
            ? "border-[var(--accent-coral)] bg-[var(--bg-input)]"
            : "border-[var(--border)] bg-[var(--bg-panel)] hover:border-[var(--border-hover,var(--border))] hover:bg-[var(--bg-input)]"
      } ${isSubSection ? "ml-4" : ""}`}
      onClick={onClick}
      onMouseEnter={() => setHovered(true)}
      onMouseLeave={() => setHovered(false)}
      data-run-status={runStatus}
    >
      <div className="flex items-start gap-2 px-3 py-2">
        {/* Fidelity dot */}
        <span className="mt-0.5 shrink-0">
          <SkillFidelityDot />
        </span>

        {/* Main content */}
        <div className="min-w-0 flex-1">
          <div className="flex items-center gap-1.5">
            {/* Heading */}
            <span className="truncate text-xs font-medium text-[var(--text-primary)]">
              {section.heading}
            </span>

            {/* Step count badge */}
            {stepCount > 0 && (
              <span className="shrink-0 rounded bg-[var(--bg-dark)] px-1.5 py-0.5 text-[10px] text-[var(--text-muted)]">
                {stepCount} {stepCount === 1 ? "step" : "steps"}
              </span>
            )}

            {/* Run state badge */}
            {runStatus && runStatus !== "pending" && (
              <span
                data-testid="run-state-badge"
                className={`shrink-0 rounded px-1.5 py-0.5 text-[10px] font-medium ${RUN_STATE_BADGE[runStatus].className}`}
              >
                {RUN_STATE_BADGE[runStatus].label}
              </span>
            )}
          </div>

          {/* One-line summary */}
          {summary && summary !== section.heading && (
            <p className="mt-0.5 truncate text-[11px] text-[var(--text-secondary)]">
              {summary}
            </p>
          )}
        </div>

        {/* Right side: expand / raw affordances */}
        <div className="flex shrink-0 items-center gap-1">
          {/* Resume from failure button — shown on failed sections */}
          {isFailed && onResume && (
            <button
              type="button"
              data-testid="resume-from-failure"
              className="rounded px-1.5 py-0.5 text-[10px] font-medium text-red-300 border border-red-500/40 hover:bg-red-500/10"
              onClick={(e) => {
                e.stopPropagation();
                onResume(section.id);
              }}
            >
              Resume
            </button>
          )}

          {/* Hover-revealed "Edit with assistant" button */}
          {hovered && !isFailed && (
            <button
              type="button"
              data-testid="edit-with-assistant"
              className="rounded px-1.5 py-0.5 text-[10px] text-[var(--text-muted)] opacity-0 transition-opacity group-hover:opacity-100 hover:bg-[var(--bg-dark)] hover:text-[var(--text-primary)]"
              onClick={(e) => {
                e.stopPropagation();
                // Phase 2: wire to assistant
              }}
            >
              Edit
            </button>
          )}

          {/* "Open raw markdown" affordance */}
          <button
            type="button"
            data-testid="open-raw-markdown"
            aria-label="Open raw markdown"
            className="rounded p-0.5 text-[var(--text-muted)] hover:bg-[var(--bg-dark)] hover:text-[var(--text-primary)]"
            onClick={(e) => {
              e.stopPropagation();
              setRawOpen(true);
            }}
          >
            <svg
              width="12"
              height="12"
              viewBox="0 0 16 16"
              fill="currentColor"
            >
              <path d="M1 2.5A1.5 1.5 0 012.5 1h11A1.5 1.5 0 0115 2.5v11a1.5 1.5 0 01-1.5 1.5h-11A1.5 1.5 0 011 13.5v-11zM2.5 2a.5.5 0 00-.5.5v11a.5.5 0 00.5.5h11a.5.5 0 00.5-.5v-11a.5.5 0 00-.5-.5h-11zM5 6.5a.5.5 0 01.5-.5h5a.5.5 0 010 1h-5a.5.5 0 01-.5-.5zm0 3a.5.5 0 01.5-.5h2a.5.5 0 010 1h-2a.5.5 0 01-.5-.5z" />
            </svg>
          </button>

          {/* Expand toggle for sub-sections */}
          {isSubSection && stepCount > 0 && (
            <button
              type="button"
              data-testid="expand-toggle"
              aria-label={expanded ? "Collapse" : "Expand"}
              className="rounded p-0.5 text-[var(--text-muted)] hover:bg-[var(--bg-dark)]"
              onClick={(e) => {
                e.stopPropagation();
                setExpanded((v) => !v);
              }}
            >
              <svg
                width="10"
                height="10"
                viewBox="0 0 10 10"
                fill="currentColor"
                className={`transition-transform ${expanded ? "rotate-180" : ""}`}
              >
                <path d="M5 6.5L1 2.5h8L5 6.5z" />
              </svg>
            </button>
          )}
        </div>
      </div>

      {/* Expanded step IDs */}
      {expanded && stepCount > 0 && (
        <div className="border-t border-[var(--border)] px-3 py-1.5">
          {section.step_ids.map((stepId) => (
            <div
              key={stepId}
              className="py-0.5 text-[11px] text-[var(--text-secondary)]"
            >
              {stepId}
            </div>
          ))}
        </div>
      )}

      {/* Inline approval overlay */}
      {hasApproval && sectionApproval && (
        <SkillSectionApprovalOverlay approval={sectionApproval} />
      )}

      {/* Raw markdown modal */}
      {rawOpen && (
        <RawMarkdownModal
          heading={section.heading}
          body={sectionBody}
          onClose={() => setRawOpen(false)}
        />
      )}
    </div>
  );
}

interface RawMarkdownModalProps {
  heading: string;
  body: string;
  onClose: () => void;
}

function RawMarkdownModal({ heading, body, onClose }: RawMarkdownModalProps) {
  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center bg-black/50"
      onClick={onClose}
    >
      <div
        className="mx-4 max-h-[80vh] w-full max-w-2xl overflow-auto rounded-lg border border-[var(--border)] bg-[var(--bg-panel)] p-4"
        onClick={(e) => e.stopPropagation()}
      >
        <div className="mb-2 flex items-center justify-between">
          <h3 className="text-sm font-medium text-[var(--text-primary)]">
            {heading}
          </h3>
          <button
            type="button"
            onClick={onClose}
            className="text-[var(--text-muted)] hover:text-[var(--text-primary)]"
          >
            ✕
          </button>
        </div>
        <pre className="overflow-x-auto text-xs text-[var(--text-secondary)] whitespace-pre-wrap">
          {body || "(empty)"}
        </pre>
      </div>
    </div>
  );
}
