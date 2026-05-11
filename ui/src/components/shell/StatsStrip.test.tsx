import { describe, expect, it, beforeEach, vi } from "vitest";
import { render, screen, fireEvent } from "@testing-library/react";
import { StatsStrip } from "./StatsStrip";
import { useStore } from "../../store/useAppStore";

const mockSkill = (id: string, state: "draft" | "confirmed" | "promoted") => ({
  id,
  version: 1,
  name: id,
  description: "",
  state,
  scope: "project_local" as const,
  occurrence_count: 1,
  success_rate: 1,
  edited_by_user: false,
});

describe("StatsStrip", () => {
  beforeEach(() => {
    useStore.setState({
      drafts: [
        mockSkill("a", "draft"),
        mockSkill("b", "draft"),
        mockSkill("c", "draft"),
        mockSkill("d", "draft"),
      ],
      confirmed: [mockSkill("e", "confirmed")],
      promoted: [],
    });
  });

  it("renders the first three drafts in array order, not all four", () => {
    render(<StatsStrip onOpenSkillsManager={() => {}} />);
    expect(screen.getByText("a")).toBeInTheDocument();
    expect(screen.getByText("b")).toBeInTheDocument();
    expect(screen.getByText("c")).toBeInTheDocument();
    expect(screen.queryByText("d")).toBeNull();
  });

  it("shows the bucket count from the slice", () => {
    render(<StatsStrip onOpenSkillsManager={() => {}} />);
    expect(screen.getByText("4")).toBeInTheDocument();
  });

  it("invokes onOpenSkillsManager when the Skills Manager pill is clicked", () => {
    const onOpen = vi.fn();
    render(<StatsStrip onOpenSkillsManager={onOpen} />);
    fireEvent.click(screen.getByText(/skills manager/i));
    expect(onOpen).toHaveBeenCalledOnce();
  });
});
