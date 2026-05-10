import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { OverviewAssistantCard } from "./OverviewAssistantCard";
import { useStore } from "../../store/useAppStore";

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(),
}));

describe("OverviewAssistantCard", () => {
  const originalStartAgent = useStore.getState().startAgent;
  const originalCancelWalkthrough = useStore.getState().cancelWalkthrough;

  beforeEach(() => {
    useStore.setState({
      assistantError: null,
      messages: [],
      agentStatus: "idle",
      completionDisagreement: null,
      agentRunId: null,
      runTraces: {},
      walkthroughStatus: "Idle",
      startAgent: originalStartAgent,
      cancelWalkthrough: originalCancelWalkthrough,
      projectIntent: null,
    });
  });

  it("truncates long current-goal text inside the card", () => {
    const longGoal = `Goal-${"UnbrokenLongIntent".repeat(20)}`;
    useStore.setState({ projectIntent: longGoal });

    render(<OverviewAssistantCard />);

    expect(screen.getByTitle(longGoal)).toHaveClass("min-w-0", "truncate");
  });

  it("cancels an active walkthrough before starting an overview assistant run", async () => {
    let resolveCancel!: () => void;
    const cancelWalkthrough = vi.fn(
      () =>
        new Promise<void>((resolve) => {
          resolveCancel = resolve;
        }),
    );
    const startAgent = vi.fn();
    useStore.setState({
      walkthroughStatus: "Recording",
      cancelWalkthrough,
      startAgent: startAgent as unknown as typeof originalStartAgent,
    });

    render(<OverviewAssistantCard />);

    fireEvent.change(screen.getByPlaceholderText(/ask about your workflow/i), {
      target: { value: "Build the login workflow" },
    });
    fireEvent.click(screen.getByRole("button", { name: /send/i }));

    expect(cancelWalkthrough).toHaveBeenCalledTimes(1);
    expect(startAgent).not.toHaveBeenCalled();
    expect(screen.getByRole("button", { name: /send/i })).toBeDisabled();

    resolveCancel();

    await waitFor(() =>
      expect(startAgent).toHaveBeenCalledWith("Build the login workflow"),
    );
  });
});
