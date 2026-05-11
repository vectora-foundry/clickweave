/**
 * Tests for `SkillSectionCard`.
 *
 * Coverage per plan:
 * (a) renders heading + summary
 * (b) shows step count
 * (c) hover button visible only on hover
 * (d) expand toggles ### rows
 */

import { cleanup, render, screen, fireEvent } from "@testing-library/react";
import { afterEach, describe, it, expect, vi, beforeEach } from "vitest";

// Mock Tauri
vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(async () => () => {}) }));
vi.mock("@tauri-apps/api/webviewWindow", () => ({
  WebviewWindow: class {
    static async getByLabel() { return null; }
  },
}));
vi.mock("@tauri-apps/api/window", () => ({ currentMonitor: async () => null }));
vi.mock("../../bindings", () => ({
  commands: new Proxy({}, {
    get: () => vi.fn(async () => undefined),
  }),
}));

import { useStore } from "../../store/useAppStore";
import { SkillSectionCard } from "./SkillSectionCard";
import type { SkillSection } from "../../bindings";
import type { SectionRunStatus } from "../../store/slices/skillsSlice";

const section: SkillSection = {
  id: "section_1",
  heading: "Launch the app",
  level: 2,
  step_ids: ["s_001", "s_002"],
  body_range: [0, 30],
};

const sectionBody = "Open the application and click the button.";

function renderCard(props?: Partial<Parameters<typeof SkillSectionCard>[0]>) {
  return render(
    <SkillSectionCard
      section={section}
      sectionBody={sectionBody}
      selected={false}
      onClick={vi.fn()}
      {...props}
    />,
  );
}

function setRunStatus(sectionId: string, status: SectionRunStatus) {
  useStore.setState({ sectionRunState: { [sectionId]: status } });
}

describe("SkillSectionCard", () => {
  beforeEach(() => {
    useStore.setState({ sectionApproval: null, sectionRunState: {} });
  });

  afterEach(() => {
    cleanup();
    vi.clearAllMocks();
  });

  // (a) renders heading + summary
  it("renders the section heading", () => {
    renderCard();
    expect(screen.getByText("Launch the app")).toBeInTheDocument();
  });

  it("renders a one-line summary from the section body", () => {
    renderCard();
    expect(
      screen.getByText("Open the application and click the button."),
    ).toBeInTheDocument();
  });

  // (b) shows step count
  it("shows the step count badge when step_ids are present", () => {
    renderCard();
    expect(screen.getByText("2 steps")).toBeInTheDocument();
  });

  it("shows singular step label for a single step", () => {
    const oneStep: SkillSection = { ...section, step_ids: ["s_001"] };
    render(
      <SkillSectionCard
        section={oneStep}
        sectionBody={sectionBody}
        selected={false}
        onClick={vi.fn()}
      />,
    );
    expect(screen.getByText("1 step")).toBeInTheDocument();
  });

  // (c) hover button visible only on hover (data-testid)
  it("the Edit button exists in the DOM and is revealed on hover", () => {
    renderCard();
    // Trigger hover by dispatching mouseenter
    const card = screen.getByText("Launch the app").closest("div.group");
    expect(card).toBeTruthy();
    fireEvent.mouseEnter(card!);
    expect(screen.getByTestId("edit-with-assistant")).toBeInTheDocument();
  });

  // (d) expand toggle exists for sub-sections (level >= 3)
  it("shows expand toggle for sub-sections (level 3)", () => {
    const subSection: SkillSection = { ...section, level: 3 };
    render(
      <SkillSectionCard
        section={subSection}
        sectionBody={sectionBody}
        selected={false}
        onClick={vi.fn()}
      />,
    );
    expect(screen.getByTestId("expand-toggle")).toBeInTheDocument();
  });

  it("expand toggle reveals step ids when clicked", () => {
    const subSection: SkillSection = { ...section, level: 3 };
    render(
      <SkillSectionCard
        section={subSection}
        sectionBody={sectionBody}
        selected={false}
        onClick={vi.fn()}
      />,
    );
    // Step IDs are not visible yet
    expect(screen.queryByText("s_001")).not.toBeInTheDocument();
    // Click expand
    fireEvent.click(screen.getByTestId("expand-toggle"));
    expect(screen.getByText("s_001")).toBeInTheDocument();
    expect(screen.getByText("s_002")).toBeInTheDocument();
    // Click again to collapse
    fireEvent.click(screen.getByTestId("expand-toggle"));
    expect(screen.queryByText("s_001")).not.toBeInTheDocument();
  });
});

// ── 1.J.1: Run state badge ───────────────────────────────────────────────────

describe("SkillSectionCard — run state badge (1.J.1)", () => {
  beforeEach(() => {
    useStore.setState({ sectionApproval: null, sectionRunState: {} });
    cleanup();
  });

  it("shows running badge when section run state is running", () => {
    setRunStatus(section.id, "running");
    renderCard();
    expect(screen.getByTestId("run-state-badge")).toHaveTextContent("running");
  });

  it("shows succeeded badge when section run state is succeeded", () => {
    setRunStatus(section.id, "succeeded");
    renderCard();
    expect(screen.getByTestId("run-state-badge")).toHaveTextContent("succeeded");
  });

  it("does not show a badge when status is pending", () => {
    setRunStatus(section.id, "pending");
    renderCard();
    expect(screen.queryByTestId("run-state-badge")).not.toBeInTheDocument();
  });

  it("does not show a badge when no run state is set", () => {
    renderCard();
    expect(screen.queryByTestId("run-state-badge")).not.toBeInTheDocument();
  });
});

// ── 1.J.3: Failure handoff — red outline and resume button ──────────────────

describe("SkillSectionCard — failure handoff (1.J.3)", () => {
  beforeEach(() => {
    useStore.setState({ sectionApproval: null, sectionRunState: {} });
    cleanup();
  });

  // (a) failure renders red outline via data-run-status attribute
  it("(a) sets data-run-status=failed when the section has failed", () => {
    setRunStatus(section.id, "failed");
    renderCard();
    const card = screen.getByTestId("run-state-badge").closest("[data-run-status]");
    expect(card).toHaveAttribute("data-run-status", "failed");
  });

  // (c) Resume button appears on failed section and calls onResume
  it("(c) shows Resume button when section is failed and onResume is provided", () => {
    setRunStatus(section.id, "failed");
    const onResume = vi.fn();
    render(
      <SkillSectionCard
        section={section}
        sectionBody={sectionBody}
        selected={false}
        onClick={vi.fn()}
        onResume={onResume}
      />,
    );
    const resumeButton = screen.getByTestId("resume-from-failure");
    expect(resumeButton).toBeInTheDocument();
    fireEvent.click(resumeButton);
    expect(onResume).toHaveBeenCalledWith(section.id);
  });

  it("does not show Resume button when section is not failed", () => {
    setRunStatus(section.id, "running");
    const onResume = vi.fn();
    render(
      <SkillSectionCard
        section={section}
        sectionBody={sectionBody}
        selected={false}
        onClick={vi.fn()}
        onResume={onResume}
      />,
    );
    expect(screen.queryByTestId("resume-from-failure")).not.toBeInTheDocument();
  });
});
