import { useState, useCallback } from "react";
import { convertFileSrc } from "@tauri-apps/api/core";
import { useStore } from "../store/useAppStore";
import { useHorizontalResize } from "../hooks/useHorizontalResize";
import type { AppKind, WalkthroughAction, TargetCandidate, Node } from "../bindings";
import { APP_KIND_LABELS, usesCdp } from "../utils/appKind";
import { buildActionByNodeId } from "../store/slices/walkthroughSlice";
import { ImageLightbox, CrosshairOverlay, type LightboxImage } from "./ImageLightbox";

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
      const idx = preferredTargetIndex(action.target_candidates);
      const best = action.target_candidates[idx];
      if (best && best.type !== "Coordinates" && best.type !== "ImageCrop") {
        const label = (best.type === "OcrText" || best.type === "CdpElement") ? best.text : best.label;
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
    case "AccessibilityLabel": return `"${candidate.label}"`;
    case "VlmLabel": return `"${candidate.label}"`;
    case "OcrText": return `"${candidate.text}"`;
    case "ImageCrop": return "Image crop";
    case "Coordinates": return `(${candidate.x}, ${candidate.y})`;
    case "CdpElement": return `"${candidate.text}"`;
  }
}

function targetCandidateMethod(candidate: TargetCandidate): string {
  switch (candidate.type) {
    case "AccessibilityLabel": return "Accessibility";
    case "VlmLabel": return "Vision model";
    case "OcrText": return "OCR";
    case "ImageCrop": return "Image template";
    case "Coordinates": return "Screen coordinates";
    case "CdpElement": return "DevTools DOM";
  }
}

/** Accessibility roles that represent specific, actionable UI elements (mirrors Rust ACTIONABLE_AX_ROLES). */
const ACTIONABLE_AX_ROLES = new Set([
  "AXButton", "AXCheckBox", "AXComboBox", "AXDisclosureTriangle", "AXIncrementor",
  "AXLink", "AXMenuButton", "AXMenuItem", "AXPopUpButton", "AXRadioButton",
  "AXSegmentedControl", "AXSlider", "AXStaticText", "AXTab", "AXTabButton",
  "AXTextField", "AXTextArea", "AXToggle", "AXToolbarButton",
]);

/** Find the index of the preferred target candidate, mirroring backend `synthesize_draft` logic.
 *  Priority: actionable AX label > VlmLabel/OcrText > ImageCrop > Coordinates. */
function preferredTargetIndex(candidates: TargetCandidate[]): number {
  // CDP-verified elements are most reliable
  const cdpIdx = candidates.findIndex((c) => c.type === "CdpElement");
  if (cdpIdx >= 0) return cdpIdx;
  const idx = candidates.findIndex((c) => {
    if (c.type === "AccessibilityLabel") return ACTIONABLE_AX_ROLES.has(c.role ?? "");
    return c.type === "VlmLabel" || c.type === "OcrText";
  });
  if (idx >= 0) return idx;
  // No text target — prefer ImageCrop over Coordinates (matching draft synthesis).
  const cropIdx = candidates.findIndex((c) => c.type === "ImageCrop");
  if (cropIdx >= 0) return cropIdx;
  const coordIdx = candidates.findIndex((c) => c.type === "Coordinates");
  return coordIdx >= 0 ? coordIdx : 0;
}

function targetCandidateIcon(candidate: TargetCandidate): string {
  switch (candidate.type) {
    case "AccessibilityLabel": return "\u{1F3F7}";
    case "VlmLabel": return "\u{1F52D}";
    case "OcrText": return "\u{1F441}";
    case "ImageCrop": return "\u{1F5BC}";
    case "Coordinates": return "\u{1F4CD}";
    case "CdpElement": return "\u{1F310}";
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

/** Compute crosshair position as percent of image dimensions, or null if not applicable. */
function computeCrosshairPercent(
  action: WalkthroughAction,
  naturalWidth: number,
  naturalHeight: number,
): { xPercent: number; yPercent: number } | null {
  if (action.kind.type !== "Click" || !action.screenshot_meta) return null;
  const meta = action.screenshot_meta;
  const px = (action.kind.x - meta.origin_x) * meta.scale;
  const py = (action.kind.y - meta.origin_y) * meta.scale;
  if (naturalWidth <= 0 || naturalHeight <= 0) return null;
  return {
    xPercent: (px / naturalWidth) * 100,
    yPercent: (py / naturalHeight) * 100,
  };
}

export function WalkthroughPanel() {
  const walkthroughStatus = useStore((s) => s.walkthroughStatus);
  const walkthroughEvents = useStore((s) => s.walkthroughEvents);
  const walkthroughError = useStore((s) => s.walkthroughError);
  const walkthroughActions = useStore((s) => s.walkthroughActions);
  const walkthroughAnnotations = useStore((s) => s.walkthroughAnnotations);
  const walkthroughExpandedAction = useStore((s) => s.walkthroughExpandedAction);
  const walkthroughWarnings = useStore((s) => s.walkthroughWarnings);
  const setWalkthroughExpandedAction = useStore((s) => s.setWalkthroughExpandedAction);
  const walkthroughDraft = useStore((s) => s.walkthroughDraft);
  const walkthroughActionNodeMap = useStore((s) => s.walkthroughActionNodeMap);
  const deleteNode = useStore((s) => s.deleteNode);
  const restoreNode = useStore((s) => s.restoreNode);
  const renameNode = useStore((s) => s.renameNode);
  const overrideTarget = useStore((s) => s.overrideTarget);
  const promoteToVariable = useStore((s) => s.promoteToVariable);
  const removeVariablePromotion = useStore((s) => s.removeVariablePromotion);
  const applyDraftToCanvas = useStore((s) => s.applyDraftToCanvas);
  const discardDraft = useStore((s) => s.discardDraft);
  const walkthroughPanelOpen = useStore((s) => s.walkthroughPanelOpen);
  const setWalkthroughPanelOpen = useStore((s) => s.setWalkthroughPanelOpen);
  const assistantOpen = useStore((s) => s.assistantOpen);

  const [lightboxActionId, setLightboxActionId] = useState<string | null>(null);
  const [crosshairs, setCrosshairs] = useState<Map<string, { xPercent: number; yPercent: number }>>(new Map());

  const onThumbnailLoad = useCallback((action: WalkthroughAction, img: HTMLImageElement) => {
    const result = computeCrosshairPercent(action, img.naturalWidth, img.naturalHeight);
    if (!result) return;
    setCrosshairs((prev) => {
      const existing = prev.get(action.id);
      if (existing?.xPercent === result.xPercent && existing?.yPercent === result.yPercent) {
        return prev;
      }
      return new Map(prev).set(action.id, result);
    });
  }, []);

  const { width, handleResizeStart } = useHorizontalResize();

  // Recording/Paused/Processing states are now handled by RecordingBar overlay
  if (walkthroughStatus !== "Review") return null;

  // Hide while assistant panel is open to avoid rendering both side panels.
  if (assistantOpen) return null;

  // Hide when user closed the panel via X. State is preserved; can reopen from toolbar.
  if (!walkthroughPanelOpen) return null;

  // Build action lookup by node_id for metadata (screenshots, candidates, confidence).
  const actionByNodeId = buildActionByNodeId(walkthroughActionNodeMap, walkthroughActions);

  // Build app_kind map from LaunchApp/FocusWindow actions so Click nodes can
  // know whether they're targeting an Electron/Chrome app.
  const appKindMap = new Map<string, AppKind>();
  for (const a of walkthroughActions) {
    if (a.kind.type === "LaunchApp" || a.kind.type === "FocusWindow") {
      appKindMap.set(a.kind.app_name, a.kind.app_kind);
    }
  }

  const draftNodes = walkthroughDraft?.nodes ?? [];
  const deletedSet = new Set(walkthroughAnnotations.deleted_node_ids);
  const renameMap = new Map(walkthroughAnnotations.renamed_nodes.map((r) => [r.node_id, r]));
  const targetMap = new Map(walkthroughAnnotations.target_overrides.map((o) => [o.node_id, o]));
  const varPromoMap = new Map(walkthroughAnnotations.variable_promotions.map((p) => [p.node_id, p]));

  const activeNodes = draftNodes.filter((n) => !deletedSet.has(n.id));
  const allDeleted = draftNodes.length > 0 && activeNodes.length === 0;
  const warningCount = walkthroughActions.reduce((sum, a) => sum + a.warnings.length, 0) + walkthroughWarnings.length;

  // Precompute step numbers (only for non-deleted nodes)
  const stepNumbers = new Map<string, number>();
  let step = 0;
  for (const n of draftNodes) {
    if (!deletedSet.has(n.id)) {
      stepNumbers.set(n.id, ++step);
    }
  }

  // Precompute lightbox image if one is open
  const lightboxImage: LightboxImage | null = (() => {
    if (!lightboxActionId) return null;
    const action = walkthroughActions.find((a) => a.id === lightboxActionId);
    if (!action || action.artifact_paths.length === 0) return null;
    return {
      src: convertFileSrc(action.artifact_paths[0]),
      filename: action.artifact_paths[0].split("/").pop() ?? "screenshot",
      crosshair: crosshairs.get(action.id),
    };
  })();

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
          onClick={() => setWalkthroughPanelOpen(false)}
          className="rounded px-1.5 py-0.5 text-[var(--text-muted)] hover:bg-[var(--bg-hover)] hover:text-[var(--text-primary)]"
          title="Close panel"
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
              const isDeleted = deletedSet.has(node.id);
              const isExpanded = walkthroughExpandedAction === node.id;
              const currentStep = stepNumbers.get(node.id) ?? null;
              const { icon, color } = action ? actionIcon(action.kind) : nodeTypeIcon(node.node_type);

              // Get rename if any
              const renameEntry = renameMap.get(node.id);
              const defaultLabel = action ? actionLabel(action) : node.name;
              const displayLabel = renameEntry?.new_name || defaultLabel;

              // Get target override if any
              const targetOverride = targetMap.get(node.id);
              const chosenTargetIdx = targetOverride?.chosen_candidate_index ?? (action ? preferredTargetIndex(action.target_candidates) : 0);

              // Get variable promotion if any
              const variablePromo = varPromoMap.get(node.id);

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
                      {action && action.kind.type === "Click" && action.target_candidates.length > 0 && (() => {
                        const actionAppKind = action.app_name ? appKindMap.get(action.app_name) : undefined;
                        const isCdpApp = actionAppKind ? usesCdp(actionAppKind) : false;
                        // For Electron/Chrome apps, hide non-actionable AX labels (e.g. AXWindow)
                        // since native accessibility is unreliable — DevTools is used at runtime instead.
                        const displayCandidates = action.target_candidates
                          .map((candidate, i) => ({ candidate, originalIndex: i }))
                          .filter(({ candidate }) => {
                            if (!isCdpApp) return true;
                            if (candidate.type === "AccessibilityLabel" && !ACTIONABLE_AX_ROLES.has(candidate.role ?? "")) return false;
                            return true;
                          });
                        return (
                          <div>
                            <label className="mb-1 block text-[10px] text-[var(--text-muted)]">Click Target</label>
                            <div className="space-y-1">
                              {displayCandidates.map(({ candidate, originalIndex }) => (
                                <label
                                  key={originalIndex}
                                  className={`flex cursor-pointer items-center gap-2 rounded px-2 py-1 text-xs transition-colors ${
                                    originalIndex === chosenTargetIdx
                                      ? "bg-[var(--accent-coral)]/10 text-[var(--text-primary)]"
                                      : "text-[var(--text-secondary)] hover:bg-[var(--bg-hover)]"
                                  }`}
                                >
                                  <input
                                    type="radio"
                                    name={`target-${node.id}`}
                                    checked={originalIndex === chosenTargetIdx}
                                    onChange={() => overrideTarget(node.id, originalIndex)}
                                    className="accent-[var(--accent-coral)]"
                                  />
                                  <span>{targetCandidateIcon(candidate)}</span>
                                  <span className="truncate">{targetCandidateLabel(candidate)}</span>
                                  <span className="ml-auto shrink-0 text-[10px] text-[var(--text-muted)]">{targetCandidateMethod(candidate)}</span>
                                </label>
                              ))}
                            </div>
                            {isCdpApp && (
                              <p className="mt-1.5 text-[10px] text-[var(--text-muted)]">
                                {actionAppKind ? APP_KIND_LABELS[actionAppKind] : "DevTools"} — targeting used at runtime
                              </p>
                            )}
                            {/* Crop thumbnail for selected ImageCrop candidate */}
                            {(() => {
                              const chosen = action.target_candidates[chosenTargetIdx];
                              if (chosen?.type === "ImageCrop") {
                                return (
                                  <img
                                    src={convertFileSrc(chosen.path)}
                                    alt="Click crop"
                                    className="mt-1 h-16 w-16 rounded border border-[var(--border)] object-contain"
                                    onError={(e) => {
                                      // Fall back to inline base64 if the artifact file is missing.
                                      if (chosen.image_b64) {
                                        (e.target as HTMLImageElement).src =
                                          `data:image/jpeg;base64,${chosen.image_b64}`;
                                      }
                                    }}
                                  />
                                );
                              }
                              return null;
                            })()}
                          </div>
                        );
                      })()}

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
                      {action && action.artifact_paths.length > 0 && (() => {
                        const crosshair = crosshairs.get(action.id);
                        return (
                          <div>
                            <label className="mb-1 block text-[10px] text-[var(--text-muted)]">Screenshot</label>
                            <div
                              className="relative inline-block cursor-pointer"
                              onClick={() => setLightboxActionId(action.id)}
                            >
                              <img
                                src={convertFileSrc(action.artifact_paths[0])}
                                alt="Action screenshot"
                                className="max-h-32 rounded border border-[var(--border)] object-contain"
                                onLoad={(e) => onThumbnailLoad(action, e.currentTarget)}
                              />
                              {crosshair && (
                                <CrosshairOverlay xPercent={crosshair.xPercent} yPercent={crosshair.yPercent} />
                              )}
                            </div>
                          </div>
                        );
                      })()}
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
          className="rounded-lg border border-red-500/40 px-3 py-1.5 text-xs font-medium text-red-400 hover:bg-red-500/10"
        >
          Cancel
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

      {/* Screenshot lightbox */}
      {lightboxImage && (
        <ImageLightbox
          images={[lightboxImage]}
          index={0}
          onClose={() => setLightboxActionId(null)}
          onNavigate={() => {}}
        />
      )}
    </div>
  );
}
