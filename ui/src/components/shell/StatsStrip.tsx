import { useState } from "react";
import { useShallow } from "zustand/react/shallow";
import { useStore } from "../../store/useAppStore";
import type { SkillSummary } from "../../store/slices/skillsSlice";

export function StatsStrip({
  onOpenSkillsManager,
}: {
  onOpenSkillsManager: () => void;
}) {
  const { drafts, confirmed, promoted } = useStore(
    useShallow((s) => ({
      drafts: s.drafts,
      confirmed: s.confirmed,
      promoted: s.promoted,
    })),
  );

  return (
    <div className="flex items-stretch gap-2 px-6 pb-2">
      <Bucket label="Drafts" items={drafts} />
      <Bucket label="Confirmed" items={confirmed} />
      <Bucket label="Promoted" items={promoted} />
      <button
        onClick={onOpenSkillsManager}
        className="ml-auto self-center rounded-full border border-[var(--hairline)] bg-[var(--bloom-coral)] px-3 py-1 text-[11px] text-[var(--accent-coral)] hover:bg-[color-mix(in_srgb,var(--accent-coral)_14%,transparent)]"
      >
        Skills Manager
      </button>
    </div>
  );
}

function Bucket({ label, items }: { label: string; items: SkillSummary[] }) {
  const [expanded, setExpanded] = useState(false);
  const top = items.slice(0, 3);
  const rest = items.slice(3);

  return (
    <div className="cw-stats-chip flex min-w-0 flex-1 flex-col gap-1 rounded-[var(--radius-card)] border border-[var(--hairline)] bg-[var(--oxide)] px-3 py-2">
      <div className="flex items-center justify-between">
        <span className="text-[9px] font-medium uppercase tracking-[0.18em] text-[var(--text-muted)]">
          {label}
        </span>
        <span className="font-mono text-[12px] text-[var(--text-primary)]">
          {items.length}
        </span>
      </div>
      <div className="flex flex-wrap gap-1">
        {top.map((s) => (
          <span
            key={`${s.id}-${s.version}`}
            className="truncate rounded-full border border-[var(--hairline)] bg-[var(--bloom-coral)] px-2 py-0.5 text-[10px] text-[var(--text-primary)]"
            title={s.description}
          >
            {s.name}
          </span>
        ))}
        {rest.length > 0 && !expanded && (
          <button
            onClick={() => setExpanded(true)}
            className="rounded-full px-2 py-0.5 text-[10px] text-[var(--text-muted)] hover:text-[var(--text-primary)]"
            aria-label={`Expand ${label}`}
          >
            + {rest.length} more
          </button>
        )}
      </div>
      {expanded && rest.length > 0 && (
        <ul className="mt-1 max-h-32 overflow-y-auto text-[10px] text-[var(--text-secondary)]">
          {rest.map((s) => (
            <li key={`${s.id}-${s.version}`} className="truncate py-0.5">
              {s.name}
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}
