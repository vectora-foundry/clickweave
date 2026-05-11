import { render } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { useEscapeKey } from "./useEscapeKey";
import { useStore } from "../store/useAppStore";

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(),
}));

function Harness() {
  useEscapeKey();
  return null;
}

function pressEscape() {
  window.dispatchEvent(
    new KeyboardEvent("keydown", {
      key: "Escape",
      bubbles: true,
      cancelable: true,
    }),
  );
}

describe("useEscapeKey", () => {
  beforeEach(() => {
    useStore.setState({
      verdictModalOpen: false,
      showSettings: false,
      walkthroughStatus: "Idle",
      walkthroughPanelOpen: false,
      logsDrawerOpen: false,
    });
  });

  it("closes the logs drawer when no higher-priority panel is open", () => {
    useStore.setState({ logsDrawerOpen: true });
    render(<Harness />);

    pressEscape();

    expect(useStore.getState().logsDrawerOpen).toBe(false);
  });

  it("closes settings before logs drawer", () => {
    useStore.setState({ showSettings: true, logsDrawerOpen: true });
    render(<Harness />);

    pressEscape();

    expect(useStore.getState().showSettings).toBe(false);
    expect(useStore.getState().logsDrawerOpen).toBe(true);
  });

  it("closes verdict modal before settings", () => {
    useStore.setState({
      verdictModalOpen: true,
      showSettings: true,
    });
    render(<Harness />);

    pressEscape();

    expect(useStore.getState().verdictModalOpen).toBe(false);
    expect(useStore.getState().showSettings).toBe(true);
  });
});
