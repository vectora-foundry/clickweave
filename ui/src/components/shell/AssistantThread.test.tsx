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
    // 1.J.2 freeze fields
    skillFrozen: false,
    stopWorkflow: vi.fn(async () => {}),
    failedSectionId: null as string | null,
    failedSectionError: null as string | null,
    selectedSkill: null as null | { sections?: Array<{ id: string; heading: string }> },
  },
}));

vi.mock("../../store/useAppStore", () => ({
  useStore: <T,>(selector: (state: typeof storeMock.state) => T) =>
    selector(storeMock.state),
}));

import { AssistantThread, isEditShaped, parseStopAndThen } from "./AssistantThread";

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
    storeMock.state.skillFrozen = false;
    storeMock.state.stopWorkflow = vi.fn(async () => {});
    storeMock.state.failedSectionId = null;
    storeMock.state.failedSectionError = null;
    storeMock.state.selectedSkill = null;
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

// ── 1.J.2: Freeze — isEditShaped and parseStopAndThen pure helpers ──────────

describe("isEditShaped — 1.J.2", () => {
  // (c) edit-shaped input refused with one-liner
  it("returns true for typical edit imperatives", () => {
    expect(isEditShaped("edit section 2")).toBe(true);
    expect(isEditShaped("change the second step")).toBe(true);
    expect(isEditShaped("update the click target")).toBe(true);
    expect(isEditShaped("delete step 3")).toBe(true);
    expect(isEditShaped("remove the login step")).toBe(true);
    expect(isEditShaped("rename the skill")).toBe(true);
  });

  it("returns false for non-edit messages", () => {
    expect(isEditShaped("what does step 3 do?")).toBe(false);
    expect(isEditShaped("how long will this run take?")).toBe(false);
    expect(isEditShaped("stop the run")).toBe(false);
  });
});

describe("parseStopAndThen — 1.J.2", () => {
  // (d) `stop and then ...` parses and dispatches
  it("matches 'stop and then <remainder>'", () => {
    const r = parseStopAndThen("stop and then edit step 2");
    expect(r.isStopAndThen).toBe(true);
    if (r.isStopAndThen) expect(r.remainder).toBe("edit step 2");
  });

  it("matches 'stop, then <remainder>'", () => {
    const r = parseStopAndThen("stop, then remove step 1");
    expect(r.isStopAndThen).toBe(true);
    if (r.isStopAndThen) expect(r.remainder).toBe("remove step 1");
  });

  it("returns false for plain stop message", () => {
    const r = parseStopAndThen("stop");
    expect(r.isStopAndThen).toBe(false);
  });

  it("returns false for non-stop messages", () => {
    const r = parseStopAndThen("edit section 1");
    expect(r.isStopAndThen).toBe(false);
  });
});

// ── 1.J.2: Freeze — chat pill flip via SkillSelectionContext ─────────────────

// (b) Chat pill flips to "Inspecting" when frozen is tested via the SkillSelectionContext
// which is covered in SkillSelectionContext.test.tsx. The uiSlice.skillFrozen state
// is set/cleared by executor events tested via the executorNodeEvents hook.

// ── 1.J.2: Freeze — AssistantThread integration ──────────────────────────────

describe("AssistantThread — freeze behavior (1.J.2)", () => {
  beforeEach(() => {
    storeMock.state.agentStatus = "idle";
    storeMock.state.skillFrozen = true;
    storeMock.state.stopWorkflow = vi.fn(async () => {});
    storeMock.state.failedSectionId = null;
    storeMock.state.failedSectionError = null;
    storeMock.state.selectedSkill = null;
  });

  // (c) edit-shaped input is refused with one-liner when skill is frozen
  it("refuses edit-shaped input with one-liner and calls onSendMessage with the refusal", () => {
    const onSend = vi.fn();
    render(
      <AssistantThread
        error={null}
        messages={[]}
        onSendMessage={onSend}
        showHeader={false}
      />,
    );
    const textarea = screen.getByPlaceholderText(/ask about/i);
    fireEvent.change(textarea, { target: { value: "edit section 2" } });
    fireEvent.keyDown(textarea, { key: "Enter" });
    expect(onSend).toHaveBeenCalledWith(
      expect.stringContaining("Run is in progress"),
    );
  });

  // (d) `stop and then X` dispatches stopWorkflow then onSendMessage with remainder
  it("dispatches stopWorkflow on 'stop and then' compound command", () => {
    const onSend = vi.fn();
    render(
      <AssistantThread
        error={null}
        messages={[]}
        onSendMessage={onSend}
        showHeader={false}
      />,
    );
    const textarea = screen.getByPlaceholderText(/ask about/i);
    fireEvent.change(textarea, { target: { value: "stop and then edit step 1" } });
    fireEvent.keyDown(textarea, { key: "Enter" });
    expect(storeMock.state.stopWorkflow).toHaveBeenCalled();
  });
});
