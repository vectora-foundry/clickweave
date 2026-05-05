import { describe, expect, it, beforeEach, vi } from "vitest";
import { render, screen, waitFor } from "@testing-library/react";

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(),
}));

class ResizeObserverMock {
  observe() {}
  unobserve() {}
  disconnect() {}
}
// eslint-disable-next-line @typescript-eslint/no-explicit-any
(globalThis as any).ResizeObserver = ResizeObserverMock;
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

describe("CanvasPreviewCanvas (trace preview)", () => {
  beforeEach(() => {
    useStore.setState({ agentRunId: null, runTraces: {} });
  });

  it("shows an empty-state message when no run is active", () => {
    render(<CanvasPreviewCanvas />);
    expect(screen.getByText(/No active run yet/i)).toBeInTheDocument();
  });

  it("renders trace step tiles for the active run", async () => {
    useStore.setState({
      agentRunId: "run-1",
      runTraces: {
        "run-1": {
          runId: "run-1",
          phase: "executing",
          activeSubgoal: "Click button",
          steps: [
            {
              stepIndex: 0,
              toolName: "click",
              phase: "executing",
              body: "Clicked at (10,20)",
              failed: false,
            },
            {
              stepIndex: 1,
              toolName: "type_text",
              phase: "executing",
              body: "typed 'hi'",
              failed: false,
            },
          ],
          worldModelDeltas: [],
          milestones: [],
          terminalFrame: null,
        },
      },
    });

    render(<CanvasPreviewCanvas />);

    await waitFor(() => {
      expect(screen.getByText("click")).toBeInTheDocument();
      expect(screen.getByText("type_text")).toBeInTheDocument();
    });
  });

  it("renders the terminal node alone for runs that ended before any step", async () => {
    useStore.setState({
      agentRunId: "run-3",
      runTraces: {
        "run-3": {
          runId: "run-3",
          phase: "exploring",
          activeSubgoal: "",
          steps: [],
          worldModelDeltas: [],
          milestones: [],
          terminalFrame: { kind: "error", detail: "MCP spawn failed" },
        },
      },
    });

    render(<CanvasPreviewCanvas />);

    await waitFor(() => {
      expect(screen.getByText("Error")).toBeInTheDocument();
      expect(screen.getByText(/MCP spawn failed/)).toBeInTheDocument();
    });
    expect(screen.queryByText(/No active run/i)).not.toBeInTheDocument();
  });

  it("renders the terminal node when the run has finished", async () => {
    useStore.setState({
      agentRunId: "run-2",
      runTraces: {
        "run-2": {
          runId: "run-2",
          phase: "executing",
          activeSubgoal: "",
          steps: [
            {
              stepIndex: 0,
              toolName: "click",
              phase: "executing",
              body: "ok",
              failed: false,
            },
          ],
          worldModelDeltas: [],
          milestones: [],
          terminalFrame: { kind: "complete", detail: "Goal completed." },
        },
      },
    });

    render(<CanvasPreviewCanvas />);

    await waitFor(() => {
      expect(screen.getByText("Complete")).toBeInTheDocument();
    });
  });
});
