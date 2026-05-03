import { describe, expect, it, beforeEach, vi } from "vitest";
import { fireEvent, render, screen } from "@testing-library/react";

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(),
}));

vi.mock("./CanvasPreviewCard", () => ({
  CanvasPreviewCard: () => <div data-testid="canvas-preview-card" />,
}));
vi.mock("./LiveRuntimeCard", () => ({
  LiveRuntimeCard: () => <div data-testid="live-runtime-card" />,
}));
vi.mock("./OverviewAssistantCard", () => ({
  OverviewAssistantCard: () => <div data-testid="overview-assistant-card" />,
}));
vi.mock("./StatsStrip", () => ({
  StatsStrip: ({
    onOpenSkillsManager,
  }: {
    onOpenSkillsManager: () => void;
  }) => (
    <div data-testid="stats-strip">
      <button type="button" onClick={onOpenSkillsManager}>
        Skills Manager
      </button>
    </div>
  ),
}));
vi.mock("./WorkflowRow", () => ({
  WorkflowRow: () => <div data-testid="workflow-row" />,
}));
vi.mock("../skills/SkillDetailView", () => ({
  SkillDetailView: ({
    skillId,
    version,
  }: {
    skillId: string;
    version: number;
  }) => (
    <div data-testid="skill-detail-view">
      {skillId} v{version}
    </div>
  ),
}));

import { OverviewView } from "./OverviewView";
import { useStore } from "../../store/useAppStore";

describe("OverviewView walkthrough entry", () => {
  beforeEach(() => {
    useStore.setState({
      currentView: "overview",
      isNewWorkflow: true,
      agentStatus: "idle",
      walkthroughCdpModalOpen: false,
      walkthroughCdpProgress: [],
      workflow: {
        ...useStore.getState().workflow,
        nodes: [],
        edges: [],
        groups: [],
      },
    });
  });

  it("switches to Canvas before opening the CDP walkthrough modal", () => {
    render(<OverviewView />);

    fireEvent.click(screen.getByRole("button", { name: /record walkthrough/i }));

    expect(useStore.getState().currentView).toBe("canvas");
    expect(useStore.getState().walkthroughCdpModalOpen).toBe(true);
  });

  it("allows overview grid columns to shrink around long runtime content", () => {
    useStore.setState({
      isNewWorkflow: false,
      workflow: {
        ...useStore.getState().workflow,
        nodes: [
          {
            id: "node-1",
            name: "Start",
            node_type: { type: "Unknown" },
            position: { x: 0, y: 0 },
            enabled: true,
            timeout_ms: null,
            settle_ms: null,
            retries: 0,
            trace_level: "Minimal",
            expected_outcome: null,
          },
        ],
      },
    });

    const { container } = render(<OverviewView />);

    const overviewGrid = container.querySelector(".grid.min-h-0.min-w-0.flex-1");
    expect(overviewGrid).toBeInTheDocument();
    expect(screen.getByTestId("overview-assistant-card").parentElement).toHaveClass(
      "min-w-0",
    );
    expect(screen.getByTestId("live-runtime-card").parentElement).toHaveClass(
      "min-w-0",
    );
    expect(screen.getByTestId("canvas-preview-card").parentElement).toHaveClass(
      "min-w-0",
    );
    expect(
      screen.getByTestId("live-runtime-card").parentElement?.parentElement,
    ).toHaveClass("min-w-0");
  });

  it("shows skill details inside the Overview Skills Manager drawer after selection", () => {
    useStore.setState({
      isNewWorkflow: false,
      selectedSkill: null,
      drafts: [
        {
          id: "login-skill",
          version: 1,
          name: "Login Skill",
          description: "Logs in to the app",
          state: "draft",
          scope: "project_local",
          occurrence_count: 1,
          success_rate: 1,
          edited_by_user: false,
        },
      ],
      confirmed: [],
      promoted: [],
      projectPath: "/tmp/project.clickweave",
      storeTraces: true,
      skillsGlobalParticipation: false,
      workflow: {
        ...useStore.getState().workflow,
        id: "workflow-1",
        name: "Workflow",
        nodes: [
          {
            id: "node-1",
            name: "Start",
            node_type: { type: "Unknown" },
            position: { x: 0, y: 0 },
            enabled: true,
            timeout_ms: null,
            settle_ms: null,
            retries: 0,
            trace_level: "Minimal",
            expected_outcome: null,
          },
        ],
      },
    });

    render(<OverviewView />);

    fireEvent.click(screen.getByRole("button", { name: /skills manager/i }));
    fireEvent.click(screen.getByRole("button", { name: /login skillv1/i }));

    expect(screen.getByTestId("skill-detail-view")).toHaveTextContent(
      "login-skill v1",
    );
  });
});
