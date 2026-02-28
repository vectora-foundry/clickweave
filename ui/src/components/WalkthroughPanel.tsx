import { useRef, useEffect } from "react";
import { convertFileSrc } from "@tauri-apps/api/core";
import { useStore } from "../store/useAppStore";
import { useHorizontalResize } from "../hooks/useHorizontalResize";
import type { WalkthroughAction, TargetCandidate, Node } from "../bindings";
import type { WalkthroughCapturedEvent } from "../store/slices/walkthroughSlice";

function eventDescription(event: WalkthroughCapturedEvent): { icon: string; text: string } {
  const e = event as Record<string, unknown>;
  const type = (e.type ?? e.event_type ?? "") as string;

  if (type === "MouseClicked") {
    const x = (e.x as number) ?? 0;
    const y = (e.y as number) ?? 0;
    return { icon: "◉", text: `Clicked at (${Math.round(x)}, ${Math.round(y)})` };
  }
  if (type === "KeyPressed") {
    return { icon: "⌥", text: `Pressed ${(e.key as string) ?? "key"}` };
  }
  if (type === "TextCommitted") {
    const text = (e.text as string) ?? "";
    return { icon: "⌨", text: `Typed '${text.length > 30 ? text.slice(0, 30) + "…" : text}'` };
  }
  if (type === "AppFocused") {
    return { icon: "⬡", text: `Focused ${(e.app_name as string) ?? "app"}` };
  }
  if (type === "Scrolled") {
    return { icon: "↕", text: "Scrolled" };
  }
  return { icon: "•", text: type || "Event" };
}

function actionIcon(kind: WalkthroughAction["kind"]): { icon: string; color: string } {
  switch (kind.type) {
    case "LaunchApp": return { icon: "⬡", color: "text-green-400" };
    case "FocusWindow": return { icon: "◎", color: "text-green-400" };
    case "Click": return { icon: "◉", color: "text-[var(--accent-coral)]" };
    case "TypeText": return { icon: "⌨", color: "text-blue-400" };
    case "PressKey": return { icon: "⌥", color: "text-[var(--text-muted)]" };
    case "Scroll": return { icon: "↕", color: "text-[var(--text-muted)]" };
  }
}

function actionLabel(action: WalkthroughAction): string {
  const k = action.kind;
  switch (k.type) {
    case "LaunchApp": return `Launch ${k.app_name}`;
    case "FocusWindow": return `Focus ${k.app_name}`;
    case "Click": {
      const textTarget = action.target_candidates.find(
        (c) => c.type === "AccessibilityLabel" || c.type === "OcrText",
      );
      if (textTarget) {
        const label = textTarget.type === "AccessibilityLabel" ? textTarget.label : textTarget.text;
        return `Click '${label.length > 25 ? label.slice(0, 25) + "…" : label}'`;
      }
      return `Click (${k.x}, ${k.y})`;
    }
    case "TypeText": {
      const t = k.text;
      return `Type '${t.length > 30 ? t.slice(0, 30) + "…" : t}'`;
    }
    case "PressKey": {
      const mods = k.modifiers.length > 0 ? k.modifiers.join("+") + "+" : "";
      return `Press ${mods}${k.key}`;
    }
    case "Scroll": return "Scroll";
  }
}

function targetCandidateLabel(candidate: TargetCandidate): string {
  switch (candidate.type) {
    case "AccessibilityLabel": return `"${candidate.label}"${candidate.role ? ` (${candidate.role})` : ""}`;
    case "OcrText": return `"${candidate.text}"`;
    case "ImageCrop": return "Image crop";
    case "Coordinates": return `(${candidate.x}, ${candidate.y})`;
  }
}

function targetCandidateIcon(candidate: TargetCandidate): string {
  switch (candidate.type) {
    case "AccessibilityLabel": return "\u{1F3F7}";
    case "OcrText": return "\u{1F441}";
    case "ImageCrop": return "\u{1F5BC}";
    case "Coordinates": return "\u{1F4CD}";
  }
}

function nodeTypeIcon(nodeType: Node["node_type"]): { icon: string; color: string } {
  switch (nodeType.type) {
    case "FocusWindow":
    case "ListWindows": return { icon: "◎", color: "text-green-400" };
    case "Click": return { icon: "◉", color: "text-[var(--accent-coral)]" };
    case "TypeText": return { icon: "⌨", color: "text-blue-400" };
    case "PressKey": return { icon: "⌥", color: "text-[var(--text-muted)]" };
    case "Scroll": return { icon: "↕", color: "text-[var(--text-muted)]" };
    case "AiStep": return { icon: "★", color: "text-purple-400" };
    case "McpToolCall": return { icon: "⚙", color: "text-blue-400" };
    case "TakeScreenshot":
    case "FindText":
    case "FindImage": return { icon: "◇", color: "text-[var(--text-muted)]" };
    case "AppDebugKitOp": return { icon: "⚙", color: "text-[var(--text-muted)]" };
    case "If":
    case "Switch":
    case "Loop":
    case "EndLoop": return { icon: "◆", color: "text-yellow-400" };
  }
}

function confidenceDot(confidence: WalkthroughAction["confidence"]): string {
  switch (confidence) {
    case "High": return "bg-green-400";
    case "Medium": return "bg-yellow-400";
    case "Low": return "bg-red-400";
  }
}

export function WalkthroughPanel() {
  const walkthroughStatus = useStore((s) => s.walkthroughStatus);
  const walkthroughEvents = useStore((s) => s.walkthroughEvents);
  const walkthroughError = useStore((s) => s.walkthroughError);
  const walkthroughActions = useStore((s) => s.walkthroughActions);
  const walkthroughAnnotations = useStore((s) => s.walkthroughAnnotations);
  const walkthroughExpandedAction = useStore((s) => s.walkthroughExpandedAction);
  const walkthroughWarnings = useStore((s) => s.walkthroughWarnings);
  const cancelWalkthrough = useStore((s) => s.cancelWalkthrough);
  const pauseWalkthrough = useStore((s) => s.pauseWalkthrough);
  const resumeWalkthrough = useStore((s) => s.resumeWalkthrough);
  const stopWalkthrough = useStore((s) => s.stopWalkthrough);
  const setWalkthroughExpandedAction = useStore((s) => s.setWalkthroughExpandedAction);
  const walkthroughDraft = useStore((s) => s.walkthroughDraft);
  const walkthroughActionNodeMap = useStore((s) => s.walkthroughActionNodeMap);
  const walkthroughUsedFallback = useStore((s) => s.walkthroughUsedFallback);
  const deleteNode = useStore((s) => s.deleteNode);
  const restoreNode = useStore((s) => s.restoreNode);
  const renameNode = useStore((s) => s.renameNode);
  const overrideTarget = useStore((s) => s.overrideTarget);
  const promoteToVariable = useStore((s) => s.promoteToVariable);
  const removeVariablePromotion = useStore((s) => s.removeVariablePromotion);
  const applyDraftToCanvas = useStore((s) => s.applyDraftToCanvas);
  const discardDraft = useStore((s) => s.discardDraft);

  const { width, handleResizeStart } = useHorizontalResize();
  const feedEndRef = useRef<HTMLDivElement>(null);

  // Auto-scroll feed to bottom
  useEffect(() => {
    feedEndRef.current?.scrollIntoView({ behavior: "smooth" });
  }, [walkthroughEvents.length]);

  if (walkthroughStatus === "Idle" || walkthroughStatus === "Applied" || walkthroughStatus === "Cancelled") return null;

  const isRecording = walkthroughStatus === "Recording" || walkthroughStatus === "Paused";

  // Derive current focused app from last AppFocused event (backwards scan, no copy)
  let currentApp: string | null = null;
  for (let i = walkthroughEvents.length - 1; i >= 0; i--) {
    const e = walkthroughEvents[i] as Record<string, unknown>;
    if (e.type === "AppFocused") {
      currentApp = (e.app_name as string) ?? null;
      break;
    }
  }

  if (isRecording) {
    return (
      <div className="relative flex h-full flex-col border-l border-[var(--border)] bg-[var(--bg-panel)]" style={{ width, minWidth: width }}>
        {/* Resize handle */}
        <div
          onMouseDown={handleResizeStart}
          className="absolute left-0 top-0 z-10 h-full w-1.5 cursor-col-resize hover:bg-[var(--accent-coral)]/30 active:bg-[var(--accent-coral)]/40"
        />

        {/* Header */}
        <div className="flex items-center justify-between border-b border-[var(--border)] px-4 py-2.5">
          <div className="flex items-center gap-2">
            <span
              className={`inline-block h-2 w-2 rounded-full ${
                walkthroughStatus === "Recording"
                  ? "bg-red-500 animate-pulse"
                  : "bg-yellow-500"
              }`}
            />
            <h2 className="text-sm font-medium text-[var(--text-primary)]">
              {walkthroughStatus === "Recording" ? "Recording" : "Paused"}
            </h2>
          </div>
          <button
            onClick={() => cancelWalkthrough()}
            className="rounded px-1.5 py-0.5 text-[var(--text-muted)] hover:bg-[var(--bg-hover)] hover:text-[var(--text-primary)]"
            title="Cancel recording"
          >
            &times;
          </button>
        </div>

        {/* Body */}
        <div className="flex-1 overflow-y-auto px-3 py-3">
          {/* Current app + step counter */}
          <div className="mb-3 flex items-center justify-between text-[11px] text-[var(--text-muted)]">
            <span>{currentApp ? `App: ${currentApp}` : "Waiting for input…"}</span>
            <span>{walkthroughEvents.length} event{walkthroughEvents.length !== 1 ? "s" : ""}</span>
          </div>

          {/* Error banner */}
          {walkthroughError && (
            <div className="mb-3 rounded border border-red-500/30 bg-red-500/10 px-3 py-2 text-[11px] text-red-400">
              {walkthroughError}
            </div>
          )}

          {/* Event feed */}
          <div className="space-y-1">
            {walkthroughEvents.map((event, idx) => {
              const { icon, text } = eventDescription(event);
              return (
                <div key={idx} className="flex items-center gap-2 rounded px-2 py-1 text-xs text-[var(--text-secondary)]">
                  <span className="w-4 text-center opacity-60">{icon}</span>
                  <span className="truncate">{text}</span>
                </div>
              );
            })}
            <div ref={feedEndRef} />
          </div>
        </div>

        {/* Footer */}
        <div className="flex items-center gap-2 border-t border-[var(--border)] px-3 py-2.5">
          <button
            onClick={() => walkthroughStatus === "Paused" ? resumeWalkthrough() : pauseWalkthrough()}
            className="rounded-lg border border-[var(--border)] px-3 py-1.5 text-xs font-medium text-[var(--text-secondary)] hover:bg-[var(--bg-hover)]"
          >
            {walkthroughStatus === "Paused" ? "Resume" : "Pause"}
          </button>
          <button
            onClick={() => stopWalkthrough()}
            className="rounded-lg bg-[var(--accent-coral)] px-3 py-1.5 text-xs font-medium text-white hover:opacity-90"
          >
            Stop
          </button>
          <button
            onClick={() => cancelWalkthrough()}
            className="ml-auto rounded-lg px-3 py-1.5 text-xs text-[var(--text-muted)] hover:text-[var(--text-secondary)]"
          >
            Cancel
          </button>
        </div>
      </div>
    );
  }

  if (walkthroughStatus === "Processing") {
    return (
      <div className="relative flex h-full flex-col border-l border-[var(--border)] bg-[var(--bg-panel)]" style={{ width, minWidth: width }}>
        <div onMouseDown={handleResizeStart} className="absolute left-0 top-0 z-10 h-full w-1.5 cursor-col-resize hover:bg-[var(--accent-coral)]/30 active:bg-[var(--accent-coral)]/40" />
        <div className="flex items-center border-b border-[var(--border)] px-4 py-2.5">
          <h2 className="text-sm font-medium text-[var(--text-primary)]">Processing</h2>
        </div>
        <div className="flex flex-1 flex-col items-center justify-center gap-3">
          <div className="h-5 w-5 animate-spin rounded-full border-2 border-[var(--accent-coral)] border-t-transparent" />
          <span className="text-xs text-[var(--text-secondary)]">Analyzing captured actions…</span>
          <span className="text-[10px] text-[var(--text-muted)]">{walkthroughEvents.length} events recorded</span>
        </div>
      </div>
    );
  }

  // Review mode
  if (walkthroughStatus !== "Review") return null;

  // Build action lookup by node_id for metadata (screenshots, candidates, confidence).
  const actionByNodeId = new Map<string, WalkthroughAction>();
  for (const entry of walkthroughActionNodeMap) {
    const action = walkthroughActions.find((a) => a.id === entry.action_id);
    if (action) actionByNodeId.set(entry.node_id, action);
  }

  const draftNodes = walkthroughDraft?.nodes ?? [];
  const activeNodes = draftNodes.filter(
    (n) => !walkthroughAnnotations.deleted_node_ids.includes(n.id),
  );
  const allDeleted = draftNodes.length > 0 && activeNodes.length === 0;
  const warningCount = walkthroughActions.reduce((sum, a) => sum + a.warnings.length, 0) + walkthroughWarnings.length;

  // Precompute step numbers (only for non-deleted nodes)
  const stepNumbers = new Map<string, number>();
  let step = 0;
  for (const n of draftNodes) {
    if (!walkthroughAnnotations.deleted_node_ids.includes(n.id)) {
      stepNumbers.set(n.id, ++step);
    }
  }

  return (
    <div className="relative flex h-full flex-col border-l border-[var(--border)] bg-[var(--bg-panel)]" style={{ width, minWidth: width }}>
      {/* Resize handle */}
      <div onMouseDown={handleResizeStart} className="absolute left-0 top-0 z-10 h-full w-1.5 cursor-col-resize hover:bg-[var(--accent-coral)]/30 active:bg-[var(--accent-coral)]/40" />

      {/* Header */}
      <div className="flex items-center justify-between border-b border-[var(--border)] px-4 py-2.5">
        <div className="flex items-center gap-2">
          <h2 className="text-sm font-medium text-[var(--text-primary)]">Review Walkthrough</h2>
          <span className="rounded-full bg-[var(--bg-hover)] px-2 py-0.5 text-[10px] text-[var(--text-muted)]">
            {activeNodes.length} step{activeNodes.length !== 1 ? "s" : ""}
          </span>
          {warningCount > 0 && (
            <span className="rounded-full bg-yellow-500/20 px-2 py-0.5 text-[10px] text-yellow-400">
              {warningCount}
            </span>
          )}
        </div>
        <button
          onClick={discardDraft}
          className="rounded px-1.5 py-0.5 text-[var(--text-muted)] hover:bg-[var(--bg-hover)] hover:text-[var(--text-primary)]"
          title="Discard walkthrough"
        >
          &times;
        </button>
      </div>

      {/* Global warnings */}
      {walkthroughWarnings.length > 0 && (
        <div className="border-b border-[var(--border)] px-3 py-2 space-y-1">
          {walkthroughWarnings.map((w, i) => (
            <div key={i} className="rounded bg-yellow-500/10 px-2 py-1 text-[11px] text-yellow-400">
              {w}
            </div>
          ))}
        </div>
      )}

      {/* Error banner */}
      {walkthroughError && (
        <div className="border-b border-[var(--border)] px-3 py-2">
          <div className="rounded border border-red-500/30 bg-red-500/10 px-3 py-2 text-[11px] text-red-400">
            {walkthroughError}
          </div>
        </div>
      )}

      {/* Action list */}
      <div className="flex-1 overflow-y-auto px-3 py-3">
        {draftNodes.length === 0 ? (
          <div className="flex h-full items-center justify-center">
            <p className="text-center text-xs text-[var(--text-muted)]">No actions captured</p>
          </div>
        ) : (
          <div className="space-y-1.5">
            {draftNodes.map((node) => {
              const action = actionByNodeId.get(node.id);
              const isDeleted = walkthroughAnnotations.deleted_node_ids.includes(node.id);
              const isExpanded = walkthroughExpandedAction === node.id;
              const currentStep = stepNumbers.get(node.id) ?? null;
              const { icon, color } = action ? actionIcon(action.kind) : nodeTypeIcon(node.node_type);
              const isLlmAdded = !action && !walkthroughUsedFallback;

              // Get rename if any
              const renameEntry = walkthroughAnnotations.renamed_nodes.find((r) => r.node_id === node.id);
              const defaultLabel = action ? actionLabel(action) : node.name;
              const displayLabel = renameEntry?.new_name || defaultLabel;

              // Get target override if any
              const targetOverride = walkthroughAnnotations.target_overrides.find((o) => o.node_id === node.id);
              const chosenTargetIdx = targetOverride?.chosen_candidate_index ?? 0;

              // Get variable promotion if any
              const variablePromo = walkthroughAnnotations.variable_promotions.find((p) => p.node_id === node.id);

              return (
                <div
                  key={node.id}
                  className={`rounded-lg border transition-all duration-200 ${
                    isDeleted
                      ? "border-[var(--border)] opacity-40"
                      : isExpanded
                        ? "border-[var(--accent-coral)]/30 bg-[var(--bg-hover)]"
                        : "border-[var(--border)] hover:border-[var(--text-muted)]/30"
                  }`}
                >
                  {/* Collapsed row */}
                  <div
                    className="flex cursor-pointer items-center gap-2 px-3 py-2"
                    onClick={() => setWalkthroughExpandedAction(node.id)}
                  >
                    {/* Step number */}
                    <span className="w-5 text-right text-[10px] text-[var(--text-muted)]">
                      {currentStep ?? "—"}
                    </span>

                    {/* Icon */}
                    <span className={`w-4 text-center text-sm ${color}`}>{icon}</span>

                    {/* Label */}
                    <span className={`flex-1 truncate text-xs text-[var(--text-secondary)] ${isDeleted ? "line-through" : ""}`}>
                      {displayLabel}
                    </span>

                    {/* LLM-added badge */}
                    {isLlmAdded && (
                      <span className="rounded bg-purple-500/20 px-1 py-0.5 text-[9px] text-purple-400">LLM</span>
                    )}

                    {/* Confidence dot (only for nodes with an action) */}
                    {action && (
                      <span className={`h-1.5 w-1.5 rounded-full ${confidenceDot(action.confidence)}`} title={action.confidence} />
                    )}

                    {/* Delete / Restore */}
                    {isDeleted ? (
                      <button
                        onClick={(e) => { e.stopPropagation(); restoreNode(node.id); }}
                        className="rounded px-1.5 py-0.5 text-[10px] text-[var(--text-muted)] hover:text-green-400"
                        title="Restore"
                      >
                        Restore
                      </button>
                    ) : (
                      <button
                        onClick={(e) => { e.stopPropagation(); deleteNode(node.id); }}
                        className="rounded px-1 py-0.5 text-[var(--text-muted)] hover:text-red-400"
                        title="Delete step"
                      >
                        <span className="text-xs">&#x1F5D1;</span>
                      </button>
                    )}
                  </div>

                  {/* Expanded details */}
                  {isExpanded && !isDeleted && (
                    <div className="border-t border-[var(--border)] px-3 py-2.5 space-y-3">
                      {/* Rename field */}
                      <div>
                        <label className="mb-1 block text-[10px] text-[var(--text-muted)]">Name</label>
                        <input
                          type="text"
                          value={renameEntry?.new_name ?? defaultLabel}
                          onChange={(e) => renameNode(node.id, e.target.value)}
                          className="w-full rounded border border-[var(--border)] bg-[var(--bg-input)] px-2 py-1 text-xs text-[var(--text-primary)] outline-none focus:border-[var(--accent-coral)]"
                        />
                      </div>

                      {/* Target candidates (Click nodes with action metadata) */}
                      {action && action.kind.type === "Click" && action.target_candidates.length > 0 && (
                        <div>
                          <label className="mb-1 block text-[10px] text-[var(--text-muted)]">Click Target</label>
                          <div className="space-y-1">
                            {action.target_candidates.map((candidate, ci) => (
                              <label
                                key={ci}
                                className={`flex cursor-pointer items-center gap-2 rounded px-2 py-1 text-xs transition-colors ${
                                  ci === chosenTargetIdx
                                    ? "bg-[var(--accent-coral)]/10 text-[var(--text-primary)]"
                                    : "text-[var(--text-secondary)] hover:bg-[var(--bg-hover)]"
                                }`}
                              >
                                <input
                                  type="radio"
                                  name={`target-${node.id}`}
                                  checked={ci === chosenTargetIdx}
                                  onChange={() => overrideTarget(node.id, ci)}
                                  className="accent-[var(--accent-coral)]"
                                />
                                <span>{targetCandidateIcon(candidate)}</span>
                                <span className="truncate">{targetCandidateLabel(candidate)}</span>
                              </label>
                            ))}
                          </div>
                        </div>
                      )}

                      {/* Variable promotion (TypeText nodes) */}
                      {(action ? action.kind.type === "TypeText" : node.node_type.type === "TypeText") && (
                        <div>
                          <label className="flex items-center gap-2 text-xs text-[var(--text-secondary)]">
                            <input
                              type="checkbox"
                              checked={!!variablePromo}
                              onChange={(e) => {
                                if (e.target.checked) {
                                  promoteToVariable(node.id, "");
                                } else {
                                  removeVariablePromotion(node.id);
                                }
                              }}
                              className="accent-[var(--accent-coral)]"
                            />
                            Promote to variable
                          </label>
                          {variablePromo && (
                            <input
                              type="text"
                              value={variablePromo.variable_name}
                              onChange={(e) => promoteToVariable(node.id, e.target.value)}
                              placeholder="variable_name"
                              className="mt-1 w-full rounded border border-[var(--border)] bg-[var(--bg-input)] px-2 py-1 text-xs text-[var(--text-primary)] outline-none focus:border-[var(--accent-coral)]"
                            />
                          )}
                        </div>
                      )}

                      {/* Per-action warnings */}
                      {action && action.warnings.length > 0 && (
                        <div className="space-y-1">
                          {action.warnings.map((w, wi) => (
                            <div key={wi} className="rounded bg-yellow-500/10 px-2 py-1 text-[11px] text-yellow-400">
                              {w}
                            </div>
                          ))}
                        </div>
                      )}

                      {/* Screenshot thumbnail */}
                      {action && action.artifact_paths.length > 0 && (
                        <div>
                          <label className="mb-1 block text-[10px] text-[var(--text-muted)]">Screenshot</label>
                          <img
                            src={convertFileSrc(action.artifact_paths[0])}
                            alt="Action screenshot"
                            className="max-h-32 rounded border border-[var(--border)] object-contain"
                          />
                        </div>
                      )}
                    </div>
                  )}
                </div>
              );
            })}
          </div>
        )}
      </div>

      {/* Footer */}
      <div className="flex items-center gap-2 border-t border-[var(--border)] px-3 py-2.5">
        <button
          onClick={discardDraft}
          className="rounded-lg border border-[var(--border)] px-3 py-1.5 text-xs text-[var(--text-muted)] hover:bg-[var(--bg-hover)] hover:text-[var(--text-secondary)]"
        >
          Discard
        </button>
        {allDeleted && (
          <span className="text-[10px] text-[var(--text-muted)]">All steps deleted</span>
        )}
        <button
          onClick={applyDraftToCanvas}
          disabled={allDeleted || draftNodes.length === 0}
          className="ml-auto rounded-lg bg-[var(--accent-coral)] px-4 py-1.5 text-xs font-medium text-white hover:opacity-90 disabled:opacity-40"
        >
          Apply
        </button>
      </div>
    </div>
  );
}
