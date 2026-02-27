import type { StateCreator } from "zustand";
import type { StoreState } from "./types";

// These types mirror the Rust types in clickweave-core.
// They will be available from bindings.ts after regeneration,
// but are defined here to avoid blocking on a debug build.
export interface CheckResult {
  check_name: string;
  check_type: "TextPresent" | "TemplateFound" | "WindowTitleMatches" | "ScreenshotMatch";
  verdict: "Pass" | "Fail" | "Warn";
  reasoning: string;
}

export interface NodeVerdict {
  node_id: string;
  node_name: string;
  check_results: CheckResult[];
  expected_outcome_verdict: CheckResult | null;
}

export type VerdictStatus = "none" | "passed" | "failed" | "warned";

export interface VerdictSlice {
  verdicts: NodeVerdict[];
  verdictStatus: VerdictStatus;
  verdictBarVisible: boolean;
  verdictModalOpen: boolean;

  setVerdicts: (verdicts: NodeVerdict[]) => void;
  dismissVerdictBar: () => void;
  clearVerdicts: () => void;
  openVerdictModal: () => void;
  closeVerdictModal: () => void;
}

export function countChecks(verdicts: NodeVerdict[]): { total: number; passed: number } {
  const total = verdicts.reduce(
    (sum, v) => sum + v.check_results.length + (v.expected_outcome_verdict ? 1 : 0),
    0,
  );
  const passed = verdicts.reduce(
    (sum, v) =>
      sum +
      v.check_results.filter((r) => r.verdict === "Pass").length +
      (v.expected_outcome_verdict?.verdict === "Pass" ? 1 : 0),
    0,
  );
  return { total, passed };
}

function computeStatus(verdicts: NodeVerdict[]): VerdictStatus {
  if (verdicts.length === 0) return "none";
  const hasFail = verdicts.some(
    (v) =>
      v.check_results.some((r) => r.verdict === "Fail") ||
      v.expected_outcome_verdict?.verdict === "Fail",
  );
  if (hasFail) return "failed";
  const hasWarn = verdicts.some(
    (v) =>
      v.check_results.some((r) => r.verdict === "Warn") ||
      v.expected_outcome_verdict?.verdict === "Warn",
  );
  if (hasWarn) return "warned";
  return "passed";
}

export const createVerdictSlice: StateCreator<StoreState, [], [], VerdictSlice> = (set) => ({
  verdicts: [],
  verdictStatus: "none",
  verdictBarVisible: false,
  verdictModalOpen: false,

  setVerdicts: (verdicts) =>
    set({
      verdicts,
      verdictStatus: computeStatus(verdicts),
      verdictBarVisible: verdicts.length > 0,
    }),

  dismissVerdictBar: () => set({ verdictBarVisible: false }),

  clearVerdicts: () =>
    set({ verdicts: [], verdictStatus: "none", verdictBarVisible: false, verdictModalOpen: false }),

  openVerdictModal: () => set({ verdictModalOpen: true, verdictBarVisible: true }),
  closeVerdictModal: () => set({ verdictModalOpen: false }),
});
