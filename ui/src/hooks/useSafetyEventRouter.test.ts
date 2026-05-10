import { createElement } from "react";
import { act, cleanup, render } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const eventMock = vi.hoisted(() => ({
  listeners: new Map<string, Set<(event: { payload: unknown }) => void>>(),
  listen: vi.fn(
    async (
      topic: string,
      handler: (event: { payload: unknown }) => void,
    ): Promise<() => void> => {
      const set = eventMock.listeners.get(topic) ?? new Set();
      set.add(handler);
      eventMock.listeners.set(topic, set);
      return () => set.delete(handler);
    },
  ),
}));

vi.mock("@tauri-apps/api/event", () => ({
  listen: eventMock.listen,
}));

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(),
}));

import { useStore } from "../store/useAppStore";
import { useSafetyEventRouter } from "./useSafetyEventRouter";

function emit(topic: string, payload: unknown) {
  const handlers = eventMock.listeners.get(topic);
  if (!handlers) return;
  for (const h of handlers) h({ payload });
}

function TestHook() {
  useSafetyEventRouter();
  return null;
}

describe("useSafetyEventRouter", () => {
  beforeEach(() => {
    eventMock.listeners.clear();
    useStore.setState({
      sectionApproval: null,
      chatAnchoredApproval: null,
    });
  });

  afterEach(() => {
    cleanup();
    vi.clearAllMocks();
  });

  // (a) skill-scoped supervision pause routes to setSectionApproval
  it("routes skill-scoped supervision_paused to setSectionApproval", async () => {
    render(createElement(TestHook));
    // Wait for listeners to register
    await act(async () => {});

    act(() => {
      emit("executor://supervision_paused", {
        scope: {
          kind: "skill",
          skill_id: "skl_abc123",
          section_id: "section_1",
          step_id: "s_000001",
        },
        finding: "Screenshot mismatch: button not clicked",
        screenshot: null,
      });
    });

    const state = useStore.getState();
    expect(state.sectionApproval).not.toBeNull();
    expect(state.sectionApproval?.scope.kind).toBe("skill");
    expect(state.sectionApproval?.finding).toBe("Screenshot mismatch: button not clicked");
    expect(state.chatAnchoredApproval).toBeNull();
  });

  // (b) ad-hoc supervision pause routes to setChatAnchoredApproval
  it("routes ad-hoc supervision_paused to setChatAnchoredApproval", async () => {
    render(createElement(TestHook));
    await act(async () => {});

    act(() => {
      emit("executor://supervision_paused", {
        scope: {
          kind: "ad_hoc",
          run_id: "00000000-0000-0000-0000-000000000001",
        },
        finding: "Step failed verification",
        screenshot: "base64data",
      });
    });

    const state = useStore.getState();
    expect(state.chatAnchoredApproval).not.toBeNull();
    expect(state.chatAnchoredApproval?.scope.kind).toBe("ad_hoc");
    expect(state.chatAnchoredApproval?.finding).toBe("Step failed verification");
    expect(state.chatAnchoredApproval?.screenshot).toBe("base64data");
    expect(state.sectionApproval).toBeNull();
  });

  // (c) D8 freeze invariant: skill_id is stable across a run; the router
  //     never receives a scope with a missing or mutated skill_id mid-run.
  //     We verify this by asserting that a skill-scoped event with a populated
  //     skill_id is stored verbatim and that the router does not transform it.
  it("preserves skill_id from scope without modification (D8 freeze)", async () => {
    render(createElement(TestHook));
    await act(async () => {});

    const frozenSkillId = "skl_frozen_001";
    act(() => {
      emit("executor://supervision_paused", {
        scope: {
          kind: "skill",
          skill_id: frozenSkillId,
          section_id: "section_2",
          step_id: "s_000003",
        },
        finding: "VLM disagreement",
        screenshot: null,
      });
    });

    const pause = useStore.getState().sectionApproval;
    expect(pause).not.toBeNull();
    expect(pause?.scope.kind === "skill" && pause.scope.skill_id).toBe(frozenSkillId);
  });
});
