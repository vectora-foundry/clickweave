import { useState, useRef, useEffect } from "react";
import type { ExecutionMode } from "../bindings";
import { isWalkthroughBusy, type WalkthroughStatus } from "../store/slices/walkthroughSlice";

interface ConfirmDialogProps {
  title: string;
  description: string;
  confirmLabel: string;
  confirmClassName: string;
  onCancel: () => void;
  onConfirm: () => void;
}

function ConfirmDialog({ title, description, confirmLabel, confirmClassName, onCancel, onConfirm }: ConfirmDialogProps) {
  useEffect(() => {
    const handler = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.stopPropagation();
        e.preventDefault();
        onCancel();
      }
    };
    window.addEventListener("keydown", handler, true);
    return () => window.removeEventListener("keydown", handler, true);
  }, [onCancel]);

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/60">
      <div className="w-[400px] rounded-lg border border-[var(--border)] bg-[var(--bg-panel)] p-5 shadow-2xl">
        <h3 className="text-sm font-medium text-[var(--text-primary)]">
          {title}
        </h3>
        <p className="mt-2 text-xs text-[var(--text-secondary)]">
          {description}
        </p>
        <div className="mt-4 flex justify-end gap-2">
          <button
            onClick={onCancel}
            className="rounded px-3 py-1.5 text-xs text-[var(--text-secondary)] hover:bg-[var(--bg-hover)]"
          >
            Cancel
          </button>
          <button
            onClick={onConfirm}
            className={`rounded px-4 py-1.5 text-xs font-medium text-white hover:opacity-90 ${confirmClassName}`}
          >
            {confirmLabel}
          </button>
        </div>
      </div>
    </div>
  );
}

interface FloatingToolbarProps {
  executorState: "idle" | "running";
  executionMode: ExecutionMode;
  logsOpen: boolean;
  hasAiNodes: boolean;
  hasNodes: boolean;
  walkthroughStatus: WalkthroughStatus;
  walkthroughPanelOpen: boolean;
  walkthroughEventCount: number;
  autoApproveResolutions: boolean;
  onToggleLogs: () => void;
  onRunStop: () => void;
  onAssistant: () => void;
  onSetExecutionMode: (mode: ExecutionMode) => void;
  onOpenWalkthroughPanel: () => void;
  onRecord: () => void;
  onToggleAutoApprove: (enabled: boolean) => void;
}

export function FloatingToolbar({
  executorState,
  executionMode,
  logsOpen,
  hasAiNodes,
  hasNodes,
  walkthroughStatus,
  walkthroughPanelOpen,
  onToggleLogs,
  onRunStop,
  onAssistant,
  onSetExecutionMode,
  onOpenWalkthroughPanel,
  walkthroughEventCount,
  onRecord,
  autoApproveResolutions,
  onToggleAutoApprove,
}: FloatingToolbarProps) {
  const isRunning = executorState === "running";
  const [showConfirm, setShowConfirm] = useState(false);
  const [showRecordConfirm, setShowRecordConfirm] = useState(false);
  const [showModeMenu, setShowModeMenu] = useState(false);
  const menuRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!showModeMenu) return;
    const handler = (e: MouseEvent) => {
      if (menuRef.current && !menuRef.current.contains(e.target as Node)) {
        setShowModeMenu(false);
      }
    };
    document.addEventListener("mousedown", handler);
    return () => document.removeEventListener("mousedown", handler);
  }, [showModeMenu]);

  const handleRunStop = () => {
    if (isRunning) {
      onRunStop();
      return;
    }
    if (hasAiNodes) {
      setShowConfirm(true);
    } else {
      onRunStop();
    }
  };

  const handleRecord = () => {
    if (hasNodes) {
      setShowRecordConfirm(true);
    } else {
      onRecord();
    }
  };

  const runLabel = executionMode === "Test" ? "Test" : "Run";
  const walkthroughBusy = isWalkthroughBusy(walkthroughStatus);

  return (
    <>
      <div className="absolute bottom-14 left-1/2 z-20 flex -translate-x-1/2 items-center gap-1 rounded-lg border border-[var(--border)] bg-[var(--bg-panel)] px-2 py-1 shadow-lg">
        {walkthroughBusy ? (
          walkthroughStatus === "Processing" ? (
            <div className="flex items-center gap-2 px-2.5 py-1.5">
              <div className="h-3 w-3 animate-spin rounded-full border-2 border-[var(--accent-coral)] border-t-transparent" />
              <span className="text-xs text-[var(--text-secondary)]">
                Processing{walkthroughEventCount > 0 ? ` ${walkthroughEventCount} event${walkthroughEventCount !== 1 ? "s" : ""}` : ""}…
              </span>
            </div>
          ) : (
            <div className="flex items-center gap-1.5 px-2.5 py-1.5">
              <span className={`h-2 w-2 rounded-full shrink-0 ${walkthroughStatus === "Recording" ? "bg-red-500 animate-pulse" : "bg-yellow-500"}`} />
              <span className="text-xs text-[var(--text-secondary)]">
                {walkthroughStatus === "Recording" ? "Recording" : "Paused"}
              </span>
            </div>
          )
        ) : (
          <>
            <button
              onClick={onAssistant}
              className="rounded px-2.5 py-1.5 text-xs text-[var(--accent-blue)] hover:bg-[var(--bg-hover)]"
            >
              Assistant
            </button>
            {walkthroughStatus === "Idle" && !isRunning && (
              <>
                <div className="mx-1 h-4 w-px bg-[var(--border)]" />
                <button
                  onClick={handleRecord}
                  className="flex items-center gap-1.5 rounded px-2.5 py-1.5 text-xs text-[var(--accent-coral)] hover:bg-[var(--bg-hover)]"
                >
                  <span className="h-2 w-2 rounded-full bg-red-500" />
                  Record
                </button>
              </>
            )}
            {walkthroughStatus === "Review" && !walkthroughPanelOpen && (
              <>
                <div className="mx-1 h-4 w-px bg-[var(--border)]" />
                <button
                  onClick={onOpenWalkthroughPanel}
                  className="rounded px-2.5 py-1.5 text-xs text-[var(--accent-coral)] hover:bg-[var(--bg-hover)]"
                >
                  Review
                </button>
              </>
            )}
            <div className="mx-1 h-4 w-px bg-[var(--border)]" />
            <button
              onClick={onToggleLogs}
              className={`rounded px-2.5 py-1.5 text-xs transition-colors ${
                logsOpen
                  ? "bg-[var(--bg-hover)] text-[var(--text-primary)]"
                  : "text-[var(--text-secondary)] hover:bg-[var(--bg-hover)]"
              }`}
            >
              Logs
            </button>
            {executionMode === "Test" && (
              <>
                <div className="mx-1 h-4 w-px bg-[var(--border)]" />
                <label
                  title="Auto-approve runtime resolutions"
                  className={`flex items-center gap-1.5 cursor-pointer ${isRunning ? "opacity-50 pointer-events-none" : ""}`}
                >
                  <button
                    role="switch"
                    aria-checked={autoApproveResolutions}
                    onClick={() => onToggleAutoApprove(!autoApproveResolutions)}
                    disabled={isRunning}
                    className="relative rounded-full transition-colors"
                    style={{
                      width: 28,
                      height: 16,
                      padding: 0,
                      backgroundColor: autoApproveResolutions ? "var(--accent-blue)" : "#525252",
                    }}
                  >
                    <span
                      className="absolute rounded-full transition-transform"
                      style={{
                        top: 2,
                        left: 0,
                        width: 12,
                        height: 12,
                        backgroundColor: autoApproveResolutions ? "#fff" : "#a3a3a3",
                        transform: `translateX(${autoApproveResolutions ? 14 : 2}px)`,
                      }}
                    />
                  </button>
                  <span className={`text-[11px] ${autoApproveResolutions ? "text-[var(--accent-blue)]" : "text-[var(--text-tertiary)]"}`}>
                    Auto
                  </span>
                </label>
              </>
            )}
            <div className="mx-1 h-4 w-px bg-[var(--border)]" />
            {hasAiNodes && !isRunning && (
              <span className="rounded bg-[var(--accent-blue)]/20 px-1.5 py-0.5 text-[10px] font-medium text-[var(--accent-blue)]">
                AI
              </span>
            )}
            <div className="relative" ref={menuRef}>
              <div className="flex items-center">
                <button
                  onClick={handleRunStop}
                  title={isRunning ? "Stop workflow (⌘⇧Esc works globally)" : `${runLabel} (⌘R)`}
                  className={`rounded-l px-2.5 py-1 text-xs font-medium transition-colors ${
                    isRunning
                      ? "bg-red-500/20 text-red-400 hover:bg-red-500/30"
                      : "bg-[var(--accent-green)]/20 text-[var(--accent-green)] hover:bg-[var(--accent-green)]/30"
                  }`}
                >
                  {isRunning ? "Stop" : runLabel}
                </button>
                {!isRunning && (
                  <button
                    onClick={() => setShowModeMenu((prev) => !prev)}
                    title="Switch execution mode"
                    className="rounded-r border-l border-[var(--border)] bg-[var(--accent-green)]/20 px-1 py-1 text-xs text-[var(--accent-green)] hover:bg-[var(--accent-green)]/30"
                  >
                    <svg width="8" height="8" viewBox="0 0 8 8" fill="currentColor">
                      <path d="M1 3l3 3 3-3z" />
                    </svg>
                  </button>
                )}
              </div>
              {showModeMenu && (
                <div className="absolute bottom-full right-0 mb-1 w-40 rounded-md border border-[var(--border)] bg-[var(--bg-panel)] py-1 shadow-lg">
                  <button
                    onClick={() => {
                      onSetExecutionMode("Test");
                      setShowModeMenu(false);
                    }}
                    className={`flex w-full items-center gap-2 px-3 py-1.5 text-left text-xs hover:bg-[var(--bg-hover)] ${
                      executionMode === "Test"
                        ? "text-[var(--accent-green)]"
                        : "text-[var(--text-secondary)]"
                    }`}
                  >
                    {executionMode === "Test" && (
                      <span className="text-[10px]">&#10003;</span>
                    )}
                    <span className={executionMode === "Test" ? "" : "ml-4"}>
                      Test
                    </span>
                  </button>
                  <button
                    onClick={() => {
                      onSetExecutionMode("Run");
                      setShowModeMenu(false);
                    }}
                    className={`flex w-full items-center gap-2 px-3 py-1.5 text-left text-xs hover:bg-[var(--bg-hover)] ${
                      executionMode === "Run"
                        ? "text-[var(--accent-green)]"
                        : "text-[var(--text-secondary)]"
                    }`}
                  >
                    {executionMode === "Run" && (
                      <span className="text-[10px]">&#10003;</span>
                    )}
                    <span className={executionMode === "Run" ? "" : "ml-4"}>
                      Run
                    </span>
                  </button>
                </div>
              )}
            </div>
          </>
        )}
      </div>
      {isRunning && (
        <div className="absolute bottom-8 left-1/2 z-20 -translate-x-1/2 animate-pulse text-center text-[10px] text-red-400/70">
          Press <kbd className="rounded border border-red-500/30 bg-red-500/10 px-1 py-0.5 font-mono text-[9px] text-red-400">⌘⇧Esc</kbd> to stop from any app
        </div>
      )}

      {showConfirm && (
        <ConfirmDialog
          title="Workflow contains AI nodes"
          description="This workflow includes non-deterministic AI steps that will make LLM calls during execution. Results may vary between runs."
          confirmLabel="Run Anyway"
          confirmClassName="bg-[var(--accent-green)]"
          onCancel={() => setShowConfirm(false)}
          onConfirm={() => { setShowConfirm(false); onRunStop(); }}
        />
      )}

      {showRecordConfirm && (
        <ConfirmDialog
          title="Replace current workflow?"
          description="Recording a new walkthrough will replace the existing nodes on your canvas. You can undo this after applying."
          confirmLabel="Record Anyway"
          confirmClassName="bg-[var(--accent-coral)]"
          onCancel={() => setShowRecordConfirm(false)}
          onConfirm={() => { setShowRecordConfirm(false); onRecord(); }}
        />
      )}
    </>
  );
}
