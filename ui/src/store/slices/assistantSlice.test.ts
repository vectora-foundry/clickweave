import { describe, it, expect, vi, beforeEach } from "vitest";

// `invoke` is pulled in transitively by the composed store; mock before import.
vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(),
}));

// Partial-mock the generated bindings: proxy so unrelated slices get
// their passthrough commands while we observe the ones we care about.
// The proxy caches per-prop fns so repeat reads return the SAME mock —
// required for `expect(commands.foo).toHaveBeenCalled()` to observe
// calls made through separately-imported references.
vi.mock("../../bindings", async () => {
  const clearAgentConversation = vi.fn(async () => undefined);
  const saveAgentChat = vi.fn(async () => undefined);
  const loadAgentChat = vi.fn(async () => ({ status: "ok", data: { messages: [] } }));
  const explicit: Record<string, unknown> = {
    clearAgentConversation,
    saveAgentChat,
    loadAgentChat,
  };
  const cache = new Map<string | symbol, unknown>();
  return {
    commands: new Proxy(explicit, {
      get(target, prop) {
        if (prop in target) return target[prop as string];
        if (!cache.has(prop)) cache.set(prop, vi.fn(async () => undefined));
        return cache.get(prop);
      },
    }),
  };
});

import { useStore } from "../useAppStore";
import { commands } from "../../bindings";

describe("assistantSlice.setAssistantOpen / toggleAssistant (D21)", () => {
  beforeEach(() => {
    useStore.setState({
      assistantSurface: null,
      currentView: "canvas",
      walkthroughStatus: "Idle",
    });
  });

  it("setAssistantOpen(true) opens the drawer surface on Canvas", () => {
    useStore.getState().setAssistantOpen(true);
    expect(useStore.getState().assistantSurface).toBe("drawer");
  });

  it("setAssistantOpen(false) closes the drawer surface on Canvas", () => {
    useStore.setState({ assistantSurface: "drawer" });
    useStore.getState().setAssistantOpen(false);
    expect(useStore.getState().assistantSurface).toBeNull();
  });

  it("setAssistantOpen(true) is a no-op on Overview", () => {
    useStore.setState({ currentView: "overview", assistantSurface: null });
    useStore.getState().setAssistantOpen(true);
    expect(useStore.getState().assistantSurface).toBeNull();
  });

  it("toggleAssistant flips between drawer and null on Canvas", () => {
    useStore.getState().toggleAssistant();
    expect(useStore.getState().assistantSurface).toBe("drawer");
    useStore.getState().toggleAssistant();
    expect(useStore.getState().assistantSurface).toBeNull();
  });

  it("toggleAssistant is a no-op on Overview", () => {
    useStore.setState({ currentView: "overview", assistantSurface: null });
    useStore.getState().toggleAssistant();
    expect(useStore.getState().assistantSurface).toBeNull();
  });

  it("setAssistantOpen(true) on Canvas does NOT cancel a Review walkthrough", () => {
    useStore.setState({ currentView: "canvas", walkthroughStatus: "Review" });
    useStore.getState().setAssistantOpen(true);
    expect(useStore.getState().walkthroughStatus).toBe("Review");
    expect(useStore.getState().assistantSurface).toBe("drawer");
  });

  it("setAssistantOpen(true) on Canvas cancels a Recording walkthrough", () => {
    const cancelSpy = vi.fn();
    useStore.setState({
      currentView: "canvas",
      walkthroughStatus: "Recording",
      cancelWalkthrough: cancelSpy as unknown as () => Promise<void>,
    });
    useStore.getState().setAssistantOpen(true);
    expect(cancelSpy).toHaveBeenCalled();
  });

  it("setCurrentView('overview') from a Recording walkthrough does NOT cancel it", () => {
    const cancelSpy = vi.fn();
    useStore.setState({
      currentView: "canvas",
      walkthroughStatus: "Recording",
      assistantSurface: null,
      cancelWalkthrough: cancelSpy as unknown as () => Promise<void>,
    });
    useStore.getState().setCurrentView("overview");
    expect(useStore.getState().walkthroughStatus).toBe("Recording");
    expect(cancelSpy).not.toHaveBeenCalled();
  });
});

describe("assistantSlice.pushAssistantMessage", () => {
  beforeEach(() => {
    useStore.setState({ messages: [] });
  });

  it("appends a user message with role/content/timestamp", () => {
    useStore.getState().pushAssistantMessage("user", "hello");
    const msgs = useStore.getState().messages;
    expect(msgs).toHaveLength(1);
    expect(msgs[0].role).toBe("user");
    expect(msgs[0].content).toBe("hello");
    expect(typeof msgs[0].timestamp).toBe("string");
    expect(msgs[0].timestamp.length).toBeGreaterThan(0);
  });

  it("appends assistant messages in order", () => {
    useStore.getState().pushAssistantMessage("user", "first");
    useStore.getState().pushAssistantMessage("assistant", "second");
    const msgs = useStore.getState().messages;
    expect(msgs.map((m) => m.content)).toEqual(["first", "second"]);
    expect(msgs.map((m) => m.role)).toEqual(["user", "assistant"]);
  });

  it("trims whitespace and ignores empty content", () => {
    useStore.getState().pushAssistantMessage("user", "  padded  ");
    useStore.getState().pushAssistantMessage("user", "   ");
    useStore.getState().pushAssistantMessage("user", "");
    const msgs = useStore.getState().messages;
    expect(msgs).toHaveLength(1);
    expect(msgs[0].content).toBe("padded");
  });
});

describe("AssistantMessage extensions", () => {
  beforeEach(() => {
    useStore.setState({ messages: [] });
  });

  it("accepts the system role via pushSystemAnnotation", () => {
    useStore
      .getState()
      .pushSystemAnnotation('Deleted 3 nodes from "Send test"');
    const msgs = useStore.getState().messages;
    expect(msgs).toHaveLength(1);
    expect(msgs[0].role).toBe("system");
    expect(msgs[0].runId).toBeUndefined();
  });

  it("tags user messages with the provided runId", () => {
    useStore
      .getState()
      .pushAssistantMessage("user", "hello", "11111111-1111-1111-1111-111111111111");
    const msg = useStore.getState().messages[0];
    expect(msg.runId).toBe("11111111-1111-1111-1111-111111111111");
  });

  it("clearConversation empties the messages array", () => {
    useStore.getState().pushAssistantMessage("user", "a");
    useStore.getState().pushAssistantMessage("assistant", "b");
    useStore.getState().clearConversation();
    expect(useStore.getState().messages).toEqual([]);
  });

  it("setMessages replaces the full array", () => {
    useStore.getState().pushAssistantMessage("user", "keep me?");
    useStore.getState().setMessages([
      { role: "user", content: "hydrated", timestamp: "t1", runId: "r1" },
    ]);
    const msgs = useStore.getState().messages;
    expect(msgs).toHaveLength(1);
    expect(msgs[0].content).toBe("hydrated");
    expect(msgs[0].runId).toBe("r1");
  });

  it("mapMessagesByRunIds updates only matching messages", () => {
    useStore.getState().pushAssistantMessage("user", "goal", "r1");
    useStore.getState().pushAssistantMessage("assistant", "summary", "r1");
    useStore.getState().pushAssistantMessage("user", "other", "r2");
    useStore.getState().mapMessagesByRunIds(new Set(["r1"]), (m) =>
      m.role === "assistant" ? { ...m, content: "(partially deleted)" } : m,
    );
    const assistants = useStore
      .getState()
      .messages.filter((m) => m.role === "assistant");
    expect(assistants[0].content).toBe("(partially deleted)");
    const otherUser = useStore
      .getState()
      .messages.find((m) => m.runId === "r2");
    expect(otherUser?.content).toBe("other");
  });

  it("dropTurnsByRunIds removes user/assistant messages but keeps system annotations", () => {
    useStore.getState().pushAssistantMessage("user", "goal", "r1");
    useStore.getState().pushAssistantMessage("assistant", "summary", "r1");
    useStore.getState().pushSystemAnnotation("kept note");
    useStore.getState().pushAssistantMessage("user", "other", "r2");
    useStore.getState().dropTurnsByRunIds(new Set(["r1"]));
    const msgs = useStore.getState().messages;
    expect(msgs.some((m) => m.runId === "r1")).toBe(false);
    expect(msgs.some((m) => m.role === "system")).toBe(true);
    expect(msgs.some((m) => m.runId === "r2")).toBe(true);
  });
});

describe("RunTrace reducers", () => {
  beforeEach(() => {
    useStore.setState({ runTraces: {} });
  });

  it("applyTaskStateUpdate initializes trace phase and active subgoal", () => {
    useStore.getState().applyTaskStateUpdate("run-1", {
      goal: "goal",
      phase: "executing",
      subgoal_stack: [
        {
          id: "subgoal-1",
          text: "Open settings",
          pushed_at_step: 0,
          parent: null,
        },
      ],
      watch_slots: [],
      hypotheses: [],
      milestones: [],
    });

    const trace = useStore.getState().runTraces["run-1"];
    expect(trace.phase).toBe("executing");
    expect(trace.activeSubgoal).toBe("Open settings");
    expect(trace.steps).toEqual([]);
  });

  it("applyWorldModelDelta records changed fields at the next step index", () => {
    useStore.getState().pushTraceStep("run-1", {
      stepIndex: 0,
      toolName: "cdp_click",
      phase: "executing",
      body: "clicked",
      failed: false,
    });

    useStore.getState().applyWorldModelDelta("run-1", {
      changed_fields: ["focused_app", "elements"],
    });

    expect(useStore.getState().runTraces["run-1"].worldModelDeltas).toEqual([
      { stepIndex: 1, changedFields: ["focused_app", "elements"] },
    ]);
  });

  it("applyBoundary appends non-terminal milestones and ignores terminal boundaries", () => {
    useStore
      .getState()
      .applyBoundary("run-1", "subgoal_completed", 2, "Logged in");
    useStore
      .getState()
      .applyBoundary("run-1", "recovery_succeeded", 3, null);
    useStore.getState().applyBoundary("run-1", "terminal", 4, null);

    expect(useStore.getState().runTraces["run-1"].milestones).toEqual([
      { stepIndex: 2, kind: "subgoal_completed", text: "Logged in" },
      { stepIndex: 3, kind: "recovery_succeeded", text: "Recovery succeeded" },
    ]);
  });

  it("pushTraceStep, setTerminalFrame, and clearTrace update the run trace", () => {
    useStore.getState().pushTraceStep("run-1", {
      stepIndex: 0,
      toolName: "cdp_find_elements",
      phase: "exploring",
      body: "found button",
      failed: false,
    });
    useStore
      .getState()
      .setTerminalFrame("run-1", { kind: "complete", detail: "Done" });

    expect(useStore.getState().runTraces["run-1"].steps).toHaveLength(1);
    expect(useStore.getState().runTraces["run-1"].terminalFrame).toEqual({
      kind: "complete",
      detail: "Done",
    });

    useStore.getState().clearTrace("run-1");
    expect(useStore.getState().runTraces["run-1"]).toBeUndefined();
  });
});

describe("agent chat persistence", () => {
  beforeEach(() => {
    (commands.saveAgentChat as ReturnType<typeof vi.fn>).mockClear();
    useStore.setState({ messages: [] });
  });

  it("calls saveAgentChat after pushAssistantMessage", async () => {
    useStore.getState().pushAssistantMessage("user", "hello");
    // Let fire-and-forget save microtasks + the dynamic import in
    // agentChatPersistence settle.
    await new Promise<void>((resolve) => setTimeout(resolve, 0));
    await new Promise<void>((resolve) => setTimeout(resolve, 0));
    expect(commands.saveAgentChat).toHaveBeenCalled();
  });

  it("calls saveAgentChat after pushSystemAnnotation", async () => {
    useStore.getState().pushSystemAnnotation("deleted a thing");
    await new Promise<void>((resolve) => setTimeout(resolve, 0));
    await new Promise<void>((resolve) => setTimeout(resolve, 0));
    expect(commands.saveAgentChat).toHaveBeenCalled();
  });
});

describe("clearConversationFlow", () => {
  beforeEach(() => {
    (commands.clearAgentConversation as ReturnType<typeof vi.fn>).mockClear();
    useStore.setState({ messages: [], projectPath: null, storeTraces: true });
  });

  it("zeroes agentRunStartedAt and agentRunFinishedAt (D24)", async () => {
    useStore.setState({
      agentRunStartedAt: 100,
      agentRunFinishedAt: 200,
    });

    await useStore.getState().clearConversationFlow();

    const s = useStore.getState();
    expect(s.agentRunStartedAt).toBeNull();
    expect(s.agentRunFinishedAt).toBeNull();
  });

  it("hydrateRunTrace drops the trace and sets agentRunId when no run is active", () => {
    useStore.setState({ agentRunId: null, runTraces: {} });
    useStore.getState().hydrateRunTrace({
      runId: "hydrated-2026-04-01",
      phase: "executing",
      activeSubgoal: "Click button",
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
      terminalFrame: null,
    });
    const s = useStore.getState();
    expect(s.agentRunId).toBe("hydrated-2026-04-01");
    expect(s.runTraces["hydrated-2026-04-01"].steps).toHaveLength(1);
  });

  it("hydrateRunTrace is a no-op while a live run is active", () => {
    useStore.setState({
      agentRunId: "live-run",
      runTraces: {
        "live-run": {
          runId: "live-run",
          phase: "exploring",
          activeSubgoal: "",
          steps: [],
          worldModelDeltas: [],
          milestones: [],
          terminalFrame: null,
        },
      },
    });
    useStore.getState().hydrateRunTrace({
      runId: "hydrated-x",
      phase: "executing",
      activeSubgoal: "",
      steps: [],
      worldModelDeltas: [],
      milestones: [],
      terminalFrame: null,
    });
    const s = useStore.getState();
    expect(s.agentRunId).toBe("live-run");
    expect(s.runTraces["hydrated-x"]).toBeUndefined();
  });

  it("drops the active run trace when clearing conversation", async () => {
    useStore.setState({
      agentRunId: "r1",
      runTraces: {
        r1: {
          runId: "r1",
          phase: "exploring",
          activeSubgoal: "Inspect app",
          steps: [],
          worldModelDeltas: [],
          milestones: [],
          terminalFrame: null,
        },
      },
    });

    await useStore.getState().clearConversationFlow();

    expect(useStore.getState().runTraces.r1).toBeUndefined();
  });
});
