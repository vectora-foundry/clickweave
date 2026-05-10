import { describe, expect, it, beforeEach, vi } from "vitest";
import { fireEvent, render, screen } from "@testing-library/react";

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(),
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
    });
  });

  it("switches to Canvas before opening the CDP walkthrough modal", () => {
    render(<OverviewView />);

    fireEvent.click(screen.getByRole("button", { name: /record walkthrough/i }));

    expect(useStore.getState().currentView).toBe("canvas");
    expect(useStore.getState().walkthroughCdpModalOpen).toBe(true);
  });

  it("allows overview grid columns to shrink around long runtime content", () => {
    useStore.setState({ isNewWorkflow: false });

    const { container } = render(<OverviewView />);

    const overviewGrid = container.querySelector(".grid.min-h-0.min-w-0.flex-1");
    expect(overviewGrid).toBeInTheDocument();
    expect(screen.getByTestId("overview-assistant-card").parentElement).toHaveClass(
      "min-w-0",
    );
    expect(screen.getByTestId("live-runtime-card").parentElement).toHaveClass(
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
      projectId: "workflow-1",
      projectName: "Workflow",
      storeTraces: true,
      skillsGlobalParticipation: false,
    });

    render(<OverviewView />);

    fireEvent.click(screen.getByRole("button", { name: /skills manager/i }));
    fireEvent.click(screen.getByRole("button", { name: /login skillv1/i }));

    expect(screen.getByTestId("skill-detail-view")).toHaveTextContent(
      "login-skill v1",
    );
  });
});
