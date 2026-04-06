interface AutoApproveBannerProps {
  count: number;
  onDismiss: () => void;
  onViewLogs: () => void;
}

export function AutoApproveBanner({ count, onDismiss, onViewLogs }: AutoApproveBannerProps) {
  if (count <= 0) return null;

  return (
    <div className="border-b bg-blue-900/60 border-blue-700/50">
      <div className="flex items-center justify-between px-4 py-2">
        <span className="text-sm text-blue-200">
          {count} resolution{count !== 1 ? "s" : ""} auto-approved during this run
        </span>
        <div className="flex items-center gap-3">
          <button
            onClick={onViewLogs}
            className="text-xs text-blue-300 hover:text-blue-100 hover:underline"
          >
            View logs
          </button>
          <button
            onClick={onDismiss}
            className="text-xs text-blue-300/60 hover:text-blue-100"
          >
            Dismiss
          </button>
        </div>
      </div>
    </div>
  );
}
