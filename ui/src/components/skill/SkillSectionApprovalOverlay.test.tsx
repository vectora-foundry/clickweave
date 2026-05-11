/**
 * Tests for `SkillSectionApprovalOverlay`.
 *
 * Coverage per plan:
 * (a) renders when a matching event fires (sectionApproval is set)
 * (b) Allow dispatches `supervision_respond({ allowed: true })` / `approve_agent_action(true)`
 * (c) Deny dispatches `supervision_respond({ allowed: false })` / `approve_agent_action(false)`
 */

import { cleanup, render, screen, fireEvent, act } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const commandMocks = vi.hoisted(() => ({
  supervisionRespond: vi.fn(),
  approveAgentAction: vi.fn(),
}));

vi.mock("../../bindings", () => ({
  commands: {
    supervisionRespond: commandMocks.supervisionRespond,
    approveAgentAction: commandMocks.approveAgentAction,
  },
}));
vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/webviewWindow", () => ({
  WebviewWindow: class {
    static async getByLabel() { return null; }
  },
}));
vi.mock("@tauri-apps/api/window", () => ({ currentMonitor: async () => null }));

import { useStore } from "../../store/useAppStore";
import { SkillSectionApprovalOverlay } from "./SkillSectionApprovalOverlay";
import type { SectionApprovalPause } from "../../store/slices/executionSlice";

const approval: SectionApprovalPause = {
  scope: {
    kind: "skill",
    skill_id: "skl_abc",
    section_id: "section_1",
    step_id: "s_001",
  },
  finding: "VLM disagreement detected",
  screenshot: null,
};

describe("SkillSectionApprovalOverlay", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    commandMocks.supervisionRespond.mockResolvedValue({ status: "ok", data: null });
    commandMocks.approveAgentAction.mockResolvedValue({ status: "ok", data: null });
    useStore.setState({ sectionApproval: approval });
  });

  afterEach(() => {
    cleanup();
  });

  // (a) renders when a matching event fires
  it("renders the approval overlay with the finding text", () => {
    render(<SkillSectionApprovalOverlay approval={approval} />);
    expect(screen.getByTestId("approval-overlay")).toBeInTheDocument();
    expect(screen.getByText("VLM disagreement detected")).toBeInTheDocument();
  });

  // (b) Allow dispatches supervision_respond
  it("Allow button calls supervision_respond('retry') and clears sectionApproval", async () => {
    render(<SkillSectionApprovalOverlay approval={approval} />);
    await act(async () => {
      fireEvent.click(screen.getByTestId("approval-allow"));
    });
    expect(commandMocks.supervisionRespond).toHaveBeenCalledWith("retry");
    expect(useStore.getState().sectionApproval).toBeNull();
  });

  // (c) Deny dispatches supervision_respond('abort')
  it("Deny button calls supervision_respond('abort') and clears sectionApproval", async () => {
    render(<SkillSectionApprovalOverlay approval={approval} />);
    await act(async () => {
      fireEvent.click(screen.getByTestId("approval-deny"));
    });
    expect(commandMocks.supervisionRespond).toHaveBeenCalledWith("abort");
    expect(useStore.getState().sectionApproval).toBeNull();
  });
});
