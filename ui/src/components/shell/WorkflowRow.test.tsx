import { describe, expect, it, beforeEach } from "vitest";
import { render, screen, fireEvent } from "@testing-library/react";
import { WorkflowRow } from "./WorkflowRow";
import { useStore } from "../../store/useAppStore";

describe("WorkflowRow", () => {
  beforeEach(() => {
    useStore.setState({ projectName: "MyFlow" });
  });

  it("renames the project on blur commit", () => {
    render(<WorkflowRow />);
    fireEvent.click(screen.getByRole("button", { name: /rename/i }));
    const input = screen.getByDisplayValue("MyFlow");
    fireEvent.change(input, { target: { value: "Renamed" } });
    fireEvent.blur(input);

    expect(useStore.getState().projectName).toBe("Renamed");
  });

  it("keeps long project names from pushing the rename control offscreen", () => {
    const longName = `Workflow-${"VeryLongNameWithoutBreaks".repeat(10)}`;
    useStore.setState({ projectName: longName });

    render(<WorkflowRow />);

    expect(screen.getByText(longName)).toHaveClass("min-w-0", "truncate");
    expect(screen.getByText(longName)).toHaveAttribute("title", longName);
    expect(screen.getByRole("button", { name: /rename/i })).toHaveClass(
      "shrink-0",
    );
  });
});
