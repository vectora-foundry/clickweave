import { useState, useCallback, useRef } from "react";
import { convertFileSrc } from "@tauri-apps/api/core";
import { useStore } from "../store/useAppStore";
import { useHorizontalResize } from "../hooks/useHorizontalResize";
import type { AppKind, WalkthroughAction } from "../bindings";
import { APP_KIND_LABELS, usesCdp } from "../utils/appKind";
import { ImageLightbox, CrosshairOverlay, type LightboxImage } from "./ImageLightbox";
import { computeAppGroups, isValidItemDrop, type AppGroup, type RenderItem } from "../utils/walkthroughGrouping";
import {
  ACTIONABLE_AX_ROLES,
  actionIcon,
  actionLabel,
  computeCrosshairPercent,
  confidenceDot,
  nodeTypeIcon,
  preferredTargetIndex,
  targetCandidateIcon,
  targetCandidateLabel,
  targetCandidateMethod,
} from "../utils/walkthroughFormatting";

const DND_ITEM_ID = "application/x-item-id";
const DND_GROUP_INDEX = "application/x-group-index";

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
  const walkthroughNodeOrder = useStore((s) => s.walkthroughNodeOrder);
  const keepCandidate = useStore((s) => s.keepCandidate);
  const dismissCandidate = useStore((s) => s.dismissCandidate);
  const deleteNode = useStore((s) => s.deleteNode);
  const restoreNode = useStore((s) => s.restoreNode);
  const renameNode = useStore((s) => s.renameNode);
  const overrideTarget = useStore((s) => s.overrideTarget);
  const promoteToVariable = useStore((s) => s.promoteToVariable);
  const removeVariablePromotion = useStore((s) => s.removeVariablePromotion);
  const applyDraftToCanvas = useStore((s) => s.applyDraftToCanvas);
  const discardDraft = useStore((s) => s.discardDraft);
  const reorderNode = useStore((s) => s.reorderNode);
  const reorderGroup = useStore((s) => s.reorderGroup);
  const walkthroughPanelOpen = useStore((s) => s.walkthroughPanelOpen);
  const setWalkthroughPanelOpen = useStore((s) => s.setWalkthroughPanelOpen);
  const assistantOpen = useStore((s) => s.assistantOpen);

  const [lightboxActionId, setLightboxActionId] = useState<string | null>(null);
  const [crosshairs, setCrosshairs] = useState<Map<string, { xPercent: number; yPercent: number }>>(new Map());
  const [dragOverIndex, setDragOverIndexRaw] = useState<number | null>(null);
  const dragOverRef = useRef<number | null>(null);
  const setDragOverIndex = useCallback((idx: number | null) => {
    if (dragOverRef.current === idx) return;
    dragOverRef.current = idx;
    setDragOverIndexRaw(idx);
  }, []);

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

  const warningCount = walkthroughActions.reduce((sum, a) => sum + a.warnings.length, 0) + walkthroughWarnings.length;

  // Compute groups from the ordered list
  const groups = computeAppGroups(
    walkthroughNodeOrder, draftNodes, walkthroughActions,
    walkthroughActionNodeMap,
  );

  // Precompute step numbers from the flat group items (only for non-deleted nodes)
  const stepNumbers = new Map<string, number>();
  let step = 0;
  for (const group of groups) {
    for (const item of group.items) {
      if (item.type === "node" && !deletedSet.has(item.id)) {
        stepNumbers.set(item.id, ++step);
      }
    }
  }

  const activeNodeCount = step;
  const allDeleted = draftNodes.length > 0 && activeNodeCount === 0;

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

  // Helper: compute flat index for a position within groups
  function flatIdxAt(groupIndex: number, itemIndex: number): number {
    let idx = 0;
    for (let gi = 0; gi < groupIndex; gi++) idx += groups[gi].items.length;
    return idx + itemIndex;
  }

  // Helper: handle drop event for both item and group drags
  function handleDrop(e: React.DragEvent, flatIdx: number, targetGroupIndex: number) {
    e.preventDefault();
    setDragOverIndex(null);

    const dragItemId = e.dataTransfer.getData(DND_ITEM_ID);
    const groupIdxStr = e.dataTransfer.getData(DND_GROUP_INDEX);

    if (dragItemId) {
      if (!isValidItemDrop(dragItemId, flatIdx, groups)) return;

      const fromIdx = walkthroughNodeOrder.indexOf(dragItemId);
      if (fromIdx >= 0) {
        const activeIds = groups.flatMap((g) => g.items.map((i) => i.id));
        const targetId = activeIds[flatIdx];
        const toIdx = targetId ? walkthroughNodeOrder.indexOf(targetId) : walkthroughNodeOrder.length;
        if (toIdx >= 0) reorderNode(fromIdx, toIdx > fromIdx ? toIdx - 1 : toIdx);
      }
    } else if (groupIdxStr) {
      const fromGroupIdx = parseInt(groupIdxStr, 10);
      if (!isNaN(fromGroupIdx) && fromGroupIdx !== targetGroupIndex) {
        reorderGroup(fromGroupIdx, targetGroupIndex);
      }
    }
  }

  // Render a single item (node or candidate) within a group
  function renderGroupItem(item: RenderItem, group: AppGroup, groupIndex: number, itemIndex: number) {
    const flatIdx = flatIdxAt(groupIndex, itemIndex);
    const isItemAnchor = group.anchorIndex >= 0 && group.items[group.anchorIndex].id === item.id;

    const borderLeftStyle = group.appName
      ? { borderLeftColor: group.color, borderLeftWidth: 3, borderLeftStyle: "solid" as const }
      : {};

    // Drop zone before this item
    const dropZone = (
      <div
        className="relative h-1"
        onDragOver={(e) => { e.preventDefault(); setDragOverIndex(flatIdx); }}
        onDragLeave={() => setDragOverIndex(null)}
        onDrop={(e) => handleDrop(e, flatIdx, groupIndex)}
      >
        {dragOverIndex === flatIdx && (
          <div className="absolute left-0 right-0 top-0 h-0.5 bg-[var(--accent-coral)]" />
        )}
      </div>
    );

    if (item.type === "candidate") {
      const { icon, color } = actionIcon(item.action.kind);
      const label = actionLabel(item.action);
      return (
        <div key={`candidate-${item.action.id}`}>
          {dropZone}
          <div
            className="rounded-lg border border-dashed border-purple-500/50 opacity-60"
            style={borderLeftStyle}
          >
            <div className="flex items-center gap-2 px-3 py-2">
              <span className="w-5 text-right text-[10px] text-[var(--text-muted)]">?</span>
              <span className={`w-4 text-center text-sm ${color}`}>{icon}</span>
              <span className="truncate text-xs text-[var(--text-secondary)]">{label}</span>
              <span className="ml-1 rounded bg-purple-900/50 px-1.5 py-0.5 text-[10px] text-purple-300">
                candidate
              </span>
              <div className="flex gap-1 ml-auto">
                <button
                  onClick={() => keepCandidate(item.action.id)}
                  className="rounded bg-green-700 px-2 py-0.5 text-[10px] text-white hover:bg-green-600"
                >
                  Keep
                </button>
                <button
                  onClick={() => dismissCandidate(item.action.id)}
                  className="rounded bg-[var(--bg-input)] px-2 py-0.5 text-[10px] text-[var(--text-muted)] hover:bg-red-500/20"
                >
                  Dismiss
                </button>
              </div>
            </div>
          </div>
        </div>
      );
    }

    const { node, action } = item;
    const isDeleted = deletedSet.has(node.id);
    const isExpanded = walkthroughExpandedAction === node.id;
    const currentStep = stepNumbers.get(node.id) ?? null;
    const { icon, color } = action ? actionIcon(action.kind) : nodeTypeIcon(node.node_type);

    const renameEntry = renameMap.get(node.id);
    const defaultLabel = action ? actionLabel(action) : node.name;
    const displayLabel = renameEntry?.new_name || defaultLabel;

    const targetOverride = targetMap.get(node.id);
    const chosenTargetIdx = targetOverride?.chosen_candidate_index ?? (action ? preferredTargetIndex(action.target_candidates) : 0);

    const variablePromo = varPromoMap.get(node.id);

    return (
      <div key={node.id} data-item-id={node.id}>
        {dropZone}
        <div
          className={`rounded-lg border transition-all duration-200 ${
            isDeleted
              ? "border-[var(--border)] opacity-40"
              : isExpanded
                ? "border-[var(--accent-coral)]/30 bg-[var(--bg-hover)]"
                : "border-[var(--border)] hover:border-[var(--text-muted)]/30"
          }`}
          style={borderLeftStyle}
        >
          {/* Collapsed row */}
          <div
            className="group flex cursor-pointer items-center gap-2 px-3 py-2"
            onClick={() => setWalkthroughExpandedAction(node.id)}
          >
            {/* Drag handle (hidden for anchors) */}
            {isItemAnchor ? (
              <span className="w-3" />
            ) : (
              <span
                className="w-3 text-[10px] text-[var(--text-muted)] opacity-0 group-hover:opacity-100 transition-opacity cursor-grab"
                draggable
                onDragStart={(e) => {
                  e.stopPropagation();
                  e.dataTransfer.setData(DND_ITEM_ID, item.id);
                  e.dataTransfer.effectAllowed = "move";
                  const card = (e.currentTarget as HTMLElement).closest("[data-item-id]");
                  if (card) (card as HTMLElement).style.opacity = "0.4";
                }}
                onDragEnd={(e) => {
                  const card = (e.currentTarget as HTMLElement).closest("[data-item-id]");
                  if (card) (card as HTMLElement).style.opacity = "";
                }}
              >
                &#x2261;
              </span>
            )}

            {/* Step number */}
            <span className="w-5 text-right text-[10px] text-[var(--text-muted)]">
              {currentStep ?? "\u2014"}
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

              {/* Target candidates (Click/Hover nodes with action metadata) */}
              {action && (action.kind.type === "Click" || action.kind.type === "Hover") && action.target_candidates.length > 0 && (() => {
                const actionAppKind = action.app_name ? appKindMap.get(action.app_name) : undefined;
                const isCdpApp = actionAppKind ? usesCdp(actionAppKind) : false;
                const displayCandidates = action.target_candidates
                  .map((candidate, i) => ({ candidate, originalIndex: i }))
                  .filter(({ candidate }) => {
                    if (!isCdpApp) return true;
                    if (candidate.type === "AccessibilityLabel" && !ACTIONABLE_AX_ROLES.has(candidate.role ?? "")) return false;
                    return true;
                  });
                return (
                  <div>
                    <label className="mb-1 block text-[10px] text-[var(--text-muted)]">{action.kind.type === "Hover" ? "Hover" : "Click"} Target</label>
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
                    {(() => {
                      const chosen = action.target_candidates[chosenTargetIdx];
                      if (chosen?.type === "ImageCrop") {
                        return (
                          <img
                            src={convertFileSrc(chosen.path)}
                            alt="Click crop"
                            className="mt-1 h-16 w-16 rounded border border-[var(--border)] object-contain"
                            onError={(e) => {
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
      </div>
    );
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
            {activeNodeCount} step{activeNodeCount !== 1 ? "s" : ""}
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
        {draftNodes.length === 0 && groups.length === 0 ? (
          <div className="flex h-full items-center justify-center">
            <p className="text-center text-xs text-[var(--text-muted)]">No actions captured</p>
          </div>
        ) : (
          <div className="space-y-1.5">
            {groups.map((group, groupIndex) => (
              <div key={`${group.appName ?? "ungrouped"}-${groupIndex}`} className="space-y-0">
                {/* Group header (only for named groups with >1 item or an anchor) */}
                {group.appName && (group.items.length > 1 || group.anchorIndex >= 0) && (
                  <div
                    className="flex items-center gap-2 px-2 py-1 cursor-grab active:cursor-grabbing group/header"
                    draggable
                    onDragStart={(e) => {
                      e.dataTransfer.setData(DND_GROUP_INDEX, String(groupIndex));
                      e.dataTransfer.effectAllowed = "move";
                      (e.currentTarget as HTMLElement).style.opacity = "0.4";
                    }}
                    onDragEnd={(e) => {
                      (e.currentTarget as HTMLElement).style.opacity = "";
                    }}
                  >
                    <span className="text-[10px] text-[var(--text-muted)] opacity-0 group-hover/header:opacity-100 transition-opacity cursor-grab">&#x2261;</span>
                    <span
                      className="text-[10px] font-medium truncate"
                      style={{ color: group.color }}
                    >
                      {group.appName}
                    </span>
                    <span className="text-[10px] text-[var(--text-muted)]">
                      {group.items.length}
                    </span>
                  </div>
                )}

                {/* Items */}
                {group.items.map((item, itemIndex) =>
                  renderGroupItem(item, group, groupIndex, itemIndex)
                )}
              </div>
            ))}

            {/* Trailing drop zone */}
            <div
              className="h-4"
              onDragOver={(e) => {
                e.preventDefault();
                setDragOverIndex(groups.reduce((sum, g) => sum + g.items.length, 0));
              }}
              onDragLeave={() => setDragOverIndex(null)}
              onDrop={(e) => {
                e.preventDefault();
                setDragOverIndex(null);
                const itemId = e.dataTransfer.getData(DND_ITEM_ID);
                const groupIdxStr = e.dataTransfer.getData(DND_GROUP_INDEX);
                if (itemId) {
                  const fromIdx = walkthroughNodeOrder.indexOf(itemId);
                  if (fromIdx >= 0) reorderNode(fromIdx, walkthroughNodeOrder.length - 1);
                } else if (groupIdxStr) {
                  const fromGroupIdx = parseInt(groupIdxStr, 10);
                  if (!isNaN(fromGroupIdx)) reorderGroup(fromGroupIdx, groups.length - 1);
                }
              }}
            />
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
