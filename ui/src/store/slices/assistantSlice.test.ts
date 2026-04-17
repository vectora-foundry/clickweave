import { describe, it, expect, vi, beforeEach } from "vitest";

// `invoke` is pulled in transitively by the composed store; mock before import.
vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(),
}));

import { useStore } from "../useAppStore";

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
