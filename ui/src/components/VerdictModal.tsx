import { useStore } from "../store/useAppStore";
import { countChecks } from "../store/slices/verdictSlice";
import type { NodeVerdict, CheckResult, VerdictStatus } from "../store/slices/verdictSlice";
import { Modal } from "./Modal";

const statusConfig: Record<Exclude<VerdictStatus, "none">, { heading: string; icon: string; iconCls: string; borderCls: string }> = {
  passed: { heading: "Test Passed", icon: "\u2713", iconCls: "bg-green-500/20 text-green-400", borderCls: "border-green-700/50" },
  warned: { heading: "Test Passed with Warnings", icon: "\u26A0", iconCls: "bg-yellow-500/20 text-yellow-400", borderCls: "border-yellow-700/50" },
  failed: { heading: "Test Failed", icon: "\u2717", iconCls: "bg-red-500/20 text-red-400", borderCls: "border-red-700/50" },
  completed: { heading: "Test Run Completed", icon: "\u2713", iconCls: "bg-blue-500/20 text-blue-400", borderCls: "border-blue-700/50" },
};

const verdictColor: Record<string, string> = {
  Pass: "text-green-400",
  Warn: "text-yellow-400",
  Fail: "text-red-400",
};

const verdictIcon: Record<string, string> = {
  Pass: "\u2713",
  Warn: "\u26A0",
  Fail: "\u2717",
};

export function VerdictModal() {
  const verdicts = useStore((s) => s.verdicts);
  const status = useStore((s) => s.verdictStatus);
  const open = useStore((s) => s.verdictModalOpen);
  const close = useStore((s) => s.closeVerdictModal);

  const hasVerdicts = verdicts.length > 0;
  const config = status !== "none"
    ? statusConfig[status]
    : { heading: "Workflow Completed", icon: "\u2713", iconCls: "bg-green-500/20 text-green-400", borderCls: "border-green-700/50" };
  const { total, passed } = countChecks(verdicts);

  return (
    <Modal open={open} onClose={close} className={`w-[520px] max-h-[80vh] flex flex-col rounded-lg border ${config.borderCls} bg-[var(--bg-panel)] shadow-2xl`}>
        <div className="flex items-center gap-3 px-5 pt-5 pb-3">
          <span className={`flex h-7 w-7 items-center justify-center rounded-full text-sm font-bold ${config.iconCls}`}>
            {config.icon}
          </span>
          <div>
            <h3 className="text-sm font-semibold text-[var(--text-primary)]">{config.heading}</h3>
            {hasVerdicts && (
              <p className="text-xs text-[var(--text-secondary)]">{passed}/{total} checks passed</p>
            )}
          </div>
        </div>

        {hasVerdicts && (
          <div className="flex-1 overflow-y-auto border-t border-[var(--border)] px-5 py-3 space-y-3">
            {verdicts.map((v, i) => (
              <VerdictNodeSection key={`${v.node_id}-${i}`} verdict={v} />
            ))}
          </div>
        )}

        <div className={`flex justify-end ${hasVerdicts ? "border-t border-[var(--border)]" : ""} px-5 py-3`}>
          <button
            onClick={close}
            className="rounded bg-[var(--bg-hover)] px-4 py-1.5 text-xs font-medium text-[var(--text-primary)] hover:opacity-90"
          >
            Close
          </button>
        </div>
    </Modal>
  );
}

function VerdictNodeSection({ verdict }: { verdict: NodeVerdict }) {
  const allResults: CheckResult[] = [
    ...verdict.check_results,
    ...(verdict.expected_outcome_verdict ? [verdict.expected_outcome_verdict] : []),
  ];
  const passed = allResults.filter((r) => r.verdict === "Pass").length;

  return (
    <div>
      <div className="flex items-center gap-2 mb-1.5">
        <span className="text-xs font-medium text-[var(--text-primary)]">{verdict.node_name}</span>
        <span className="text-xs text-[var(--text-secondary)]">
          ({passed}/{allResults.length} passed)
        </span>
      </div>
      <div className="ml-1 space-y-1">
        {allResults.map((r, i) => (
          <div key={i} className="text-xs flex items-start gap-1.5">
            <span className={verdictColor[r.verdict]}>{verdictIcon[r.verdict]}</span>
            <span className="text-[var(--text-secondary)]">
              <span className="text-[var(--text-primary)]">{r.check_name}</span>
              {" \u2014 "}
              {r.reasoning}
            </span>
          </div>
        ))}
      </div>
    </div>
  );
}
