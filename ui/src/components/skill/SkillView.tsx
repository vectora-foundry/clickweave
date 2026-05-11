/**
 * `SkillView` — primary skill rendering surface for Phase 1.F.
 *
 * Renders `selectedSkill.sections` as a virtualized vertical scrolling list of
 * `SkillSectionCard` components via `react-window` `FixedSizeList`.
 *
 * Selection state is managed by `SkillSelectionContext`:
 * - Single click → `selectSingle`
 * - Shift+click → `extendRange`
 * - ⌘/Ctrl+click → `toggleMulti`
 */

import { useRef, useState, useEffect } from "react";
import { FixedSizeList, type ListChildComponentProps } from "react-window";
import type { JsonValue, Skill, SkillSection } from "../../bindings";
import { useStore } from "../../store/useAppStore";
import { SkillSectionCard } from "./SkillSectionCard";
import {
  SkillSelectionProvider,
  useSkillSelection,
} from "./SkillSelectionContext";
import { RunWithValuesForm } from "./RunWithValuesForm";

const ITEM_HEIGHT = 80; // px, fixed-height card row

interface SectionRowData {
  sections: SkillSection[];
  body: string;
  selectedIds: string[];
  onSectionClick: (section: SkillSection, e: React.MouseEvent) => void;
  onResume: (sectionId: string) => void;
}

function SectionRow({
  index,
  style,
  data,
}: ListChildComponentProps<SectionRowData>) {
  const { sections, body, selectedIds, onSectionClick, onResume } = data;
  const section = sections[index];
  if (!section) return null;

  const [start, end] = section.body_range;
  const sectionBody = body.slice(start, end);
  const isSelected = selectedIds.includes(section.id);

  return (
    <div style={style} className="px-2 py-1">
      <SkillSectionCard
        section={section}
        sectionBody={sectionBody}
        selected={isSelected}
        onClick={(e) => onSectionClick(section, e)}
        onResume={onResume}
      />
    </div>
  );
}

interface SkillViewInnerProps {
  skill: Skill;
  onResume: (sectionId: string) => void;
}

function SkillViewInner({ skill, onResume }: SkillViewInnerProps) {
  const { selectedSectionIds, selectSingle, extendRange, toggleMulti } =
    useSkillSelection();

  const containerRef = useRef<HTMLDivElement>(null);
  const [listHeight, setListHeight] = useState(400);

  // Observe the container's height to keep the FixedSizeList sized correctly.
  useEffect(() => {
    const el = containerRef.current;
    if (!el) return;
    const obs = new ResizeObserver((entries) => {
      const entry = entries[0];
      if (entry) {
        setListHeight(entry.contentRect.height);
      }
    });
    obs.observe(el);
    setListHeight(el.clientHeight);
    return () => obs.disconnect();
  }, []);

  const sections = skill.sections ?? [];
  const body = skill.body ?? "";

  const handleSectionClick = (section: SkillSection, e: React.MouseEvent) => {
    if (e.shiftKey) {
      extendRange(
        section.id,
        sections.map((s) => s.id),
      );
    } else if (e.metaKey || e.ctrlKey) {
      toggleMulti(section.id);
    } else {
      selectSingle(section.id);
    }
  };

  if (sections.length === 0) {
    return (
      <div className="flex h-full items-center justify-center text-sm text-[var(--text-muted)]">
        No sections in this skill.
      </div>
    );
  }

  const itemData: SectionRowData = {
    sections,
    body,
    selectedIds: selectedSectionIds,
    onSectionClick: handleSectionClick,
    onResume,
  };

  return (
    <div ref={containerRef} className="h-full w-full">
      <FixedSizeList
        width="100%"
        height={listHeight}
        itemCount={sections.length}
        itemSize={ITEM_HEIGHT}
        itemData={itemData}
        overscanCount={3}
      >
        {SectionRow}
      </FixedSizeList>
    </div>
  );
}

export function SkillView() {
  const selectedSkill = useStore((s) => s.selectedSkill);
  const skillFrozen = useStore((s) => s.skillFrozen);
  const executorState = useStore((s) => s.executorState);
  const runSkillFromView = useStore((s) => s.runSkillFromView);
  const resumeSkillFromFailure = useStore((s) => s.resumeSkillFromFailure);
  const stopWorkflow = useStore((s) => s.stopWorkflow);
  const [showRunForm, setShowRunForm] = useState(false);

  if (!selectedSkill) {
    return (
      <div className="flex h-full items-center justify-center text-sm text-[var(--text-muted)]">
        Select a skill to view its sections.
      </div>
    );
  }

  const isRunning = executorState === "running";

  const handleRunClick = () => {
    const variables = selectedSkill.variables ?? [];
    const hasRequiredVars = variables.some((v) => v.default === null || v.default === undefined);
    if (hasRequiredVars) {
      setShowRunForm(true);
    } else {
      runSkillFromView(selectedSkill.id);
    }
  };

  const handleResume = (sectionId: string) => {
    resumeSkillFromFailure(selectedSkill.id, sectionId);
  };

  return (
    <SkillSelectionProvider skillId={selectedSkill.id}>
      <div className="flex h-full flex-col bg-[var(--bg-dark)]">
        {/* Skill header */}
        <div className="shrink-0 border-b border-[var(--border)] px-4 py-3">
          <div className="flex items-center justify-between">
            <div className="min-w-0 flex-1">
              <h2 className="text-sm font-semibold text-[var(--text-primary)]">
                {selectedSkill.name}
              </h2>
              {selectedSkill.description && (
                <p className="mt-0.5 text-xs text-[var(--text-secondary)] truncate">
                  {selectedSkill.description}
                </p>
              )}
            </div>
            {/* Run / Stop button */}
            <div className="shrink-0 ml-3">
              {isRunning ? (
                <button
                  type="button"
                  data-testid="stop-skill-run"
                  onClick={() => stopWorkflow()}
                  className="rounded px-2.5 py-1 text-xs font-medium border border-red-500/50 text-red-400 hover:bg-red-500/10"
                >
                  Stop
                </button>
              ) : (
                <button
                  type="button"
                  data-testid="run-skill"
                  onClick={handleRunClick}
                  disabled={skillFrozen}
                  className="rounded px-2.5 py-1 text-xs font-medium bg-[var(--accent-coral)] text-white hover:opacity-90 disabled:opacity-40"
                >
                  Run
                </button>
              )}
            </div>
          </div>
        </div>

        {/* Section list */}
        <div className="min-h-0 flex-1">
          <SkillViewInner skill={selectedSkill} onResume={handleResume} />
        </div>
      </div>

      {/* RunWithValuesForm modal */}
      {showRunForm && (
        <RunWithValuesForm
          skill={selectedSkill}
          onSubmit={(variables: Record<string, JsonValue>) => {
            setShowRunForm(false);
            runSkillFromView(selectedSkill.id, variables);
          }}
          onCancel={() => setShowRunForm(false)}
        />
      )}
    </SkillSelectionProvider>
  );
}
