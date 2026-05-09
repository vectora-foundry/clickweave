import { useNodeRuns } from "../hooks";
import { EmptyState, StatusBadge } from "../fields";
import { runDuration } from "../formatters";

export function RunsTab({
  nodeName,
  projectPath,
  projectId,
  projectName,
  onSelectRun,
}: {
  nodeName: string;
  projectPath: string | null;
  projectId: string;
  projectName: string;
  onSelectRun: (runId: string) => void;
}) {
  const runs = useNodeRuns(projectPath, projectId, projectName, nodeName);

  if (runs.length === 0) {
    return <EmptyState message="No runs yet. Execute the workflow to create runs." />;
  }

  return (
    <div className="space-y-1">
      {runs.map((run) => {
        const duration = runDuration(run);

        return (
          <button
            key={run.run_id}
            onClick={() => onSelectRun(run.run_id)}
            className="flex w-full items-center gap-3 rounded bg-[var(--bg-input)] px-3 py-2 text-left transition-colors hover:bg-[var(--bg-hover)]"
          >
            <StatusBadge status={run.status} />
            <span className="flex-1 text-xs text-[var(--text-primary)]">
              {new Date(run.started_at).toLocaleString()}
            </span>
            {duration && (
              <span className="text-xs text-[var(--text-muted)]">
                {duration}s
              </span>
            )}
            <span className="text-xs text-[var(--text-muted)]">
              {run.artifacts.length} artifacts
            </span>
          </button>
        );
      })}
    </div>
  );
}
