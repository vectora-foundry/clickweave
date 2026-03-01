import { useEffect, useState } from "react";
import { listen, emit } from "@tauri-apps/api/event";

/**
 * Standalone recording bar rendered in its own always-on-top window.
 * Communicates with the main window exclusively via Tauri events.
 */

type Status = "Recording" | "Paused" | "Processing";

export function RecordingBarView() {
  const [status, setStatus] = useState<Status>("Recording");
  const [eventCount, setEventCount] = useState(0);
  const [currentApp, setCurrentApp] = useState<string | null>(null);

  useEffect(() => {
    const unsubs = Promise.all([
      listen<{ status: string }>("walkthrough://state", (e) => {
        const s = e.payload.status;
        if (s === "Recording" || s === "Paused" || s === "Processing") {
          setStatus(s);
        }
      }),
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      listen<{ event: any }>("walkthrough://event", (e) => {
        setEventCount((c) => c + 1);
        const kind = e.payload.event?.kind;
        if (kind?.type === "AppFocused" && kind.app_name) {
          setCurrentApp(kind.app_name);
        }
      }),
    ]);
    return () => { unsubs.then((fns) => fns.forEach((f) => f())); };
  }, []);

  const onPause = () => emit("recording-bar://action", { action: "pause" });
  const onResume = () => emit("recording-bar://action", { action: "resume" });
  const onStop = () => emit("recording-bar://action", { action: "stop" });
  const onCancel = () => emit("recording-bar://action", { action: "cancel" });

  const isRecording = status === "Recording";
  const isPaused = status === "Paused";
  const isProcessing = status === "Processing";

  return (
    <div
      className="flex h-screen w-screen items-center justify-center"
      style={{ background: "transparent" }}
      data-tauri-drag-region
    >
      <div
        className="flex items-center gap-2.5 rounded-full border border-[var(--border)] bg-[var(--bg-panel)]/95 pl-3.5 pr-1.5 py-1.5 shadow-xl backdrop-blur-sm"
        style={{ WebkitAppRegion: "no-drag" } as React.CSSProperties}
      >
        {isProcessing ? (
          <>
            <div className="h-3 w-3 animate-spin rounded-full border-2 border-[var(--accent-coral)] border-t-transparent" />
            <span className="text-xs font-medium text-[var(--text-primary)]">Processing</span>
            <span className="text-[11px] text-[var(--text-muted)]">
              {eventCount} event{eventCount !== 1 ? "s" : ""}
            </span>
          </>
        ) : (
          <>
            {/* Status indicator */}
            <span
              className={`h-2 w-2 rounded-full shrink-0 ${
                isRecording ? "bg-red-500 animate-pulse" : "bg-yellow-500"
              }`}
            />

            <span className="text-xs font-medium text-[var(--text-primary)]">
              {isRecording ? "Recording" : "Paused"}
            </span>

            <div className="h-3 w-px bg-[var(--border)]" />

            <span className="text-[11px] text-[var(--text-muted)]">
              {currentApp || "Waiting\u2026"}
            </span>
            <span className="text-[11px] text-[var(--text-muted)]">
              {eventCount} event{eventCount !== 1 ? "s" : ""}
            </span>

            <div className="h-3 w-px bg-[var(--border)]" />

            <button
              onClick={isPaused ? onResume : onPause}
              className="rounded-full px-2.5 py-1 text-[11px] font-medium text-[var(--text-secondary)] hover:bg-[var(--bg-hover)] hover:text-[var(--text-primary)]"
            >
              {isPaused ? "Resume" : "Pause"}
            </button>

            <button
              onClick={onStop}
              className="rounded-full bg-[var(--accent-coral)] px-2.5 py-1 text-[11px] font-medium text-white hover:opacity-90"
            >
              Stop
            </button>

            <button
              onClick={onCancel}
              className="rounded-full px-2 py-1 text-[11px] text-[var(--text-muted)] hover:text-[var(--text-secondary)]"
              title="Cancel recording"
            >
              &times;
            </button>
          </>
        )}
      </div>
    </div>
  );
}
