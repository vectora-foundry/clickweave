interface OutputsSectionProps {
  nodeTypeName: string;
  nodeType?: Record<string, unknown>;
  autoId?: string;
  consumers?: Record<string, string[]>;
  isActionNode?: boolean;
  onEnableVerification?: () => void;
}

export function OutputsSection({
  isActionNode,
  onEnableVerification,
}: OutputsSectionProps) {
  if (isActionNode) {
    return (
      <div className="mt-3">
        <h4 className="text-xs font-medium text-[var(--text-muted)] mb-1.5">Outputs</h4>
        <p className="text-xs text-[var(--text-muted)] italic">
          No outputs — enable verification to check action effect
        </p>
        {onEnableVerification && (
          <button
            onClick={onEnableVerification}
            className="mt-1.5 text-xs text-[var(--accent-coral)] hover:underline"
          >
            Enable Verification
          </button>
        )}
      </div>
    );
  }

  return (
    <div className="mt-3">
      <h4 className="text-xs font-medium text-[var(--text-muted)] mb-1.5">Outputs</h4>
      <p className="text-xs text-[var(--text-muted)] italic">No outputs</p>
    </div>
  );
}
