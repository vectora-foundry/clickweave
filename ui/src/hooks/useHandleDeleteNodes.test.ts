import { describe, it, expect, vi, beforeEach } from "vitest";
import { renderHook, act } from "@testing-library/react";

// Mock Tauri core — the zustand slices transitively import `invoke`.
vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

// Partial-mock the generated bindings: keep every command the rest of
// the store uses as a passthrough vi.fn so the slice factories don't
// blow up during setState, but override `pruneAgentCacheForNodes`
// with a spy so we can assert it was called.
vi.mock("../bindings", async () => {
  const pruneAgentCacheForNodes = vi.fn(async () => undefined);
  const noop = () => vi.fn(async () => undefined);
  return {
    commands: new Proxy(
      { pruneAgentCacheForNodes },
      {
        get(target, prop, receiver) {
          if (prop in target) return Reflect.get(target, prop, receiver);
          return noop();
        },
      },
    ),
  };
});

import { useStore } from "../store/useAppStore";
import { useHandleDeleteNodes } from "./useHandleDeleteNodes";
import { commands, type Node } from "../bindings";

function makeNode(
  id: string,
  name: string,
  sourceRunId: string | null,
): Node {
  return {
    id,
    node_type: { type: "CdpWait", text: "", timeout_ms: 1000 } as Node["node_type"],
    position: { x: 0, y: 0 },
    name,
    enabled: true,
    timeout_ms: null,
    settle_ms: null,
    retries: 0,
    trace_level: "Minimal",
    role: "Default" as Node["role"],
    expected_outcome: null,
    auto_id: "",
    source_run_id: sourceRunId,
  } as Node;
}

function seedStore(
  nodes: Node[],
  messages: Array<{ role: "user" | "assistant" | "system"; content: string; runId?: string }>,
) {
  useStore.setState({
    workflow: {
      id: "00000000-0000-0000-0000-000000000001",
      name: "wf",
      nodes,
      edges: [],
      groups: [],
      next_id_counters: {},
      intent: null,
    },
    messages: messages.map((m) => ({
      role: m.role,
      content: m.content,
      timestamp: "t",
      runId: m.runId,
    })),
    projectPath: null,
    storeTraces: true,
  });
}

describe("useHandleDeleteNodes", () => {
  beforeEach(() => {
    (commands.pruneAgentCacheForNodes as ReturnType<typeof vi.fn>).mockClear();
  });

  it("calls pruneAgentCacheForNodes for deleted agent-built nodes", async () => {
    const removeNodes = vi.fn((ids: string[]) => {
      useStore.setState((s) => ({
        workflow: {
          ...s.workflow,
          nodes: s.workflow.nodes.filter((n) => !ids.includes(n.id)),
        },
      }));
    });
    seedStore(
      [
        makeNode("n1", "cdp_click", "r1"),
        makeNode("n2", "cdp_fill", "r1"),
      ],
      [
        { role: "user", content: "goal", runId: "r1" },
        { role: "assistant", content: "summary", runId: "r1" },
      ],
    );

    const { result } = renderHook(() => useHandleDeleteNodes(removeNodes));
    await act(async () => {
      result.current(["n1"]);
    });

    expect(removeNodes).toHaveBeenCalledWith(["n1"]);
    expect((commands.pruneAgentCacheForNodes as ReturnType<typeof vi.fn>)).toHaveBeenCalledWith(
      expect.objectContaining({ node_ids: ["n1"], store_traces: true }),
    );
  });

  it("appends a system annotation summarizing the deletion", async () => {
    const removeNodes = vi.fn((ids: string[]) => {
      useStore.setState((s) => ({
        workflow: {
          ...s.workflow,
          nodes: s.workflow.nodes.filter((n) => !ids.includes(n.id)),
        },
      }));
    });
    seedStore(
      [
        makeNode("n1", "cdp_click", "r1"),
        makeNode("n2", "cdp_fill", "r1"),
      ],
      [
        { role: "user", content: "send test", runId: "r1" },
        { role: "assistant", content: "done", runId: "r1" },
      ],
    );

    const { result } = renderHook(() => useHandleDeleteNodes(removeNodes));
    await act(async () => {
      result.current(["n1"]);
    });

    const sys = useStore
      .getState()
      .messages.find((m) => m.role === "system");
    expect(sys).toBeDefined();
    expect(sys!.content).toMatch(/Deleted/);
    expect(sys!.content).toMatch(/cdp_click/);
  });

  it("redacts partial-turn assistant summaries", async () => {
    const removeNodes = vi.fn((ids: string[]) => {
      useStore.setState((s) => ({
        workflow: {
          ...s.workflow,
          nodes: s.workflow.nodes.filter((n) => !ids.includes(n.id)),
        },
      }));
    });
    seedStore(
      [
        makeNode("n1", "cdp_click", "r1"),
        makeNode("n2", "cdp_fill", "r1"),
      ],
      [
        { role: "user", content: "goal", runId: "r1" },
        { role: "assistant", content: "full summary", runId: "r1" },
      ],
    );

    const { result } = renderHook(() => useHandleDeleteNodes(removeNodes));
    await act(async () => {
      result.current(["n1"]);
    });

    const assistant = useStore
      .getState()
      .messages.find((m) => m.role === "assistant" && m.runId === "r1");
    expect(assistant!.content).toBe("(partially deleted by user)");
  });

  it("drops the turn entirely when all its nodes are deleted", async () => {
    const removeNodes = vi.fn((ids: string[]) => {
      useStore.setState((s) => ({
        workflow: {
          ...s.workflow,
          nodes: s.workflow.nodes.filter((n) => !ids.includes(n.id)),
        },
      }));
    });
    seedStore(
      [makeNode("n1", "cdp_click", "r1")],
      [
        { role: "user", content: "goal", runId: "r1" },
        { role: "assistant", content: "summary", runId: "r1" },
      ],
    );

    const { result } = renderHook(() => useHandleDeleteNodes(removeNodes));
    await act(async () => {
      result.current(["n1"]);
    });

    const remaining = useStore
      .getState()
      .messages.filter((m) => m.role !== "system" && m.runId === "r1");
    expect(remaining).toEqual([]);
  });

  it("skips all conversational side-effects when no deleted node carries source_run_id", async () => {
    const removeNodes = vi.fn();
    seedStore(
      [makeNode("n1", "user_click", null)],
      [{ role: "user", content: "unrelated", runId: "r1" }],
    );

    const { result } = renderHook(() => useHandleDeleteNodes(removeNodes));
    await act(async () => {
      result.current(["n1"]);
    });

    expect(removeNodes).toHaveBeenCalledWith(["n1"]);
    expect((commands.pruneAgentCacheForNodes as ReturnType<typeof vi.fn>)).not.toHaveBeenCalled();
    const sys = useStore
      .getState()
      .messages.find((m) => m.role === "system");
    expect(sys).toBeUndefined();
  });
});
