import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { WalkthroughAction, WalkthroughAnnotations, Workflow } from "../bindings";
import type { ActionNodeEntry, WalkthroughStatus } from "../store/slices/walkthroughSlice";

type MockFn = ReturnType<typeof vi.fn>;

type MockWalkthrough = {
  status: WalkthroughStatus;
  panelOpen: boolean;
  error: string | null;
  sessionId: string;
  actions: WalkthroughAction[];
  draft: Workflow | null;
  warnings: string[];
  annotations: WalkthroughAnnotations;
  expandedAction: string | null;
  actionNodeMap: ActionNodeEntry[];
  nodeOrder: string[];
  setPanelOpen: MockFn;
  setExpandedAction: MockFn;
  keepCandidate: MockFn;
  dismissCandidate: MockFn;
  deleteNode: MockFn;
  restoreNode: MockFn;
  renameNode: MockFn;
  overrideTarget: MockFn;
  promoteToVariable: MockFn;
  removeVariablePromotion: MockFn;
  reorderNode: MockFn;
  reorderGroup: MockFn;
  applyDraftToCanvas: MockFn;
  discardDraft: MockFn;
};

const mocks = vi.hoisted(() => ({
  invoke: vi.fn(),
  loadSkillsForPanel: vi.fn(),
  walkthrough: {
    status: "Review",
    panelOpen: true,
    error: null,
    sessionId: "session-1",
    actions: [
      {
        id: "action-1",
        candidate: false,
        kind: { type: "Click", x: 1, y: 1, button: "Left", click_count: 1 },
        app_name: null,
        window_title: null,
        artifact_paths: [],
        source_event_ids: [],
        target_candidates: [],
        warnings: [],
        confidence: "High",
      },
    ],
    draft: {
      id: "draft-1",
      name: "Draft",
      nodes: [],
      edges: [],
      groups: [],
      intent: null,
    },
    warnings: [],
    annotations: {
      deleted_node_ids: [],
      renamed_nodes: [],
      target_overrides: [],
      variable_promotions: [],
    },
    expandedAction: null,
    actionNodeMap: [],
    nodeOrder: [],
    setPanelOpen: vi.fn(),
    setExpandedAction: vi.fn(),
    keepCandidate: vi.fn(),
    dismissCandidate: vi.fn(),
    deleteNode: vi.fn(),
    restoreNode: vi.fn(),
    renameNode: vi.fn(),
    overrideTarget: vi.fn(),
    promoteToVariable: vi.fn(),
    removeVariablePromotion: vi.fn(),
    reorderNode: vi.fn(),
    reorderGroup: vi.fn(),
    applyDraftToCanvas: vi.fn(),
    discardDraft: vi.fn(),
  } as MockWalkthrough,
}));

vi.mock("@tauri-apps/api/core", () => ({
  convertFileSrc: (path: string) => path,
  invoke: (...args: unknown[]) => mocks.invoke(...args),
}));

vi.mock("../hooks/useHorizontalResize", () => ({
  useHorizontalResize: () => ({
    width: 360,
    handleResizeStart: vi.fn(),
  }),
}));

vi.mock("../hooks/useWalkthrough", () => ({
  useWalkthrough: () => mocks.walkthrough,
}));

vi.mock("../store/useAppStore", () => ({
  useStore: (selector: (state: unknown) => unknown) =>
    selector({
      assistantSurface: null,
      projectPath: null,
      workflow: {
        id: "workflow-1",
        name: "Workflow",
        nodes: [],
        edges: [],
        groups: [],
      },
      storeTraces: true,
      skillsEnabled: true,
      skillsGlobalParticipation: false,
      loadSkillsForPanel: mocks.loadSkillsForPanel,
    }),
}));

vi.mock("../utils/walkthroughGrouping", () => ({
  computeAppGroups: () => [],
}));

import { WalkthroughPanel } from "./WalkthroughPanel";

describe("WalkthroughPanel", () => {
  beforeEach(() => {
    mocks.invoke.mockReset();
    mocks.invoke.mockResolvedValue({
      id: "skill-1",
      version: 1,
      name: "Saved Skill",
    });
    mocks.loadSkillsForPanel.mockReset();
    mocks.loadSkillsForPanel.mockResolvedValue(undefined);
    mocks.walkthrough.actions = [
      {
        id: "action-1",
        candidate: false,
        kind: { type: "Click", x: 1, y: 1, button: "Left", click_count: 1 },
        app_name: null,
        window_title: null,
        artifact_paths: [],
        source_event_ids: [],
        target_candidates: [],
        warnings: [],
        confidence: "High",
      },
    ];
    mocks.walkthrough.draft = {
      id: "draft-1",
      name: "Draft",
      nodes: [],
      edges: [],
      groups: [],
      intent: null,
    };
    mocks.walkthrough.annotations = {
      deleted_node_ids: [],
      renamed_nodes: [],
      target_overrides: [],
      variable_promotions: [],
    };
    mocks.walkthrough.actionNodeMap = [];
    mocks.walkthrough.nodeOrder = [];
  });

  it("saves the reviewed walkthrough as a skill", async () => {
    render(<WalkthroughPanel />);

    fireEvent.click(screen.getByRole("button", { name: /save as skill/i }));

    await waitFor(() => {
      expect(mocks.invoke).toHaveBeenCalledWith("save_walkthrough_as_skill", {
        request: {
          session_id: "session-1",
          project_path: null,
          project_name: "Workflow",
          project_id: "workflow-1",
          reviewed_draft: {
            id: "workflow-1",
            name: "Workflow",
            nodes: [],
            edges: [],
            groups: [],
            intent: null,
          },
          reviewed_actions: mocks.walkthrough.actions,
          store_traces: true,
        },
      });
    });
    await waitFor(() => {
      expect(mocks.loadSkillsForPanel).toHaveBeenCalledWith({
        projectPath: null,
        projectName: "Workflow",
        projectId: "workflow-1",
        includeGlobal: false,
        storeTraces: true,
      });
    });
  });

  it("sends the reviewed annotated draft when saving as a skill", async () => {
    mocks.walkthrough.draft = {
      id: "draft-1",
      name: "Draft",
      nodes: [
        {
          id: "node-1",
          name: "Original",
          node_type: { type: "Click", target: null, button: "Left", click_count: 1 },
          position: { x: 0, y: 0 },
          enabled: true,
          timeout_ms: null,
          settle_ms: null,
          retries: 0,
          supervision_retries: 2,
          trace_level: "Minimal",
          role: "Default",
          expected_outcome: null,
        },
        {
          id: "node-2",
          name: "Deleted",
          node_type: { type: "Click", target: null, button: "Left", click_count: 1 },
          position: { x: 0, y: 100 },
          enabled: true,
          timeout_ms: null,
          settle_ms: null,
          retries: 0,
          supervision_retries: 2,
          trace_level: "Minimal",
          role: "Default",
          expected_outcome: null,
        },
      ],
      edges: [{ from: "node-1", to: "node-2" }],
      groups: [],
      intent: null,
    };
    mocks.walkthrough.annotations = {
      deleted_node_ids: ["node-2"],
      renamed_nodes: [{ node_id: "node-1", new_name: "Reviewed" }],
      target_overrides: [],
      variable_promotions: [],
    };
    mocks.walkthrough.nodeOrder = ["node-2", "node-1"];

    render(<WalkthroughPanel />);
    fireEvent.click(screen.getByRole("button", { name: /save as skill/i }));

    await waitFor(() => {
      const request = mocks.invoke.mock.calls[0][1].request;
      expect(request.reviewed_draft.nodes).toHaveLength(1);
      expect(request.reviewed_draft.nodes[0].id).toBe("node-1");
      expect(request.reviewed_draft.nodes[0].name).toBe("Reviewed");
      expect(request.reviewed_draft.edges).toEqual([]);
    });
  });
});
