import { render, screen } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { OverviewAssistantCard } from "./OverviewAssistantCard";
import { useStore } from "../../store/useAppStore";

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(),
}));

describe("OverviewAssistantCard", () => {
  beforeEach(() => {
    useStore.setState({
      assistantError: null,
      messages: [],
      agentStatus: "idle",
      completionDisagreement: null,
      agentRunId: null,
      runTraces: {},
      workflow: {
        ...useStore.getState().workflow,
        intent: null,
      },
    });
  });

  it("truncates long current-goal text inside the card", () => {
    const longGoal = `Goal-${"UnbrokenLongIntent".repeat(20)}`;
    useStore.setState({
      workflow: {
        ...useStore.getState().workflow,
        intent: longGoal,
      },
    });

    render(<OverviewAssistantCard />);

    expect(screen.getByTitle(longGoal)).toHaveClass("min-w-0", "truncate");
  });
});
