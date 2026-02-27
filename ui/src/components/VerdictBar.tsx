import { useStore } from "../store/useAppStore";
import { countChecks } from "../store/slices/verdictSlice";

export function VerdictBar() {
  const verdicts = useStore((s) => s.verdicts);
  const status = useStore((s) => s.verdictStatus);
  const visible = useStore((s) => s.verdictBarVisible);
  const executorState = useStore((s) => s.executorState);
  const hasNodes = useStore((s) => s.workflow.nodes.length > 0);
  const dismiss = useStore((s) => s.dismissVerdictBar);
  const openModal = useStore((s) => s.openVerdictModal);

  // Hide when running or when there's no workflow to test
  if (executorState === "running" || !hasNodes) return null;

  const hasRun = status !== "none";

  // Post-run state: show result bar
  if (visible) {
    const { total, passed } = countChecks(verdicts);

    const bgColor =
      status === "passed"
        ? "bg-green-900/80 border-green-700"
        : status === "warned"
          ? "bg-yellow-900/80 border-yellow-700"
          : status === "failed"
            ? "bg-red-900/80 border-red-700"
            : "bg-blue-900/80 border-blue-700";

    const label =
      status === "passed"
        ? `PASSED — ${passed}/${total} checks`
        : status === "warned"
          ? `PASSED with warnings — ${passed}/${total} checks`
          : status === "failed"
            ? `FAILED — ${passed}/${total} checks passed`
            : "Test run completed — no checks";

    return (
      <div className={`border-b ${bgColor}`}>
        <div className="flex items-center justify-between px-4 py-2">
          <button
            onClick={openModal}
            className="text-sm font-semibold text-white hover:underline"
          >
            {label}
          </button>
          <button
            onClick={dismiss}
            className="text-xs text-white/60 hover:text-white"
          >
            Dismiss
          </button>
        </div>
      </div>
    );
  }

  // Dismissed post-run state: compact summary
  if (hasRun) {
    const summaryColor =
      status === "passed"
        ? "text-green-500"
        : status === "warned"
          ? "text-yellow-500"
          : status === "failed"
            ? "text-red-500"
            : "text-blue-500";

    const summaryLabel =
      status === "passed"
        ? "Passed"
        : status === "warned"
          ? "Passed with warnings"
          : status === "failed"
            ? "Failed"
            : "Completed";

    return (
      <div className="border-b bg-zinc-800/40 border-zinc-700/50">
        <div className="px-4 py-1.5">
          <span className={`text-xs ${summaryColor}`}>
            Last run: {summaryLabel}
          </span>
        </div>
      </div>
    );
  }

  // Never-run state
  return (
    <div className="border-b bg-zinc-800/40 border-zinc-700/50">
      <div className="px-4 py-1.5">
        <span className="text-xs text-zinc-500">Not yet tested</span>
      </div>
    </div>
  );
}
