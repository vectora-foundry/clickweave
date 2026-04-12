import { describe, it, expect, vi, beforeEach } from "vitest";

// Tauri's `invoke` must be mocked before walkthroughSlice is imported — the
// slice imports command bindings at module init time.
const invokeMock = vi.fn();
vi.mock("@tauri-apps/api/core", () => ({
  invoke: (...args: unknown[]) => invokeMock(...args),
}));

// Webview window helpers are only needed for the recording-bar lifecycle in
// other actions — stub them so they don't touch a real Tauri runtime.
vi.mock("@tauri-apps/api/webviewWindow", () => ({
  WebviewWindow: class {
    static async getByLabel() {
      return null;
    }
  },
}));
vi.mock("@tauri-apps/api/window", () => ({
  currentMonitor: async () => null,
}));

import { useStore } from "../useAppStore";

describe("walkthroughSlice.pushWalkthroughEvent", () => {
  beforeEach(() => {
    invokeMock.mockReset();
    useStore.setState({
      walkthroughStatus: "Idle",
      walkthroughEvents: [],
    });
  });

  it("appends the event while Recording", () => {
    useStore.setState({ walkthroughStatus: "Recording" });
    useStore.getState().pushWalkthroughEvent({ kind: { type: "Clicked" } });
    expect(useStore.getState().walkthroughEvents).toHaveLength(1);
  });

  it("appends the event while Paused", () => {
    useStore.setState({ walkthroughStatus: "Paused" });
    useStore.getState().pushWalkthroughEvent({ kind: { type: "Clicked" } });
    expect(useStore.getState().walkthroughEvents).toHaveLength(1);
  });

  it("freezes the counter once the backend transitions to Processing", () => {
    // Simulate two events captured during Recording, then a transition to
    // Processing followed by late hover/CDP events from the drain phase.
    useStore.setState({ walkthroughStatus: "Recording" });
    useStore.getState().pushWalkthroughEvent({ kind: { type: "Clicked" } });
    useStore.getState().pushWalkthroughEvent({ kind: { type: "KeyPressed" } });

    useStore.setState({ walkthroughStatus: "Processing" });
    useStore.getState().pushWalkthroughEvent({ kind: { type: "HoverDetected" } });
    useStore.getState().pushWalkthroughEvent({ kind: { type: "CdpHoverResolved" } });

    expect(useStore.getState().walkthroughEvents).toHaveLength(2);
  });

  it("drops events received in Idle/Review/Applied/Cancelled terminal states", () => {
    for (const status of ["Idle", "Review", "Applied", "Cancelled"] as const) {
      useStore.setState({ walkthroughStatus: status, walkthroughEvents: [] });
      useStore.getState().pushWalkthroughEvent({ kind: { type: "Clicked" } });
      expect(useStore.getState().walkthroughEvents).toHaveLength(0);
    }
  });
});
