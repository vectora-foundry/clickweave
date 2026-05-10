import { render, screen } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(),
}));

vi.mock("../AssistantPanel", () => ({
  AssistantPanel: ({ open }: { open: boolean }) =>
    open ? <div data-testid="assistant-panel" /> : null,
}));
vi.mock("../FloatingToolbar", () => ({
  FloatingToolbar: () => <div data-testid="floating-toolbar" />,
}));
vi.mock("../IntentEmptyState", () => ({
  IntentEmptyState: () => <div data-testid="intent-empty-state" />,
}));
vi.mock("../skills/SkillDetailView", () => ({
  SkillDetailView: () => <div data-testid="skill-detail-view" />,
}));
vi.mock("../WalkthroughPanel", () => ({
  WalkthroughPanel: () => <div data-testid="walkthrough-panel" />,
}));

import { CanvasView } from "./CanvasView";
import { useStore } from "../../store/useAppStore";

describe("CanvasView", () => {
  beforeEach(() => {
    useStore.setState({
      currentView: "canvas",
      isNewWorkflow: false,
      assistantSurface: null,
      selectedNode: null,
      selectedSkill: null,
      skillsEnabled: false,
      storeTraces: true,
      nodeTypes: [],
      workflow: {
        id: "wf-canvas",
        name: "Canvas Workflow",
        nodes: [
          {
            id: "node-1",
            name: "Node",
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
        edges: [],
        groups: [],
      },
    });
  });

  it("renders the floating toolbar", () => {
    render(<CanvasView />);

    expect(screen.getByTestId("floating-toolbar")).toBeInTheDocument();
  });

  it("does not render an editable node palette or node detail modal", () => {
    render(<CanvasView />);

    expect(screen.queryByTestId("node-palette")).not.toBeInTheDocument();
    expect(screen.queryByTestId("node-detail-modal")).not.toBeInTheDocument();
  });

  it("shows the assistant drawer when assistantSurface is 'drawer'", () => {
    useStore.setState({ assistantSurface: "drawer" });

    render(<CanvasView />);

    expect(screen.getByTestId("assistant-panel")).toBeInTheDocument();
  });
});
