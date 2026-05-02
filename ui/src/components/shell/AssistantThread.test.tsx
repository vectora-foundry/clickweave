import { render, screen, fireEvent } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

const storeMock = vi.hoisted(() => ({
  state: {
    agentStatus: "idle" as "idle" | "running" | "complete" | "stopped" | "error",
    agentRunId: null as string | null,
    pendingApproval: null,
    completionDisagreement: null as {
      runId?: string;
      agentSummary: string;
      vlmReasoning: string;
      screenshotBase64: string;
    } | null,
    consecutiveDestructiveCapHit: null,
    setConsecutiveDestructiveCapHit: vi.fn(),
    confirmDisagreementAsComplete: vi.fn(),
    cancelDisagreement: vi.fn(),
    stopAgent: vi.fn(),
    approveAction: vi.fn(),
    rejectAction: vi.fn(),
    ambiguityResolutions: [] as unknown[],
    openAmbiguityModal: vi.fn(),
    confirmClearOpen: false,
    setConfirmClearOpen: vi.fn(),
    workflow: {
      intent: null as string | null,
      nodes: [] as unknown[],
    },
    setIntent: vi.fn(),
    executorState: "idle" as "idle" | "running",
    runTraces: {} as Record<string, unknown>,
  },
}));

vi.mock("../../store/useAppStore", () => ({
  useStore: <T,>(selector: (state: typeof storeMock.state) => T) =>
    selector(storeMock.state),
}));

import { AssistantThread } from "./AssistantThread";

describe("AssistantThread", () => {
  beforeEach(() => {
    storeMock.state.agentStatus = "idle";
    storeMock.state.agentRunId = null;
    storeMock.state.pendingApproval = null;
    storeMock.state.completionDisagreement = null;
    storeMock.state.consecutiveDestructiveCapHit = null;
    storeMock.state.ambiguityResolutions = [];
    storeMock.state.confirmClearOpen = false;
    storeMock.state.setConfirmClearOpen = vi.fn();
    storeMock.state.workflow = { intent: null, nodes: [] };
    storeMock.state.runTraces = {};
  });

  it("dispatches setConfirmClearOpen(true) when the showClearIcon trash is clicked (D14)", () => {
    const messages = [
      { role: "user" as const, content: "hello", timestamp: "2026-05-02T00:00:00Z" },
    ];
    render(
      <AssistantThread
        error={null}
        messages={messages}
        onSendMessage={() => {}}
        showHeader={false}
        showClearIcon={true}
      />,
    );
    fireEvent.click(screen.getByLabelText(/clear conversation/i));
    expect(storeMock.state.setConfirmClearOpen).toHaveBeenCalledWith(true);
  });

  it("dispatches setConfirmClearOpen(true) when the drawer-header Clear button is clicked", () => {
    const messages = [
      { role: "user" as const, content: "hello", timestamp: "2026-05-02T00:00:00Z" },
    ];
    render(
      <AssistantThread
        error={null}
        messages={messages}
        onSendMessage={() => {}}
        showHeader={true}
      />,
    );
    fireEvent.click(screen.getByRole("button", { name: /^clear$/i }));
    expect(storeMock.state.setConfirmClearOpen).toHaveBeenCalledWith(true);
  });

  it("does not mount AmbiguityResolutionModal or ConfirmClearConversationModal (P1.H1)", () => {
    const { container } = render(
      <AssistantThread
        error={null}
        messages={[]}
        onSendMessage={() => {}}
        showHeader={true}
      />,
    );
    expect(container.textContent).not.toContain("Clear conversation?");
  });

  it("wraps long assistant errors inside the thread", () => {
    const error = `Error-${"UnbrokenAssistantError".repeat(20)}`;
    render(
      <AssistantThread
        error={error}
        messages={[]}
        onSendMessage={() => {}}
        showHeader={false}
      />,
    );

    expect(screen.getByText(error)).toHaveClass("break-words");
  });

  it("wraps long completion-disagreement copy inside the thread", () => {
    const summary = `Summary-${"UnbrokenSummary".repeat(20)}`;
    const reasoning = `Reasoning-${"UnbrokenReasoning".repeat(20)}`;
    storeMock.state.completionDisagreement = {
      agentSummary: summary,
      vlmReasoning: reasoning,
      screenshotBase64: "",
    };

    render(
      <AssistantThread
        error={null}
        messages={[]}
        onSendMessage={() => {}}
        showHeader={false}
      />,
    );

    expect(screen.getByText(`Agent said: ${summary}`)).toHaveClass(
      "break-words",
    );
    expect(screen.getByText(`VLM: ${reasoning}`)).toHaveClass("break-words");
  });

  it("renders the active run trace when agentStatus is running and a runId is set", () => {
    storeMock.state.agentStatus = "running";
    storeMock.state.agentRunId = "run-1";
    storeMock.state.runTraces = {
      "run-1": {
        runId: "run-1",
        phase: "executing",
        activeSubgoal: "Open account page",
        steps: [
          {
            stepIndex: 0,
            toolName: "cdp_click",
            phase: "executing",
            body: "Clicked account",
            failed: false,
          },
        ],
        worldModelDeltas: [],
        milestones: [],
        terminalFrame: null,
      },
    };

    render(
      <AssistantThread
        error={null}
        messages={[]}
        onSendMessage={() => {}}
        showHeader={false}
      />,
    );

    expect(screen.getByText("Open account page")).toBeInTheDocument();
    expect(screen.getByText("cdp_click")).toBeInTheDocument();
    expect(screen.queryByText("Agent running...")).not.toBeInTheDocument();
  });
});
