import { describe, it, expect, vi, beforeEach } from "vitest";

// Tauri's `invoke` must be mocked before agentSlice is imported — the
// slice captures the imported binding at module init time.
const invokeMock = vi.fn();
vi.mock("@tauri-apps/api/core", () => ({
  invoke: (...args: unknown[]) => invokeMock(...args),
}));

import { useStore } from "../useAppStore";

describe("agentSlice.startAgent", () => {
  beforeEach(() => {
    invokeMock.mockReset();
    useStore.getState().resetAgent();
  });

  it("preserves the active run's state when invoke rejects with AlreadyRunning", async () => {
    useStore.getState().setAgentRunId("run-prior");
    useStore.getState().setAgentStatus("running");
    useStore.getState().addAgentStep({
      summary: "prior step",
      toolName: "click",
      toolArgs: null,
      toolResult: "ok",
      pageTransitioned: false,
    });
    useStore.getState().setPendingApproval({
      stepIndex: 0,
      toolName: "click",
      arguments: {},
      description: "Click the button",
    });

    invokeMock.mockRejectedValueOnce({
      kind: "AlreadyRunning",
      message: "Already running",
    });

    await useStore.getState().startAgent("duplicate goal");

    const state = useStore.getState();
    // The rejection path must NOT wipe the live run — otherwise
    // useAgentEvents drops every subsequent event as stale.
    expect(state.agentRunId).toBe("run-prior");
    expect(state.agentStatus).toBe("running");
    expect(state.agentSteps).toHaveLength(1);
    expect(state.pendingApproval).not.toBeNull();
    expect(state.agentError).toBeNull();
    const lastLog = useStore.getState().logs.at(-1);
    expect(lastLog).toContain("Agent start rejected");
    expect(lastLog).toContain("Already running");
  });

  it("preserves the active run's state when invoke rejects with a string error", async () => {
    useStore.getState().setAgentRunId("run-prior");
    useStore.getState().setAgentStatus("running");

    invokeMock.mockRejectedValueOnce("AlreadyRunning: Already running");

    await useStore.getState().startAgent("duplicate goal");

    const state = useStore.getState();
    expect(state.agentRunId).toBe("run-prior");
    expect(state.agentStatus).toBe("running");
    const lastLog = useStore.getState().logs.at(-1);
    expect(lastLog).toMatch(/agent start rejected/i);
    expect(lastLog).toMatch(/already running/i);
  });

  it("resets run state and enters running state when invoke succeeds", async () => {
    invokeMock.mockResolvedValueOnce(undefined);

    await useStore.getState().startAgent("do something else");

    const state = useStore.getState();
    expect(state.agentStatus).toBe("running");
    expect(state.agentError).toBeNull();
    expect(state.agentGoal).toBe("do something else");
    expect(state.agentSteps).toEqual([]);
    expect(state.pendingApproval).toBeNull();
  });

  it("does not overwrite an agentRunId installed by agent://started during invoke", async () => {
    // Simulate the backend emitting agent://started (which calls
    // setAgentRunId) *before* the invoke promise resolves — the listener
    // races the continuation in useAgentEvents.
    invokeMock.mockImplementationOnce(async () => {
      useStore.getState().setAgentRunId("run-new");
    });

    await useStore.getState().startAgent("fresh goal");

    expect(useStore.getState().agentRunId).toBe("run-new");
  });

  it("sets agentStatus to running before awaiting invoke on a fresh start", async () => {
    // Early terminal events (e.g. agent://error from a fast MCP-spawn
    // failure) can arrive while invoke is still in flight. The error
    // listener gates on `agentStatus === "running"`, so the status must
    // already be "running" by the time invoke is awaited.
    let statusDuringInvoke: string | undefined;
    invokeMock.mockImplementationOnce(async () => {
      statusDuringInvoke = useStore.getState().agentStatus;
    });

    await useStore.getState().startAgent("fresh goal");

    expect(statusDuringInvoke).toBe("running");
  });

  it("clears a leftover agentRunId on a fresh start so stale events from the prior run are dropped", async () => {
    // After a run reaches a terminal state, `agentRunId` is not cleared
    // automatically — terminal event handlers leave it in place. A fresh
    // start from "complete"/"stopped"/"error" must null it out so any
    // late in-flight event from the prior run fails `isStaleRunId`
    // instead of being accepted into the new run's state.
    useStore.getState().setAgentRunId("run-prior");
    useStore.getState().setAgentStatus("complete");
    let runIdDuringInvoke: string | null | undefined;
    invokeMock.mockImplementationOnce(async () => {
      runIdDuringInvoke = useStore.getState().agentRunId;
    });

    await useStore.getState().startAgent("fresh goal");

    expect(runIdDuringInvoke).toBeNull();
  });

  it("surfaces non-AlreadyRunning rejections as agentStatus=error on a fresh start", async () => {
    invokeMock.mockRejectedValueOnce({
      kind: "Internal",
      message: "MCP binary not found",
    });

    await useStore.getState().startAgent("fresh goal");

    const state = useStore.getState();
    expect(state.agentStatus).toBe("error");
    expect(state.agentError).toBe("MCP binary not found");
    const lastLog = useStore.getState().logs.at(-1);
    expect(lastLog).toContain("Agent start rejected");
  });

  it("restores the prior run's terminal state when AlreadyRunning fires during backend cleanup", async () => {
    // Backend cleanup is async: after a terminal event the handle can
    // still be populated for a brief window, during which run_agent
    // rejects with AlreadyRunning even though the UI has already left
    // the "running" state.
    useStore.getState().setAgentRunId("run-prior");
    useStore.getState().setAgentStatus("complete");
    useStore.getState().addAgentStep({
      summary: "prior step",
      toolName: "click",
      toolArgs: null,
      toolResult: "ok",
      pageTransitioned: false,
    });

    invokeMock.mockRejectedValueOnce({
      kind: "AlreadyRunning",
      message: "Already running",
    });

    await useStore.getState().startAgent("retry goal");

    const state = useStore.getState();
    // Terminal run's history must be preserved — not converted to "error".
    expect(state.agentStatus).toBe("complete");
    expect(state.agentRunId).toBe("run-prior");
    expect(state.agentSteps).toHaveLength(1);
    expect(state.agentError).toBeNull();
  });

  it("restores the prior run's terminal state on string-serialized AlreadyRunning during cleanup", async () => {
    useStore.getState().setAgentRunId("run-prior");
    useStore.getState().setAgentStatus("stopped");

    invokeMock.mockRejectedValueOnce("AlreadyRunning: Already running");

    await useStore.getState().startAgent("retry goal");

    const state = useStore.getState();
    expect(state.agentStatus).toBe("stopped");
    expect(state.agentRunId).toBe("run-prior");
  });
});

describe("agentSlice ambiguity resolutions", () => {
  beforeEach(() => {
    invokeMock.mockReset();
    useStore.getState().clearAmbiguityResolutions();
    useStore.getState().resetAgent();
  });

  const sample = {
    id: "res-1",
    nodeId: "node-1",
    target: "Save",
    candidates: [
      {
        uid: "a1",
        snippet: '[uid="a1"] button "Save"',
        rect: { x: 1, y: 2, width: 3, height: 4 },
      },
    ],
    chosenUid: "a1",
    reasoning: "only visible candidate",
    viewportWidth: 1280,
    viewportHeight: 720,
    screenshotPath: "ambiguity_abc.png",
    screenshotBase64: "aaaa",
    createdAt: 1,
  };

  it("prepends new resolutions so the newest shows first", () => {
    useStore.getState().addAmbiguityResolution(sample);
    useStore
      .getState()
      .addAmbiguityResolution({ ...sample, id: "res-2", target: "Delete" });
    const list = useStore.getState().ambiguityResolutions;
    expect(list.map((r) => r.id)).toEqual(["res-2", "res-1"]);
  });

  it("open/close modal toggles activeAmbiguityId", () => {
    useStore.getState().addAmbiguityResolution(sample);
    useStore.getState().openAmbiguityModal("res-1");
    expect(useStore.getState().activeAmbiguityId).toBe("res-1");
    useStore.getState().closeAmbiguityModal();
    expect(useStore.getState().activeAmbiguityId).toBeNull();
  });

  it("resetAgent keeps ambiguity history intact for cross-run inspection", () => {
    useStore.getState().addAmbiguityResolution(sample);
    useStore.getState().resetAgent();
    expect(useStore.getState().ambiguityResolutions.length).toBe(1);
  });

  it("clearAmbiguityResolutions empties the list and closes the modal", () => {
    useStore.getState().addAmbiguityResolution(sample);
    useStore.getState().openAmbiguityModal("res-1");
    useStore.getState().clearAmbiguityResolutions();
    expect(useStore.getState().ambiguityResolutions).toEqual([]);
    expect(useStore.getState().activeAmbiguityId).toBeNull();
  });
});

describe("agentSlice approval actions", () => {
  beforeEach(() => {
    invokeMock.mockReset();
    useStore.getState().resetAgent();
  });

  it("formats structured Tauri errors from approveAction into the activity log", async () => {
    useStore.getState().setPendingApproval({
      stepIndex: 0,
      toolName: "click",
      arguments: {},
      description: "Click the button",
    });
    invokeMock.mockRejectedValueOnce({
      kind: "Validation",
      message: "No pending approval request",
    });

    await useStore.getState().approveAction();

    const lastLog = useStore.getState().logs.at(-1);
    expect(lastLog).toContain("No pending approval request");
    expect(lastLog).not.toContain("[object Object]");
  });

  it("formats structured Tauri errors from rejectAction into the activity log", async () => {
    useStore.getState().setPendingApproval({
      stepIndex: 0,
      toolName: "click",
      arguments: {},
      description: "Click the button",
    });
    invokeMock.mockRejectedValueOnce({
      kind: "Validation",
      message: "Approval channel closed — agent task may have ended",
    });

    await useStore.getState().rejectAction();

    const lastLog = useStore.getState().logs.at(-1);
    expect(lastLog).toContain("Approval channel closed");
    expect(lastLog).not.toContain("[object Object]");
  });
});

describe("agentSlice.confirmDisagreementAsComplete", () => {
  beforeEach(() => {
    invokeMock.mockReset();
    useStore.getState().resetAgent();
  });

  it("invokes resolve_completion_disagreement with 'confirm' and flips status to complete", async () => {
    useStore.getState().setCompletionDisagreement({
      screenshotBase64: "abc",
      vlmReasoning: "button still visible",
      agentSummary: "clicked submit",
    });
    useStore.getState().setAgentStatus("stopped");
    invokeMock.mockResolvedValueOnce(undefined);

    await useStore.getState().confirmDisagreementAsComplete();

    expect(invokeMock).toHaveBeenCalledWith("resolve_completion_disagreement", {
      action: "confirm",
    });
    const state = useStore.getState();
    expect(state.completionDisagreement).toBeNull();
    expect(state.agentStatus).toBe("complete");
  });

  it("keeps the optimistic UI state when invoke rejects (stale run tore down)", async () => {
    useStore.getState().setCompletionDisagreement({
      screenshotBase64: "abc",
      vlmReasoning: "button still visible",
      agentSummary: "clicked submit",
    });
    invokeMock.mockRejectedValueOnce({
      kind: "Validation",
      message: "No pending completion disagreement",
    });

    await useStore.getState().confirmDisagreementAsComplete();

    const state = useStore.getState();
    // Card is still dismissed and status still `complete` — the local
    // record of the user's choice outlives a lost race with cleanup.
    expect(state.completionDisagreement).toBeNull();
    expect(state.agentStatus).toBe("complete");
    const lastLog = useStore.getState().logs.at(-1);
    expect(lastLog).toContain("No pending completion disagreement");
  });
});

describe("agentSlice.cancelDisagreement", () => {
  beforeEach(() => {
    invokeMock.mockReset();
    useStore.getState().resetAgent();
  });

  it("invokes resolve_completion_disagreement with 'cancel' and flips status to stopped", async () => {
    useStore.getState().setCompletionDisagreement({
      screenshotBase64: "abc",
      vlmReasoning: "modal still visible",
      agentSummary: "clicked submit",
    });
    useStore.getState().setAgentStatus("stopped");
    invokeMock.mockResolvedValueOnce(undefined);

    await useStore.getState().cancelDisagreement();

    expect(invokeMock).toHaveBeenCalledWith("resolve_completion_disagreement", {
      action: "cancel",
    });
    const state = useStore.getState();
    expect(state.completionDisagreement).toBeNull();
    expect(state.agentStatus).toBe("stopped");
  });

  it("swallows rejection silently and still dismisses the card", async () => {
    useStore.getState().setCompletionDisagreement({
      screenshotBase64: "abc",
      vlmReasoning: "modal still visible",
      agentSummary: "clicked submit",
    });
    invokeMock.mockRejectedValueOnce({
      kind: "Validation",
      message: "No pending completion disagreement",
    });

    await useStore.getState().cancelDisagreement();

    const state = useStore.getState();
    expect(state.completionDisagreement).toBeNull();
    expect(state.agentStatus).toBe("stopped");
  });
});
