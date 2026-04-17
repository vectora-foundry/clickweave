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

  it("replaces a leftover agentRunId on a fresh start so stale events from the prior run are dropped", async () => {
    // After a run reaches a terminal state, `agentRunId` is not cleared
    // automatically — terminal event handlers leave it in place. A fresh
    // start from "complete"/"stopped"/"error" must install the new
    // run's ID (client-generated, per D1.M1) before the invoke so any
    // late in-flight event from the prior run fails `isStaleRunId`
    // instead of being accepted into the new run's state.
    useStore.getState().setAgentRunId("run-prior");
    useStore.getState().setAgentStatus("complete");
    let runIdDuringInvoke: string | null | undefined;
    invokeMock.mockImplementationOnce(async () => {
      runIdDuringInvoke = useStore.getState().agentRunId;
    });

    await useStore.getState().startAgent("fresh goal");

    expect(runIdDuringInvoke).not.toBe("run-prior");
    expect(typeof runIdDuringInvoke).toBe("string");
    expect(runIdDuringInvoke?.length).toBeGreaterThan(0);
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

  it("invokes resolve_completion_disagreement with 'confirm' without clearing the card locally", async () => {
    // The card is now cleared by the terminal `agent://complete`
    // event handler (see useAgentEvents) — not by this action —
    // so the `isAgentActive` gates stay armed until the backend
    // task finishes its final cache/variant-index writes.
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
    // Card stays — terminal event clears it.
    expect(state.completionDisagreement).not.toBeNull();
    // Status is unchanged — the backend's terminal event is the
    // authoritative status transition.
    expect(state.agentStatus).toBe("stopped");
  });

  it("dismisses the card when the backend rejects the confirm (run already torn down)", async () => {
    useStore.getState().setCompletionDisagreement({
      screenshotBase64: "abc",
      vlmReasoning: "button still visible",
      agentSummary: "clicked submit",
    });
    // The disagreement card is rendered while status is `stopped`
    // (see useAgentEvents: `agent://completion_disagreement` calls
    // `setStatusIfActive("stopped")`).
    useStore.getState().setAgentStatus("stopped");
    invokeMock.mockRejectedValueOnce({
      kind: "Validation",
      message: "No pending completion disagreement",
    });

    await useStore.getState().confirmDisagreementAsComplete();

    const state = useStore.getState();
    // Resolver is gone — the card is dropped so the user isn't
    // re-prompted in a never-going-to-end state.
    expect(state.completionDisagreement).toBeNull();
    expect(state.agentStatus).toBe("stopped");
    const lastLog = useStore.getState().logs.at(-1);
    expect(lastLog).toContain("No pending completion disagreement");
  });
});

describe("agentSlice.cancelDisagreement", () => {
  beforeEach(() => {
    invokeMock.mockReset();
    useStore.getState().resetAgent();
  });

  it("invokes resolve_completion_disagreement with 'cancel' without clearing the card locally", async () => {
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
    // Card stays — terminal `agent://stopped` event clears it.
    expect(state.completionDisagreement).not.toBeNull();
    expect(state.agentStatus).toBe("stopped");
  });

  it("dismisses the card when the backend rejects the cancel (run already torn down)", async () => {
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


describe("confirmDisagreementAsComplete — active-gate preservation", () => {
  beforeEach(() => {
    invokeMock.mockReset();
    useStore.getState().resetAgent();
  });

  it("keeps completionDisagreement non-null across the resolver return so the terminal event can clear it", async () => {
    useStore.getState().setCompletionDisagreement({
      screenshotBase64: "abc",
      vlmReasoning: "r",
      agentSummary: "s",
    });
    useStore.getState().setAgentStatus("stopped");

    invokeMock.mockResolvedValueOnce(undefined);

    await useStore.getState().confirmDisagreementAsComplete();

    // Card is STILL set — `resolve_completion_disagreement` only
    // forwards the operator choice into the pending oneshot; the
    // original agent task keeps running until its final
    // cache/variant-index/terminal-event work completes. The
    // terminal `agent://complete` handler in useAgentEvents is the
    // one that finally drops the card, so `isAgentActive` stays
    // true from Confirm until the backend is actually done.
    expect(useStore.getState().completionDisagreement).not.toBeNull();
  });
});

describe("cancelDisagreement — active-gate preservation", () => {
  beforeEach(() => {
    invokeMock.mockReset();
    useStore.getState().resetAgent();
  });

  it("keeps completionDisagreement non-null across the resolver return so the terminal event can clear it", async () => {
    useStore.getState().setCompletionDisagreement({
      screenshotBase64: "abc",
      vlmReasoning: "r",
      agentSummary: "s",
    });
    useStore.getState().setAgentStatus("stopped");

    invokeMock.mockResolvedValueOnce(undefined);

    await useStore.getState().cancelDisagreement();

    expect(useStore.getState().completionDisagreement).not.toBeNull();
  });
});

describe("stopAgent — preserves completionDisagreement for terminal event", () => {
  beforeEach(() => {
    invokeMock.mockReset();
    useStore.getState().resetAgent();
  });

  it("does not clear completionDisagreement when a disagreement is pending", async () => {
    useStore.getState().setCompletionDisagreement({
      screenshotBase64: "abc",
      vlmReasoning: "r",
      agentSummary: "s",
    });
    invokeMock.mockResolvedValueOnce(undefined);

    await useStore.getState().stopAgent();

    // Stop is fire-and-forget on the backend (`force_stop` returns
    // before the agent task finishes cleanup). The disagreement
    // card stays visible until the terminal `agent://stopped`
    // event arrives, so `isAgentActive` gates stay armed until
    // the backend task is actually done.
    expect(useStore.getState().completionDisagreement).not.toBeNull();
    expect(useStore.getState().agentStatus).toBe("stopped");
  });

  it("still clears pendingApproval so the engine oneshot unblocks", async () => {
    useStore.getState().setPendingApproval({
      stepIndex: 0,
      toolName: "click",
      arguments: {},
      description: "",
    });
    invokeMock.mockResolvedValueOnce(undefined);

    await useStore.getState().stopAgent();

    expect(useStore.getState().pendingApproval).toBeNull();
  });
});

describe("startAgent — blocked during pending completion-disagreement", () => {
  beforeEach(() => {
    invokeMock.mockReset();
    useStore.getState().resetAgent();
    useStore.setState({ messages: [] });
  });

  it("does not optimistically reset state when a disagreement resolver is pending", async () => {
    // Simulate: agent emitted disagreement, UI flipped status to
    // "stopped" per useAgentEvents. The backend task is still alive
    // waiting on `resolve_completion_disagreement`. A user now tries
    // to send a new prompt — `startAgent` must treat this as "wasActive"
    // to avoid wiping the live run's state and leaving a stray user
    // bubble behind.
    useStore.getState().setAgentRunId("run-prior");
    useStore.getState().setAgentStatus("stopped");
    useStore.getState().setCompletionDisagreement({
      screenshotBase64: "",
      vlmReasoning: "",
      agentSummary: "",
    });
    invokeMock.mockRejectedValueOnce({
      kind: "AlreadyRunning",
      message: "Already running",
    });

    await useStore.getState().startAgent("follow-up prompt");

    const state = useStore.getState();
    expect(state.agentRunId).toBe("run-prior");
    expect(state.completionDisagreement).not.toBeNull();
    // Most importantly: no stray user bubble was pushed.
    expect(state.messages).toEqual([]);
  });
});

describe("isAgentActive", () => {
  it("is true when status is running", async () => {
    const { isAgentActive } = await import("./agentSlice");
    expect(isAgentActive("running", null)).toBe(true);
  });

  it("is true when a completion-disagreement resolver is pending, even if status is stopped", async () => {
    const { isAgentActive } = await import("./agentSlice");
    expect(
      isAgentActive("stopped", {
        screenshotBase64: "",
        vlmReasoning: "",
        agentSummary: "",
      }),
    ).toBe(true);
  });

  it("is false when idle with no pending disagreement", async () => {
    const { isAgentActive } = await import("./agentSlice");
    expect(isAgentActive("idle", null)).toBe(false);
    expect(isAgentActive("complete", null)).toBe(false);
    expect(isAgentActive("stopped", null)).toBe(false);
    expect(isAgentActive("error", null)).toBe(false);
  });
});
