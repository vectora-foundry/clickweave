import { useMemo } from "react";
import type { Edge, Node } from "@xyflow/react";
import { invoke } from "@tauri-apps/api/core";
import { useShallow } from "zustand/react/shallow";
import { GraphCanvas, type SkillCanvasSource } from "../GraphCanvas";
import { useStore } from "../../store/useAppStore";
import { SkillRefinementForm } from "./SkillRefinementForm";
import type {
  ActionSketchStep,
  LoopPredicate,
  SkillRefinementProposal,
  SkillSummary,
} from "../../store/slices/skillsSlice";

interface SkillDetailViewProps {
  skillId: string;
  version: number;
  projectPath: string | null;
  projectName: string;
  projectId: string;
  runId?: string | null;
  storeTraces: boolean;
  onChanged?: () => void;
}

export function SkillDetailView({
  skillId,
  version,
  projectPath,
  projectName,
  projectId,
  runId,
  storeTraces,
  onChanged,
}: SkillDetailViewProps) {
  const { breadcrumb, drafts, confirmed, promoted } = useStore(
    useShallow((s) => ({
      breadcrumb: s.breadcrumb,
      drafts: s.drafts,
      confirmed: s.confirmed,
      promoted: s.promoted,
    })),
  );
  const clearBreadcrumb = useStore((s) => s.clearSkillBreadcrumb);
  const popSkillBreadcrumbTo = useStore((s) => s.popSkillBreadcrumbTo);
  const pushSkillBreadcrumb = useStore((s) => s.pushSkillBreadcrumb);
  const setSelectedSkill = useStore((s) => s.setSelectedSkill);
  const applySkillConfirmed = useStore((s) => s.applySkillConfirmed);

  const allSkills = [...drafts, ...confirmed, ...promoted];
  const skill = allSkills.find(
    (s) => s.id === skillId && s.version === version,
  );
  const skillSource = useMemo<SkillCanvasSource | null>(() => {
    if (!skill?.action_sketch) return null;
    return projectSketchToCanvas(skill.action_sketch, allSkills, (child) => {
      if (skill) {
        pushSkillBreadcrumb({
          id: skill.id,
          version: skill.version,
          name: skill.name || skill.id,
        });
      }
      setSelectedSkill(child.id, child.version);
    });
  }, [allSkills, pushSkillBreadcrumb, setSelectedSkill, skill]);

  const confirmProposal = async (proposal: SkillRefinementProposal) => {
    await invoke("confirm_skill_proposal", {
      request: {
        skill_id: skillId,
        version,
        accepted_proposal: proposal,
        project_path: projectPath,
        project_name: projectName,
        project_id: projectId,
        run_id: runId ?? "",
        store_traces: storeTraces,
      },
    });
    applySkillConfirmed({
      run_id: runId ?? "",
      event_run_id: runId ?? "",
      skill_id: skillId,
      version,
    });
    onChanged?.();
  };

  const rejectProposal = async () => {
    await invoke("reject_skill_proposal", {
      request: {
        skill_id: skillId,
        version,
        project_path: projectPath,
        project_name: projectName,
        project_id: projectId,
        store_traces: storeTraces,
      },
    });
    onChanged?.();
  };

  return (
    <div className="flex h-full flex-col">
      {breadcrumb.length > 0 && (
        <nav className="flex items-center gap-1 border-b border-[var(--border)] px-3 py-2 text-xs">
          <button
            type="button"
            onClick={() => {
              const root = breadcrumb[0];
              if (root) setSelectedSkill(root.id, root.version);
              clearBreadcrumb();
            }}
            className="text-[var(--text-muted)] hover:text-[var(--text-primary)]"
          >
            Home
          </button>
          {breadcrumb.map((entry, idx) => (
            <span
              key={`${entry.id}-${entry.version}-${idx}`}
              className="flex items-center gap-1"
            >
              <span className="text-[var(--text-muted)]">/</span>
              <button
                type="button"
                onClick={() => {
                  popSkillBreadcrumbTo(idx - 1);
                  setSelectedSkill(entry.id, entry.version);
                }}
                className="text-[var(--text-secondary)] hover:text-[var(--text-primary)]"
              >
                {entry.name}
              </button>
            </span>
          ))}
        </nav>
      )}
      <div className="flex-1 overflow-y-auto p-3">
        {skill ? (
          <div>
            <h2 className="mb-1 text-sm font-semibold text-[var(--text-primary)]">
              {skill.name}{" "}
              <span className="text-xs opacity-60">v{skill.version}</span>
            </h2>
            <p className="mb-3 text-xs text-[var(--text-secondary)]">
              {skill.description || (
                <em className="text-[var(--text-muted)]">no description</em>
              )}
            </p>
            <dl className="grid grid-cols-2 gap-2 text-xs">
              <dt className="text-[var(--text-muted)]">State</dt>
              <dd>{skill.state}</dd>
              <dt className="text-[var(--text-muted)]">Scope</dt>
              <dd>{skill.scope}</dd>
              <dt className="text-[var(--text-muted)]">Occurrences</dt>
              <dd>{skill.occurrence_count}</dd>
              <dt className="text-[var(--text-muted)]">Success rate</dt>
              <dd>{(skill.success_rate * 100).toFixed(0)}%</dd>
            </dl>
            <div className="mt-4 h-[420px] overflow-hidden rounded border border-[var(--border)]">
              {skillSource ? (
                <GraphCanvas skillSource={skillSource} />
              ) : (
                <div className="flex h-full items-center justify-center text-[10px] italic text-[var(--text-muted)]">
                  Action sketch not loaded in panel index.
                </div>
              )}
            </div>
            {skill.state === "draft" && skill.proposal && (
              <div className="mt-4 rounded border border-[var(--border)] bg-[var(--bg-panel)]">
                <SkillRefinementForm
                  initial={skill.proposal}
                  onAccept={confirmProposal}
                  onReject={rejectProposal}
                />
              </div>
            )}
          </div>
        ) : (
          <p className="text-xs italic text-[var(--text-muted)]">
            Skill not found in panel index.
          </p>
        )}
      </div>
    </div>
  );
}

export function projectSketchToCanvas(
  sketch: ActionSketchStep[],
  skills: SkillSummary[],
  openSubSkill: (skill: { id: string; version: number }) => void,
): SkillCanvasSource {
  const nodes: Node[] = [];
  const edges: Edge[] = [];
  const childWidth = 280;
  const rowHeight = 210;

  const skillName = (id: string, version: number) =>
    skills.find((s) => s.id === id && s.version === version)?.name ?? id;

  function walk(
    steps: ActionSketchStep[],
    path: string,
    parentId: string | undefined,
    baseY: number,
  ) {
    let previousId: string | null = null;
    steps.forEach((step, idx) => {
      const id = `skill-step-${path}${idx}`;
      const common = parentId
        ? {
            parentId,
            extent: "parent" as const,
            position: { x: 36 + idx * childWidth, y: 92 },
          }
        : { position: { x: idx * childWidth, y: baseY } };

      if (step.kind === "tool_call") {
        nodes.push({
          id,
          type: "skillToolCall",
          data: { tool: step.tool, args: step.args },
          ...common,
        });
      } else if (step.kind === "sub_skill") {
        nodes.push({
          id,
          type: "skillSubSkill",
          data: {
            skillId: step.skill_id,
            version: step.version,
            name: skillName(step.skill_id, step.version),
            parameters: step.parameters,
            bindOutputsAs: step.bind_outputs_as,
            onOpen: () =>
              openSubSkill({ id: step.skill_id, version: step.version }),
          },
          ...common,
        });
      } else {
        const width = Math.max(320, step.body.length * childWidth + 72);
        nodes.push({
          id,
          type: "skillLoop",
          data: {
            label: "Loop",
            until: predicateText(step.until),
            maxIterations: step.max_iterations,
            childCount: step.body.length,
          },
          style: { width, height: rowHeight },
          ...common,
        });
        walk(step.body, `${path}${idx}-`, id, baseY + rowHeight);
      }

      if (previousId) {
        edges.push({
          id: `${previousId}-${id}`,
          source: previousId,
          target: id,
          type: "smoothstep",
        });
      }
      previousId = id;
    });
  }

  walk(sketch, "", undefined, 0);
  return { nodes, edges, readOnly: true };
}

function predicateText(predicate: LoopPredicate): string {
  if (predicate.kind === "world_model_delta") return predicate.expr;
  return `step count reaches ${predicate.count}`;
}
