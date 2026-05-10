import { describe, expect, it, beforeEach, afterEach, vi } from "vitest";
import { act, cleanup, render, screen } from "@testing-library/react";

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(),
}));

import { LiveRuntimeCard } from "./LiveRuntimeCard";
import { useStore } from "../../store/useAppStore";

describe("LiveRuntimeCard elapsed (D24)", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    useStore.setState({
      agentStatus: "idle",
      completionDisagreement: null,
      pendingApproval: null,
      agentRunId: null,
      runTraces: {},
      agentRunStartedAt: null,
      agentRunFinishedAt: null,
      lastRunStatus: null,
    });
  });

  afterEach(() => {
    cleanup();
    vi.useRealTimers();
  });

  it("renders an em-dash for elapsed when no run has started", () => {
    render(<LiveRuntimeCard />);
    // The Elapsed cell is the only mono Stat that shows "—" in the
    // initial state (Phase / Active Tool show "—" but as non-mono).
    const monoDashes = screen
      .getAllByText("—")
      .filter((el) => el.classList.contains("font-mono"));
    expect(monoDashes.length).toBeGreaterThan(0);
  });

  it("shows the pending approval tool before the last completed tool", () => {
    useStore.setState({
      agentStatus: "running",
      pendingApproval: {
        scope: null,
        toolName: "approve_next_tool",
        arguments: {},
        description: "Approve next tool",
      },
      agentRunId: "run-1",
      runTraces: {
        "run-1": {
          runId: "run-1",
          phase: "executing",
          activeSubgoal: "",
          steps: [
            {
              stepIndex: 1,
              toolName: "previous_tool",
              phase: "executing",
              body: "{}",
              failed: false,
            },
          ],
          worldModelDeltas: [],
          milestones: [],
          terminalFrame: null,
        },
      },
    });

    render(<LiveRuntimeCard />);

    const activeToolStat = screen.getByText("Active Tool").parentElement;
    expect(activeToolStat?.textContent).toContain("approve_next_tool");
    expect(activeToolStat?.textContent).not.toContain("previous_tool");
  });

  it("ticks elapsed while live and freezes when finished (D24)", () => {
    const t0 = 1_000_000_000_000;
    vi.setSystemTime(t0);
    useStore.setState({
      agentStatus: "running",
      agentRunId: "run-1",
      agentRunStartedAt: t0,
      agentRunFinishedAt: null,
    });
    render(<LiveRuntimeCard />);

    // `advanceTimersByTime` advances the system clock as well, so do
    // NOT call `setSystemTime` separately or the elapsed will double.
    act(() => {
      vi.advanceTimersByTime(5000);
    });
    const monoText = () =>
      screen
        .getAllByText(/^\d+:\d{2}$|^—$/)
        .filter((el) => el.classList.contains("font-mono"))
        .map((el) => el.textContent);
    expect(monoText()).toContain("0:05");

    // Freeze when terminal arrives. Stamp a finishedAt and let real
    // time keep advancing; the displayed Elapsed must stay at the
    // freeze duration (not the latest now-startedAt).
    act(() => {
      useStore.setState({
        agentStatus: "stopped",
        agentRunFinishedAt: t0 + 8000,
      });
      vi.advanceTimersByTime(60000);
    });
    expect(monoText()).toContain("0:08");
  });

  it("resets to em-dash when both timestamp fields are nulled (clearConversationFlow)", () => {
    useStore.setState({
      agentRunStartedAt: 1,
      agentRunFinishedAt: 2,
      agentStatus: "stopped",
    });
    const { rerender } = render(<LiveRuntimeCard />);
    // Restrict to Stat-cell text (`div.font-mono`) so we don't
    // accidentally read the header pill (which is a `<span class="font-mono">`).
    const monoTextOf = (root: HTMLElement) =>
      Array.from(root.querySelectorAll("div.font-mono")).map(
        (el) => el.textContent,
      );
    expect(monoTextOf(document.body)).toContain("0:00");

    useStore.setState({ agentRunStartedAt: null, agentRunFinishedAt: null });
    rerender(<LiveRuntimeCard />);
    expect(monoTextOf(document.body)).toContain("—");
  });

  it("clears elapsed on the next startAgent (next-start zeroing contract)", () => {
    const tStart = 100;
    const tFinish = 5_100;
    useStore.setState({
      agentStatus: "stopped",
      agentRunStartedAt: tStart,
      agentRunFinishedAt: tFinish,
    });
    const { rerender } = render(<LiveRuntimeCard />);
    // Restrict to Stat-cell text (`div.font-mono`) so we don't
    // accidentally read the header pill (which is a `<span class="font-mono">`).
    const monoTextOf = (root: HTMLElement) =>
      Array.from(root.querySelectorAll("div.font-mono")).map(
        (el) => el.textContent,
      );
    expect(monoTextOf(document.body)).toContain("0:05");

    // Simulate the next startAgent: both fields reset together to a
    // fresh stamp / null. Elapsed should jump to "0:00" or near it.
    const tNewStart = 1_000_000_000_000;
    vi.setSystemTime(tNewStart);
    useStore.setState({
      agentStatus: "running",
      agentRunStartedAt: tNewStart,
      agentRunFinishedAt: null,
    });
    rerender(<LiveRuntimeCard />);
    expect(monoTextOf(document.body)).toContain("0:00");
  });
});
