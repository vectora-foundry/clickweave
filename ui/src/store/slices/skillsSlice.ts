import type { StateCreator } from "zustand";
import { commands } from "../../bindings";
import type { Skill } from "../../bindings";
import { errorMessage } from "../../utils/commandError";
import type { StoreState } from "./types";

/// Lightweight projection of the engine's `Skill` type for the panel.
/// Mirrors `SkillSummary` produced by the `list_skills_for_panel` Tauri
/// command. Kept as a local interface so existing consumers (SkillsPanel,
/// StatsStrip, SkillDetailView) that use the old optional-field shape
/// stay compatible until 1.G cleans them up.
export interface SkillSummary {
  id: string;
  version: number;
  name: string;
  description: string;
  state: "draft" | "confirmed" | "promoted";
  scope: "project_local" | "global";
  tags?: string[];
  parameter_schema?: ParameterSlot[];
  applicability?: unknown;
  action_sketch?: ActionSketchStep[];
  proposal?: SkillRefinementProposal | null;
  occurrence_count: number;
  success_rate: number;
  edited_by_user: boolean;
}

export interface ParameterSlot {
  name: string;
  type_tag: string;
  description?: string | null;
  default?: unknown | null;
  enum_values?: string[] | null;
}

export type ActionSketchStep =
  | {
      kind: "tool_call";
      tool: string;
      args: unknown;
      captures_pre?: unknown[];
      captures?: unknown[];
      expected_world_model_delta?: unknown;
    }
  | {
      kind: "sub_skill";
      skill_id: string;
      version: number;
      parameters: unknown;
      bind_outputs_as: Record<string, string>;
    }
  | {
      kind: "loop";
      until: LoopPredicate;
      body: ActionSketchStep[];
      max_iterations: number;
      iteration_delay_ms: number;
    };

export type LoopPredicate =
  | { kind: "world_model_delta"; expr: string }
  | { kind: "step_count_reached"; count: number };

export interface BindingCorrection {
  step_index: number;
  capture_name: string;
  keep: boolean;
  correction: unknown | null;
}

export interface SkillRefinementProposal {
  parameter_schema: ParameterSlot[];
  binding_corrections: BindingCorrection[];
  description: string;
  name_suggestion: string | null;
}

export interface SkillExtractedPayload {
  run_id: string;
  event_run_id: string;
  skill_id: string;
  version: number;
  state: SkillSummary["state"];
  scope: SkillSummary["scope"];
}

export interface SkillConfirmedPayload {
  run_id: string;
  event_run_id: string;
  skill_id: string;
  version: number;
}

export interface SkillBreadcrumbEntry {
  id: string;
  version: number;
  name: string;
}

export type SectionRunStatus = "pending" | "running" | "succeeded" | "repaired" | "failed" | "skipped";

export interface SkillsSlice {
  drafts: SkillSummary[];
  confirmed: SkillSummary[];
  promoted: SkillSummary[];
  /** Full `Skill` shape loaded on selection; `null` when nothing is selected. */
  selectedSkill: Skill | null;
  breadcrumb: SkillBreadcrumbEntry[];
  /**
   * Per-section run state painted by the skill runner. Keys are section IDs;
   * values are one of the SectionRunStatus literals. Cleared when a new run
   * starts or the selected skill changes.
   */
  sectionRunState: Record<string, SectionRunStatus>;
  /** The section ID that failed during the last run, for failure handoff. */
  failedSectionId: string | null;
  /** Error message from the last section failure, for chat pre-fill. */
  failedSectionError: string | null;

  setSkillsList: (list: SkillSummary[]) => void;
  loadSkillsForPanel: (request: LoadSkillsForPanelRequest) => Promise<void>;
  /** Select a skill by id+version and load its full shape via IPC. */
  loadSelectedSkill: (request: LoadSkillsForPanelRequest & { skill_id: string; version: number }) => Promise<void>;
  setSelectedSkill: (id: string, version: number) => void;
  clearSelectedSkill: () => void;
  findSkill: (id: string, version: number) => SkillSummary | null;
  applySkillExtracted: (event: SkillExtractedPayload) => void;
  applySkillConfirmed: (event: SkillConfirmedPayload) => void;
  pushSkillBreadcrumb: (entry: SkillBreadcrumbEntry) => void;
  popSkillBreadcrumbTo: (idx: number) => void;
  popSkillBreadcrumb: () => void;
  clearSkillBreadcrumb: () => void;
  /** Seed all sections of the selected skill as "pending" when a run starts. */
  initSectionRunState: () => void;
  /** Mark a specific section by ID with the given status. */
  setSectionRunStatus: (sectionId: string, status: SectionRunStatus) => void;
  /** Flip all sections to the given terminal status (succeeded or failed). */
  finalizeSectionRunState: (status: "succeeded" | "failed") => void;
  /** Record a section failure for failure handoff. */
  recordSectionFailure: (sectionId: string, error: string) => void;
  /** Clear all run state (used between runs). */
  clearSectionRunState: () => void;
}

export interface LoadSkillsForPanelRequest {
  projectPath: string | null;
  projectName: string;
  projectId: string;
  includeGlobal: boolean;
  storeTraces: boolean;
}

function bucketize(list: SkillSummary[]): {
  drafts: SkillSummary[];
  confirmed: SkillSummary[];
  promoted: SkillSummary[];
} {
  const drafts: SkillSummary[] = [];
  const confirmed: SkillSummary[] = [];
  const promoted: SkillSummary[] = [];
  for (const s of list) {
    if (s.state === "draft") drafts.push(s);
    else if (s.state === "confirmed") confirmed.push(s);
    else if (s.state === "promoted") promoted.push(s);
  }
  return { drafts, confirmed, promoted };
}

export const createSkillsSlice: StateCreator<
  StoreState,
  [],
  [],
  SkillsSlice
> = (set, get) => ({
  drafts: [],
  confirmed: [],
  promoted: [],
  selectedSkill: null,
  breadcrumb: [],
  sectionRunState: {},
  failedSectionId: null,
  failedSectionError: null,

  setSkillsList: (list) => set(bucketize(list)),

  loadSkillsForPanel: async ({
    projectPath,
    projectName,
    projectId,
    includeGlobal,
    storeTraces,
  }) => {
    if (!storeTraces) {
      set(bucketize([]));
      return;
    }
    const { pushLog } = get();
    const baseRequest = {
      project_path: projectPath,
      project_name: projectName,
      project_id: projectId,
      store_traces: storeTraces,
    };
    const projectLocalResult = await commands.listSkillsForPanel({
      ...baseRequest,
      scope: "project_local",
    });
    if (projectLocalResult.status === "error") {
      pushLog(`Failed to load skills: ${errorMessage(projectLocalResult.error)}`);
      return;
    }
    const globalResult = includeGlobal
      ? await commands.listSkillsForPanel({ ...baseRequest, scope: "global" })
      : null;
    if (globalResult && globalResult.status === "error") {
      pushLog(`Failed to load global skills: ${errorMessage(globalResult.error)}`);
      return;
    }
    const globalList = globalResult?.status === "ok" ? globalResult.data : [];
    // Cast: bindings SkillSummary is structurally compatible with the local
    // SkillSummary interface (same wire format, just stricter required fields).
    const combined = [...projectLocalResult.data, ...globalList] as unknown as SkillSummary[];
    set(bucketize(combined));
  },

  loadSelectedSkill: async ({ projectPath, projectName, projectId, storeTraces, skill_id, version }) => {
    const { pushLog } = get();
    const result = await commands.loadSkillFull({
      skill_id,
      version,
      project_path: projectPath,
      project_name: projectName,
      project_id: projectId,
      store_traces: storeTraces,
    });
    if (result.status === "error") {
      pushLog(`Failed to load skill: ${errorMessage(result.error)}`);
      return;
    }
    set({ selectedSkill: result.data });
  },

  setSelectedSkill: (id, version) => {
    // Keep a lightweight stub so the sidebar can reflect selection state
    // immediately while loadSelectedSkill races in the background. Cleared
    // when clearSelectedSkill is called. The full Skill shape overwrites
    // this stub once the IPC call completes.
    const existing = get().findSkill(id, version);
    if (existing) {
      // Cast the SkillSummary to a partial Skill for selection display.
      // The full shape arrives from loadSelectedSkill before SkillView renders.
      set({ selectedSkill: existing as unknown as Skill });
    }
  },

  clearSelectedSkill: () => set({ selectedSkill: null, breadcrumb: [] }),

  findSkill: (id, version) => {
    const { drafts, confirmed, promoted } = get();
    return (
      drafts.find((s) => s.id === id && s.version === version) ??
      confirmed.find((s) => s.id === id && s.version === version) ??
      promoted.find((s) => s.id === id && s.version === version) ??
      null
    );
  },

  applySkillExtracted: (event) => {
    // Insert a stub entry into the right bucket. The next list refresh
    // (loadAll) will replace it with the full SkillSummary. This keeps
    // the panel responsive without an extra round-trip per event.
    const stub: SkillSummary = {
      id: event.skill_id,
      version: event.version,
      name: event.skill_id,
      description: "",
      state: event.state,
      scope: event.scope,
      occurrence_count: 1,
      success_rate: 1,
      edited_by_user: false,
    };
    const { drafts, confirmed, promoted } = get();
    const removeMatch = (xs: SkillSummary[]) =>
      xs.filter(
        (s) => !(s.id === event.skill_id && s.version === event.version),
      );
    const next: {
      drafts: SkillSummary[];
      confirmed: SkillSummary[];
      promoted: SkillSummary[];
    } = {
      drafts: removeMatch(drafts),
      confirmed: removeMatch(confirmed),
      promoted: removeMatch(promoted),
    };
    if (event.state === "draft") next.drafts.push(stub);
    else if (event.state === "confirmed") next.confirmed.push(stub);
    else if (event.state === "promoted") next.promoted.push(stub);
    set(next);
  },

  applySkillConfirmed: (event) => {
    const { drafts, confirmed } = get();
    const idx = drafts.findIndex(
      (s) => s.id === event.skill_id && s.version === event.version,
    );
    if (idx === -1) return;
    const moved = { ...drafts[idx], state: "confirmed" as const };
    set({
      drafts: drafts.slice(0, idx).concat(drafts.slice(idx + 1)),
      confirmed: confirmed.concat(moved),
    });
  },

  pushSkillBreadcrumb: (entry) =>
    set({ breadcrumb: [...get().breadcrumb, entry] }),

  popSkillBreadcrumbTo: (idx) => {
    const crumbs = get().breadcrumb;
    set({ breadcrumb: crumbs.slice(0, Math.max(0, idx + 1)) });
  },

  popSkillBreadcrumb: () => {
    const crumbs = get().breadcrumb;
    set({ breadcrumb: crumbs.slice(0, -1) });
  },

  clearSkillBreadcrumb: () => set({ breadcrumb: [] }),

  initSectionRunState: () => {
    const { selectedSkill } = get();
    if (!selectedSkill?.sections) {
      set({ sectionRunState: {}, failedSectionId: null, failedSectionError: null });
      return;
    }
    const initial: Record<string, SectionRunStatus> = {};
    for (const section of selectedSkill.sections) {
      initial[section.id] = "pending";
    }
    set({ sectionRunState: initial, failedSectionId: null, failedSectionError: null });
  },

  setSectionRunStatus: (sectionId, status) => {
    set((s) => ({
      sectionRunState: { ...s.sectionRunState, [sectionId]: status },
    }));
  },

  finalizeSectionRunState: (status) => {
    const { sectionRunState } = get();
    const next: Record<string, SectionRunStatus> = {};
    for (const id of Object.keys(sectionRunState)) {
      const current = sectionRunState[id];
      // Leave already-terminal states alone (failed sections stay failed).
      if (current === "failed" || current === "succeeded" || current === "repaired" || current === "skipped") {
        next[id] = current;
      } else {
        next[id] = status;
      }
    }
    set({ sectionRunState: next });
  },

  recordSectionFailure: (sectionId, error) => {
    set((s) => ({
      sectionRunState: { ...s.sectionRunState, [sectionId]: "failed" },
      failedSectionId: sectionId,
      failedSectionError: error,
    }));
  },

  clearSectionRunState: () => {
    set({ sectionRunState: {}, failedSectionId: null, failedSectionError: null });
  },
});
