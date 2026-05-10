import { useShallow } from "zustand/react/shallow";
import { useStore } from "../../store/useAppStore";
import type { SkillSummary } from "../../store/slices/skillsSlice";

interface SkillsPanelProps {
  onNewFromWalkthrough?: () => void;
}

/// Skills panel left rail. Three collapsible sections (Drafts /
/// Confirmed / Promoted) with `name + version` listing; a footer
/// button kicks off "Save as Skill" flow (wired in Phase 7). Click
/// selects a skill and loads its full shape via `loadSelectedSkill`.
export function SkillsPanel({ onNewFromWalkthrough }: SkillsPanelProps) {
  const { drafts, confirmed, promoted, selectedSkill } = useStore(
    useShallow((s) => ({
      drafts: s.drafts,
      confirmed: s.confirmed,
      promoted: s.promoted,
      selectedSkill: s.selectedSkill,
    })),
  );
  const setSelectedSkill = useStore((s) => s.setSelectedSkill);
  const loadSelectedSkill = useStore((s) => s.loadSelectedSkill);
  const { projectPath, projectId, projectName, storeTraces } = useStore(
    useShallow((s) => ({
      projectPath: s.projectPath,
      projectId: s.projectId,
      projectName: s.projectName,
      storeTraces: s.storeTraces,
    })),
  );

  const handleSelect = (id: string, version: number) => {
    // Set a lightweight stub immediately so SkillDetailView renders without
    // waiting for the IPC round-trip. The full Skill shape is fetched in the
    // background via loadSelectedSkill for SkillView.
    setSelectedSkill(id, version);
    loadSelectedSkill({
      projectPath,
      projectId,
      projectName,
      storeTraces,
      includeGlobal: false,
      skill_id: id,
      version,
    }).catch((e) => console.error("Failed to load skill", e));
  };

  return (
    <div className="flex h-full w-56 shrink-0 flex-col border-r border-[var(--border)] bg-[var(--bg-panel)] text-xs">
      <div className="flex-1 overflow-y-auto p-2">
        <SkillsBucket
          title="Drafts"
          skills={drafts}
          selected={selectedSkill}
          onSelect={handleSelect}
        />
        <SkillsBucket
          title="Confirmed"
          skills={confirmed}
          selected={selectedSkill}
          onSelect={handleSelect}
        />
        <SkillsBucket
          title="Promoted"
          skills={promoted}
          selected={selectedSkill}
          onSelect={handleSelect}
        />
      </div>
      {onNewFromWalkthrough && (
        <button
          type="button"
          onClick={onNewFromWalkthrough}
          className="border-t border-[var(--border)] bg-[var(--bg-input)] px-3 py-2 text-left text-[10px] uppercase tracking-wider text-[var(--text-muted)] hover:text-[var(--text-primary)]"
        >
          + New from walkthrough...
        </button>
      )}
    </div>
  );
}

interface SkillsBucketProps {
  title: string;
  skills: SkillSummary[];
  selected: { id: string; version: number } | null;
  onSelect: (id: string, version: number) => void;
}

function SkillsBucket({
  title,
  skills,
  selected,
  onSelect,
}: SkillsBucketProps) {
  return (
    <section className="mb-3">
      <h3 className="mb-1 text-[10px] font-semibold uppercase tracking-wider text-[var(--text-muted)]">
        {title} ({skills.length})
      </h3>
      {skills.length === 0 ? (
        <p className="text-[10px] italic text-[var(--text-muted)]">empty</p>
      ) : (
        <ul className="space-y-0.5">
          {skills.map((skill) => {
            const isSelected =
              selected?.id === skill.id && selected.version === skill.version;
            return (
              <li key={`${skill.id}-v${skill.version}`}>
                <button
                  type="button"
                  onClick={() => onSelect(skill.id, skill.version)}
                  className={`w-full rounded px-2 py-1 text-left text-xs ${
                    isSelected
                      ? "bg-[var(--accent-coral)] text-white"
                      : "text-[var(--text-secondary)] hover:bg-[var(--bg-input)]"
                  }`}
                >
                  <span className="truncate">{skill.name || skill.id}</span>
                  <span className="ml-1 text-[10px] opacity-70">
                    v{skill.version}
                  </span>
                </button>
              </li>
            );
          })}
        </ul>
      )}
    </section>
  );
}
