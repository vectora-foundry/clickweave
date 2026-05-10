import { describe, it, expect, beforeEach, vi } from "vitest";

const commandMocks = vi.hoisted(() => ({
  listSkillsForPanel: vi.fn(),
  loadSkillFull: vi.fn(),
}));

vi.mock("../../bindings", () => {
  const cache = new Map<string | symbol, unknown>();
  const explicit: Record<string, unknown> = {
    listSkillsForPanel: commandMocks.listSkillsForPanel,
    loadSkillFull: commandMocks.loadSkillFull,
  };

  return {
    commands: new Proxy(explicit, {
      get(target, prop) {
        if (prop in target) return target[prop as string];
        if (!cache.has(prop)) cache.set(prop, vi.fn(async () => undefined));
        return cache.get(prop);
      },
    }),
  };
});

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(),
}));
vi.mock("@tauri-apps/api/webviewWindow", () => ({
  WebviewWindow: class {
    static async getByLabel() {
      return null;
    }
  },
}));
vi.mock("@tauri-apps/api/window", () => ({
  currentMonitor: async () => null,
}));

import { useStore } from "../useAppStore";
import type { SkillSummary } from "./skillsSlice";
import type { Skill } from "../../bindings";

function reset() {
  useStore.setState({
    drafts: [],
    confirmed: [],
    promoted: [],
    selectedSkill: null,
    breadcrumb: [],
  });
}

function summary(
  partial: Partial<SkillSummary> & { id: string; version: number },
): SkillSummary {
  return {
    name: partial.id,
    description: "",
    state: "draft",
    scope: "project_local",
    occurrence_count: 1,
    success_rate: 1,
    edited_by_user: false,
    ...partial,
  };
}

describe("skillsSlice.setSkillsList", () => {
  beforeEach(reset);

  it("buckets skills into drafts / confirmed / promoted by state", () => {
    useStore.getState().setSkillsList([
      summary({ id: "a", version: 1, state: "draft" }),
      summary({ id: "b", version: 1, state: "confirmed" }),
      summary({ id: "c", version: 2, state: "promoted" }),
      summary({ id: "d", version: 1, state: "draft" }),
    ]);
    const s = useStore.getState();
    expect(s.drafts.map((x) => x.id)).toEqual(["a", "d"]);
    expect(s.confirmed.map((x) => x.id)).toEqual(["b"]);
    expect(s.promoted.map((x) => x.id)).toEqual(["c"]);
  });
});

describe("skillsSlice.applySkillExtracted", () => {
  beforeEach(reset);

  it("inserts a stub draft entry on first extraction", () => {
    useStore.getState().applySkillExtracted({
      run_id: "r1",
      event_run_id: "r1",
      skill_id: "new-skill",
      version: 1,
      state: "draft",
      scope: "project_local",
    });
    expect(useStore.getState().drafts).toHaveLength(1);
    expect(useStore.getState().drafts[0].id).toBe("new-skill");
  });

  it("upserts on a second extraction with the same id+version", () => {
    useStore.getState().applySkillExtracted({
      run_id: "r1",
      event_run_id: "r1",
      skill_id: "skill-a",
      version: 1,
      state: "draft",
      scope: "project_local",
    });
    useStore.getState().applySkillExtracted({
      run_id: "r1",
      event_run_id: "r1",
      skill_id: "skill-a",
      version: 1,
      state: "draft",
      scope: "project_local",
    });
    expect(useStore.getState().drafts).toHaveLength(1);
  });
});

describe("skillsSlice.applySkillConfirmed", () => {
  beforeEach(reset);

  it("moves a skill from drafts to confirmed", () => {
    useStore
      .getState()
      .setSkillsList([summary({ id: "x", version: 1, state: "draft" })]);
    useStore.getState().applySkillConfirmed({
      run_id: "r1",
      event_run_id: "r1",
      skill_id: "x",
      version: 1,
    });
    const s = useStore.getState();
    expect(s.drafts).toHaveLength(0);
    expect(s.confirmed.map((x) => x.id)).toEqual(["x"]);
  });
});

describe("skillsSlice.loadSkillsForPanel", () => {
  beforeEach(() => {
    reset();
    vi.clearAllMocks();
  });

  it("does not read skill files when trace persistence is disabled", async () => {
    useStore
      .getState()
      .setSkillsList([summary({ id: "existing", version: 1 })]);

    await useStore.getState().loadSkillsForPanel({
      projectPath: null,
      projectName: "Workflow",
      projectId: "workflow-1",
      includeGlobal: true,
      storeTraces: false,
    });

    expect(commandMocks.listSkillsForPanel).not.toHaveBeenCalled();
    expect(useStore.getState().drafts).toHaveLength(0);
  });

  it("passes the privacy gate to both project and global list commands", async () => {
    commandMocks.listSkillsForPanel.mockResolvedValue({ status: "ok", data: [] });

    await useStore.getState().loadSkillsForPanel({
      projectPath: "/tmp/project.json",
      projectName: "Workflow",
      projectId: "workflow-1",
      includeGlobal: true,
      storeTraces: true,
    });

    expect(commandMocks.listSkillsForPanel).toHaveBeenCalledWith({
      project_path: "/tmp/project.json",
      project_name: "Workflow",
      project_id: "workflow-1",
      scope: "project_local",
      store_traces: true,
    });
    expect(commandMocks.listSkillsForPanel).toHaveBeenCalledWith({
      project_path: "/tmp/project.json",
      project_name: "Workflow",
      project_id: "workflow-1",
      scope: "global",
      store_traces: true,
    });
  });
});

describe("skillsSlice.loadSelectedSkill", () => {
  beforeEach(() => {
    reset();
    vi.clearAllMocks();
  });

  const baseRequest = {
    projectPath: "/tmp/project.json",
    projectName: "Workflow",
    projectId: "workflow-1",
    includeGlobal: false,
    storeTraces: true,
  };

  const fullSkill: Skill = {
    id: "skl_abc",
    version: 1,
    name: "Test Skill",
    description: "A test skill",
    state: "confirmed",
    scope: "project_local",
    tags: [],
    subgoal_text: "",
    subgoal_signature: "",
    applicability: { apps: [], hosts: [], signature: "" },
    parameter_schema: [],
    action_sketch: [],
    outputs: [],
    outcome_predicate: { type: "subgoal_completed", post_state_world_model_signature: null },
    provenance: [],
    stats: { occurrence_count: 1, success_rate: 1, last_seen_at: null, last_invoked_at: null },
    edited_by_user: false,
    created_at: "2026-01-01T00:00:00Z",
    updated_at: "2026-01-01T00:00:00Z",
    produced_node_ids: [],
    body: "# Test Skill",
    schema_version: 1,
    variables: [],
    sections: [
      { id: "section_1", heading: "Step 1", level: 2, step_ids: [], body_range: [0, 10] },
    ],
  };

  // (a) Selecting a skill triggers load_skill_full and stores the full Skill
  it("selecting a skill triggers load_skill_full and stores full Skill shape", async () => {
    commandMocks.loadSkillFull.mockResolvedValue({ status: "ok", data: fullSkill });

    await useStore.getState().loadSelectedSkill({
      ...baseRequest,
      skill_id: "skl_abc",
      version: 1,
    });

    expect(commandMocks.loadSkillFull).toHaveBeenCalledWith({
      skill_id: "skl_abc",
      version: 1,
      project_path: "/tmp/project.json",
      project_name: "Workflow",
      project_id: "workflow-1",
      store_traces: true,
    });

    const state = useStore.getState();
    expect(state.selectedSkill).not.toBeNull();
    expect(state.selectedSkill?.id).toBe("skl_abc");
    expect(state.selectedSkill?.sections).toHaveLength(1);
    expect(state.selectedSkill?.sections?.[0].heading).toBe("Step 1");
  });

  // (b) Clearing project clears selectedSkill
  it("clearSelectedSkill clears the selectedSkill and breadcrumb", async () => {
    commandMocks.loadSkillFull.mockResolvedValue({ status: "ok", data: fullSkill });
    await useStore.getState().loadSelectedSkill({
      ...baseRequest,
      skill_id: "skl_abc",
      version: 1,
    });
    expect(useStore.getState().selectedSkill).not.toBeNull();

    useStore.getState().clearSelectedSkill();
    expect(useStore.getState().selectedSkill).toBeNull();
    expect(useStore.getState().breadcrumb).toHaveLength(0);
  });
});

describe("skillsSlice.breadcrumb", () => {
  beforeEach(reset);

  it("push then pop maintains stack order", () => {
    useStore
      .getState()
      .pushSkillBreadcrumb({ id: "parent", version: 1, name: "parent" });
    useStore
      .getState()
      .pushSkillBreadcrumb({ id: "child", version: 1, name: "child" });
    expect(useStore.getState().breadcrumb).toHaveLength(2);
    useStore.getState().popSkillBreadcrumb();
    expect(useStore.getState().breadcrumb.map((x) => x.id)).toEqual(["parent"]);
  });
});
