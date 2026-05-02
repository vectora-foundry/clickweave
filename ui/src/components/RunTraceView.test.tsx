import { render, screen } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { RunTrace } from "../store/slices/assistantSlice";

const storeMock = vi.hoisted(() => ({
  state: {
    runTraces: {} as Record<string, RunTrace>,
  },
}));

vi.mock("../store/useAppStore", () => ({
  useStore: <T,>(selector: (state: typeof storeMock.state) => T) =>
    selector(storeMock.state),
}));

import { RunTraceView } from "./RunTraceView";

function trace(overrides: Partial<RunTrace> = {}): RunTrace {
  return {
    runId: "run-1",
    phase: "executing",
    activeSubgoal: "Submit the form",
    steps: [
      {
        stepIndex: 1,
        toolName: "cdp_click",
        phase: "executing",
        body: "Clicked Submit",
        failed: false,
      },
    ],
    worldModelDeltas: [
      {
        stepIndex: 1,
        changedFields: ["elements", "cdp_page"],
      },
    ],
    milestones: [
      {
        stepIndex: 0,
        kind: "subgoal_completed",
        text: "Login form opened",
      },
      {
        stepIndex: 2,
        kind: "recovery_succeeded",
        text: "Recovery succeeded",
      },
    ],
    terminalFrame: {
      kind: "complete",
      detail: "Done",
    },
    ...overrides,
  };
}

describe("RunTraceView", () => {
  beforeEach(() => {
    storeMock.state.runTraces = {};
  });

  it("renders a fallback before trace events arrive", () => {
    render(<RunTraceView runId="run-1" />);

    expect(screen.getByText("Agent running...")).toBeInTheDocument();
  });

  it("renders phase, active subgoal, steps, milestones, deltas, and terminal frame", () => {
    storeMock.state.runTraces["run-1"] = trace();

    render(<RunTraceView runId="run-1" />);

    expect(screen.getAllByText("Executing").length).toBeGreaterThan(0);
    expect(screen.getByText("Submit the form")).toBeInTheDocument();
    expect(screen.getByText("cdp_click")).toBeInTheDocument();
    expect(screen.getByText("Clicked Submit")).toBeInTheDocument();
    expect(screen.getByText("Login form opened")).toBeInTheDocument();
    expect(screen.getByText("Recovery succeeded")).toBeInTheDocument();
    expect(screen.getByText("elements, cdp_page")).toBeInTheDocument();
    expect(screen.getByText("Complete")).toBeInTheDocument();
    expect(screen.getByText("Done")).toBeInTheDocument();
  });

  it.each([
    ["exploring", "Exploring"],
    ["executing", "Executing"],
    ["recovering", "Recovering"],
  ] as const)("renders the %s phase chip", (phase, label) => {
    storeMock.state.runTraces["run-1"] = trace({ phase });

    render(<RunTraceView runId="run-1" />);

    expect(screen.getAllByText(label).length).toBeGreaterThan(0);
  });

  it.each([
    ["complete", "Complete"],
    ["stopped", "Stopped"],
    ["error", "Error"],
    ["disagreement_cancelled", "Cancelled"],
  ] as const)("renders the %s terminal frame", (kind, label) => {
    storeMock.state.runTraces["run-1"] = trace({
      terminalFrame: { kind, detail: "terminal detail" },
    });

    render(<RunTraceView runId="run-1" />);

    expect(screen.getByText(label)).toBeInTheDocument();
    expect(screen.getByText("terminal detail")).toBeInTheDocument();
  });

  it("contains long terminal details inside the trace card", () => {
    const detail = `Terminal-${"UnbrokenDetail".repeat(20)}`;
    storeMock.state.runTraces["run-1"] = trace({
      terminalFrame: { kind: "error", detail },
    });

    render(<RunTraceView runId="run-1" />);

    expect(screen.getByText(detail)).toHaveClass("break-words");
  });
});
