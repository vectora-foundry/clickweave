import { describe, expect, it, beforeEach } from "vitest";
import { render, screen, fireEvent } from "@testing-library/react";
import { LogsBar } from "./LogsBar";
import { useStore } from "../../store/useAppStore";

describe("LogsBar", () => {
  beforeEach(() => {
    useStore.setState({ logs: ["alpha started", "beta failed", "gamma completed"], logsDrawerOpen: true });
  });

  it("filters by search substring without altering the underlying logs slice", () => {
    render(<LogsBar />);
    fireEvent.change(screen.getByPlaceholderText(/search logs/i), { target: { value: "beta" } });
    expect(screen.getByText(/beta failed/)).toBeInTheDocument();
    expect(screen.queryByText(/alpha started/)).toBeNull();
    expect(useStore.getState().logs).toHaveLength(3);
  });

  it("clears logs via the existing slice action", () => {
    render(<LogsBar />);
    fireEvent.click(screen.getByLabelText(/clear logs/i));
    expect(useStore.getState().logs).toHaveLength(0);
  });

  it("applies LogsDrawer's color classes to error/success/normal rows (P1.M2)", () => {
    render(<LogsBar />);
    expect(screen.getByText(/beta failed/)).toHaveClass("text-red-400");
    expect(screen.getByText(/gamma completed/)).toHaveClass("text-[var(--accent-green)]");
    expect(screen.getByText(/alpha started/)).toHaveClass("text-[var(--text-secondary)]");
  });
});
