import type { StateCreator } from "zustand";
import { invoke } from "@tauri-apps/api/core";
import type { StoreState } from "./types";

/// Lightweight projection of the engine's `Skill` type for the panel.
/// Mirrors `SkillSummary` produced by the `list_skills_for_panel` Tauri
/// command. Auto-generated bindings will replace this once `cargo run`
/// regenerates `bindings.ts` in dev mode.
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

export interface SkillsSlice {
  drafts: SkillSummary[];
  confirmed: SkillSummary[];
  promoted: SkillSummary[];
  selectedSkill: { id: string; version: number } | null;
  breadcrumb: SkillBreadcrumbEntry[];

  setSkillsList: (list: SkillSummary[]) => void;
  loadSkillsForPanel: (request: LoadSkillsForPanelRequest) => Promise<void>;
  setSelectedSkill: (id: string, version: number) => void;
  clearSelectedSkill: () => void;
  findSkill: (id: string, version: number) => SkillSummary | null;
  applySkillExtracted: (event: SkillExtractedPayload) => void;
  applySkillConfirmed: (event: SkillConfirmedPayload) => void;
  pushSkillBreadcrumb: (entry: SkillBreadcrumbEntry) => void;
  popSkillBreadcrumbTo: (idx: number) => void;
  popSkillBreadcrumb: () => void;
  clearSkillBreadcrumb: () => void;
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
    const baseRequest = {
      project_path: projectPath,
      project_name: projectName,
      project_id: projectId,
      store_traces: storeTraces,
    };
    const projectLocal = await invoke<SkillSummary[]>("list_skills_for_panel", {
      request: { ...baseRequest, scope: "project_local" },
    });
    const global = includeGlobal
      ? await invoke<SkillSummary[]>("list_skills_for_panel", {
          request: { ...baseRequest, scope: "global" },
        })
      : [];
    set(bucketize([...projectLocal, ...global]));
  },

  setSelectedSkill: (id, version) => set({ selectedSkill: { id, version } }),

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
});
