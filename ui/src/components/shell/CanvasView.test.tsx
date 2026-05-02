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
vi.mock("../GraphCanvas", () => ({
  GraphCanvas: () => <div data-testid="graph-canvas" />,
}));
vi.mock("../IntentEmptyState", () => ({
  IntentEmptyState: () => <div data-testid="intent-empty-state" />,
}));
vi.mock("../node-detail/NodeDetailModal", () => ({
  NodeDetailModal: () => <div data-testid="node-detail-modal" />,
}));
vi.mock("../NodePalette", () => ({
  NodePalette: () => <div data-testid="node-palette" />,
}));
vi.mock("../skills/SkillsPanel", () => ({
  SkillsPanel: () => <div data-testid="skills-panel" />,
}));
vi.mock("../skills/SkillDetailView", () => ({
  SkillDetailView: () => <div data-testid="skill-detail-view" />,
}));
vi.mock("../WalkthroughPanel", () => ({
  WalkthroughPanel: () => <div data-testid="walkthrough-panel" />,
}));
vi.mock("../../hooks/useHandleDeleteNodes", () => ({
  useHandleDeleteGroupWithContents: () => () => {},
  useHandleDeleteNodes: () => () => {},
}));
vi.mock("../../hooks/useNodeSync", () => ({
  buildAppKindMap: () => new Map(),
}));
vi.mock("../../hooks/useWorkflowActions", () => ({
  useWorkflowActions: () => ({
    addNode: () => {},
    removeNodes: () => {},
    removeEdgesOnly: () => {},
    updateNodePositions: () => {},
    updateNode: () => {},
    addEdge: () => {},
    createGroup: () => {},
    removeGroup: () => {},
    deleteGroupWithContents: () => {},
    renameGroup: () => {},
    recolorGroup: () => {},
    addNodesToGroup: () => {},
    removeNodesFromGroup: () => {},
  }),
}));

import { CanvasView } from "./CanvasView";
import { useStore } from "../../store/useAppStore";

describe("CanvasView compact drawer layout", () => {
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

  it("removes the node palette from compact layout while the assistant drawer is open", () => {
    useStore.setState({ assistantSurface: "drawer" });

    render(<CanvasView />);

    expect(screen.getByTestId("node-palette").parentElement).toHaveClass(
      "hidden",
      "min-[1100px]:block",
    );
    expect(screen.getByTestId("assistant-panel")).toBeInTheDocument();
  });

  it("keeps the node palette visible when the assistant drawer is closed", () => {
    render(<CanvasView />);

    expect(screen.getByTestId("node-palette").parentElement).toHaveClass(
      "h-full",
      "shrink-0",
    );
    expect(screen.getByTestId("node-palette").parentElement).not.toHaveClass(
      "hidden",
    );
  });
});
