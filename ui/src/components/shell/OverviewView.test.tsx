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
  StatsStrip: () => <div data-testid="stats-strip" />,
}));
vi.mock("./WorkflowRow", () => ({
  WorkflowRow: () => <div data-testid="workflow-row" />,
}));
vi.mock("../skills/SkillsPanel", () => ({
  SkillsPanel: () => <div data-testid="skills-panel" />,
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
});
