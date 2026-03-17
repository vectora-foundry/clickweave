import { useState, useEffect } from "react";
import { commands } from "../bindings";
import type { CdpAppConfig, DetectedCdpApp } from "../bindings";
import { errorMessage } from "../utils/commandError";

/** CDP setup progress event (emitted via Tauri events, not in auto-generated bindings). */
export type CdpSetupProgress = {
  app_name: string;
  status: "Restarting" | "Launching" | "Connecting" | "Ready" | "Done" | { Failed: { reason: string } };
};
import { open } from "@tauri-apps/plugin-dialog";

type ModalPhase = "detecting" | "selection" | "setup";

interface Props {
  open: boolean;
  mcpCommand: string;
  cdpProgress: CdpSetupProgress[];
  onStart: (cdpApps: CdpAppConfig[]) => void;
  onSkip: () => void;
  onCancel: () => void;
}

export function CdpAppSelectModal({
  open: isOpen,
  mcpCommand,
  cdpProgress,
  onStart,
  onSkip,
  onCancel,
}: Props) {
  const [phase, setPhase] = useState<ModalPhase>("detecting");
  const [apps, setApps] = useState<DetectedCdpApp[]>([]);
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [addedApps, setAddedApps] = useState<DetectedCdpApp[]>([]);
  const [addedPaths, setAddedPaths] = useState<Map<string, string>>(new Map());
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (!isOpen) return;
    setPhase("detecting");
    setApps([]);
    setSelected(new Set());
    setAddedApps([]);
    setAddedPaths(new Map());
    setError(null);

    commands.detectCdpApps(mcpCommand).then((result) => {
      if (result.status === "ok") {
        setApps(result.data);
        setPhase("selection");
      } else {
        setError(errorMessage(result.error));
        setPhase("selection");
      }
    });
  }, [isOpen, mcpCommand]);

  if (!isOpen) return null;

  const allApps = [...apps, ...addedApps];

  const toggleApp = (name: string) => {
    setSelected((prev) => {
      const next = new Set(prev);
      if (next.has(name)) next.delete(name);
      else next.add(name);
      return next;
    });
  };

  const handleAddApp = async () => {
    const path = await open({
      filters: [
        { name: "Applications", extensions: ["app", "exe"] },
      ],
    });
    if (!path) return;
    const result = await commands.validateAppPath(path as string);
    if (result.status === "ok") {
      const app = result.data;
      setAddedApps((prev) => [...prev, app]);
      setAddedPaths((prev) => new Map(prev).set(app.name, path as string));
      setSelected((prev) => new Set(prev).add(app.name));
    } else {
      setError(errorMessage(result.error));
    }
  };

  const handleStart = () => {
    const cdpApps: CdpAppConfig[] = allApps
      .filter((a) => selected.has(a.name))
      .map((a) => ({
        name: a.name,
        binary_path: addedPaths.get(a.name) ?? null,
        app_kind: a.app_kind,
      }));
    setPhase("setup");
    onStart(cdpApps);
  };

  const statusIcon = (status: CdpSetupProgress["status"]) => {
    if (typeof status === "string") {
      if (status === "Ready") return "✓";
      return "…";
    }
    return "✗";
  };

  const statusText = (status: CdpSetupProgress["status"]) => {
    if (typeof status === "string") return status;
    if ("Failed" in status) return `Failed: ${status.Failed.reason}`;
    return "Unknown";
  };

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/60">
      <div className="w-[480px] rounded-lg border border-[var(--border)] bg-[var(--bg-panel)] p-5 shadow-2xl">
        <h3 className="mb-3 text-sm font-medium text-[var(--text-primary)]">
          DevTools Integration
        </h3>

        {phase === "detecting" && (
          <div className="flex items-center gap-2 py-6 text-xs text-[var(--text-secondary)]">
            <span className="animate-spin">⟳</span>
            Scanning for Electron and Chrome apps…
          </div>
        )}

        {phase === "selection" && (
          <>
            {error && (
              <div className="mb-3 rounded bg-red-500/10 px-3 py-2 text-xs text-red-400">
                {error}
              </div>
            )}

            {allApps.length === 0 ? (
              <p className="mb-3 text-xs text-[var(--text-secondary)]">
                No running Electron or Chrome apps detected.
              </p>
            ) : (
              <>
                <p className="mb-2 text-xs text-[var(--text-secondary)]">
                  Select apps to enable DevTools capture. Selected apps will be
                  restarted with remote debugging enabled.
                </p>
                <div className="mb-3 max-h-48 space-y-1 overflow-y-auto">
                  {allApps.map((app) => (
                    <label
                      key={app.name}
                      className="flex cursor-pointer items-center gap-2 rounded px-2 py-1.5 text-xs hover:bg-[var(--bg-hover)]"
                    >
                      <input
                        type="checkbox"
                        checked={selected.has(app.name)}
                        onChange={() => toggleApp(app.name)}
                        className="accent-[var(--accent-coral)]"
                      />
                      <span className="text-[var(--text-primary)]">
                        {app.name}
                      </span>
                      <span className="text-[var(--text-muted)]">
                        {app.app_kind === "ElectronApp"
                          ? "Electron"
                          : "Chrome"}
                      </span>
                    </label>
                  ))}
                </div>
              </>
            )}

            <button
              onClick={handleAddApp}
              className="mb-4 text-xs text-[var(--accent-coral)] hover:underline"
            >
              + Add app from file…
            </button>

            <div className="flex justify-end gap-2">
              <button
                onClick={onCancel}
                className="rounded px-3 py-1.5 text-xs text-[var(--text-secondary)] hover:bg-[var(--bg-hover)]"
              >
                Cancel
              </button>
              <button
                onClick={() => onSkip()}
                className="rounded px-3 py-1.5 text-xs text-[var(--text-secondary)] hover:bg-[var(--bg-hover)]"
              >
                Skip
              </button>
              <button
                onClick={handleStart}
                disabled={selected.size === 0}
                className="rounded bg-[var(--accent-coral)] px-4 py-1.5 text-xs font-medium text-white hover:opacity-90 disabled:opacity-40"
              >
                Start Recording
              </button>
            </div>
          </>
        )}

        {phase === "setup" && (
          <>
            <p className="mb-3 text-xs text-[var(--text-secondary)]">
              Setting up DevTools connections…
            </p>
            <div className="mb-4 space-y-1">
              {cdpProgress.map((p, i) => (
                <div
                  key={i}
                  className="flex items-center gap-2 rounded px-2 py-1.5 text-xs"
                >
                  <span
                    className={
                      typeof p.status === "string" && p.status === "Ready"
                        ? "text-green-400"
                        : typeof p.status !== "string"
                          ? "text-red-400"
                          : "text-[var(--text-muted)]"
                    }
                  >
                    {statusIcon(p.status)}
                  </span>
                  <span className="text-[var(--text-primary)]">
                    {p.app_name}
                  </span>
                  <span className="text-[var(--text-muted)]">
                    {statusText(p.status)}
                  </span>
                </div>
              ))}
            </div>
            <div className="flex justify-end">
              <button
                onClick={onCancel}
                className="rounded px-3 py-1.5 text-xs text-red-400 hover:bg-red-500/10"
              >
                Cancel
              </button>
            </div>
          </>
        )}
      </div>
    </div>
  );
}
