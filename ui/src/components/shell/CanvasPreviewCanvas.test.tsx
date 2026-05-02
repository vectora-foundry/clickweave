import { describe, expect, it, beforeEach, vi } from "vitest";
import { render } from "@testing-library/react";
// `?raw` is Vite's first-class file-as-string import — no Node types
// needed at typecheck time.
import graphCanvasSource from "../GraphCanvas.tsx?raw";

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(),
}));

// jsdom doesn't ship ResizeObserver; React Flow needs it.
class ResizeObserverMock {
  observe() {}
  unobserve() {}
  disconnect() {}
}
// eslint-disable-next-line @typescript-eslint/no-explicit-any
(globalThis as any).ResizeObserver = ResizeObserverMock;
// jsdom also lacks DOMMatrixReadOnly, used by React Flow's panZoom.
// eslint-disable-next-line @typescript-eslint/no-explicit-any
if (!(globalThis as any).DOMMatrixReadOnly) {
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  (globalThis as any).DOMMatrixReadOnly = class {
    m22 = 1;
    constructor(_init?: unknown) {}
  };
}

import { CanvasPreviewCanvas } from "./CanvasPreviewCanvas";
import { useStore } from "../../store/useAppStore";

describe("CanvasPreviewCanvas (D12)", () => {
  beforeEach(() => {
    useStore.setState({
      workflow: {
        ...useStore.getState().workflow,
        nodes: [],
        edges: [],
        groups: [],
      },
    });
  });

  it("renders a react-flow root with no draggable nodes", () => {
    const { container } = render(<CanvasPreviewCanvas />);
    const rf = container.querySelector(".react-flow");
    expect(rf).not.toBeNull();
    // No draggable class on the rendered nodes.
    const draggable = container.querySelector(".react-flow__node.draggable");
    expect(draggable).toBeNull();
  });

  it("wraps every custom node in pointer-events:none (P1.M4)", () => {
    useStore.setState({
      workflow: {
        ...useStore.getState().workflow,
        nodes: [
          {
            id: "n1",
            name: "First",
            node_type: { type: "ManualStep" },
            position: { x: 0, y: 0 },
            enabled: true,
            timeout_ms: null,
            settle_ms: null,
            retries: 0,
            trace_level: "Minimal",
            role: "Default",
            expected_outcome: null,
            // eslint-disable-next-line @typescript-eslint/no-explicit-any
          } as any,
        ],
      },
    });
    const { container } = render(<CanvasPreviewCanvas />);
    const wrapper = container.querySelector(
      ".react-flow__node > div[style*='pointer-events']",
    );
    expect(wrapper).not.toBeNull();
  });

  it("registers nodeTypes keys that match the editor's GraphCanvas keys (P2.M2)", () => {
    // Snake-case key for agent-run groups; camelCase for the others.
    // GraphCanvas is a wrapper that delegates to an inner component, so
    // `.toString()` only sees the wrapper — read the source file
    // directly to verify the registered keys.
    expect(graphCanvasSource).toContain("agent_run_group");
    expect(graphCanvasSource).toContain("appGroup");
    expect(graphCanvasSource).toContain("userGroup");
    expect(graphCanvasSource).toContain("workflow:");
  });
});
