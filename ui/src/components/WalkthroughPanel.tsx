import { useState, useCallback, useRef, useEffect } from "react";
import { convertFileSrc } from "@tauri-apps/api/core";
import { useStore } from "../store/useAppStore";
import { useWalkthrough } from "../hooks/useWalkthrough";
import { useHorizontalResize } from "../hooks/useHorizontalResize";
import type { AppKind, WalkthroughAction } from "../bindings";
import { APP_KIND_LABELS, usesCdp } from "../utils/appKind";
import { ImageLightbox, CrosshairOverlay, type LightboxImage } from "./ImageLightbox";
import { computeAppGroups, type AppGroup, type RenderItem } from "../utils/walkthroughGrouping";
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

const DND_GROUP_INDEX = "application/x-group-index";

function groupBorderStyle(group: AppGroup): React.CSSProperties {
  return group.appName
    ? { borderLeftColor: group.color, borderLeftWidth: 3, borderLeftStyle: "solid" }
    : {};
}

export function WalkthroughPanel() {
  const walkthrough = useWalkthrough();
  const assistantOpen = useStore((s) => s.assistantOpen);

  const [lightboxActionId, setLightboxActionId] = useState<string | null>(null);
  const [crosshairs, setCrosshairs] = useState<Map<string, { xPercent: number; yPercent: number }>>(new Map());
  const [dragOverIndex, setDragOverIndex] = useState<number | null>(null);

  // Pointer-based item drag state
  type ItemDrag = {
    id: string;
    groupIndex: number;
    originalIndex: number; // index within group.items (non-candidate nodes only)
    hoverIndex: number;    // where the card would land
    y: number;             // pointer Y (viewport)
    offsetY: number;       // pointer offset from card top
    cardRect: DOMRect;     // original card bounding rect
  };
  const [itemDrag, setItemDrag] = useState<ItemDrag | null>(null);
  const itemDragRef = useRef<ItemDrag | null>(null);
  const cardRefsMap = useRef<Map<string, HTMLElement>>(new Map());

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

  // Pointer move/up handlers for item drag
  useEffect(() => {
    if (!itemDrag) return;
    const onPointerMove = (e: PointerEvent) => {
      const drag = itemDragRef.current;
      if (!drag) return;
      const newY = e.clientY;
      // Compute hover index by comparing pointer Y to card midpoints in the group
      const group = groups[drag.groupIndex];
      if (!group) return;
      const nodeItems = group.items.filter((i) => i.type === "node");
      // Find minimum draggable index (after all anchors)
      let minIdx = 0;
      for (let i = 0; i < nodeItems.length; i++) {
        const gi = group.items.indexOf(nodeItems[i]);
        if (group.anchorIndices.has(gi)) minIdx = i + 1;
      }
      let hoverIndex = drag.originalIndex;
      for (let i = 0; i < nodeItems.length; i++) {
        if (i === drag.originalIndex) continue;
        const el = cardRefsMap.current.get(nodeItems[i].id);
        if (!el) continue;
        const rect = el.getBoundingClientRect();
        const mid = rect.top + rect.height / 2;
        if (i < drag.originalIndex && newY < mid) { hoverIndex = i; break; }
        if (i > drag.originalIndex && newY > mid) { hoverIndex = i; }
      }
      hoverIndex = Math.max(hoverIndex, minIdx);
      const updated = { ...drag, y: newY, hoverIndex };
      itemDragRef.current = updated;
      setItemDrag(updated);
    };
    const onPointerUp = () => {
      const drag = itemDragRef.current;
      if (drag && drag.hoverIndex !== drag.originalIndex) {
        // Map group-local indices to walkthrough.nodeOrder indices
        const group = groups[drag.groupIndex];
        const nodeItems = group.items.filter((i) => i.type === "node");
        const fromId = nodeItems[drag.originalIndex]?.id;
        const toId = nodeItems[drag.hoverIndex]?.id;
        if (fromId && toId) {
          const fromIdx = walkthrough.nodeOrder.indexOf(fromId);
          const toIdx = walkthrough.nodeOrder.indexOf(toId);
          if (fromIdx >= 0 && toIdx >= 0) walkthrough.reorderNode(fromIdx, toIdx);
        }
      }
      itemDragRef.current = null;
      setItemDrag(null);
    };
    window.addEventListener("pointermove", onPointerMove);
    window.addEventListener("pointerup", onPointerUp);
    return () => {
      window.removeEventListener("pointermove", onPointerMove);
      window.removeEventListener("pointerup", onPointerUp);
    };
  // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [itemDrag !== null]);

  // Recording/Paused/Processing states are now handled by RecordingBar overlay
  if (walkthrough.status !== "Review") return null;

  // Hide while assistant panel is open to avoid rendering both side panels.
  if (assistantOpen) return null;

  // Hide when user closed the panel via X. State is preserved; can reopen from toolbar.
  if (!walkthrough.panelOpen) return null;

  // Build app_kind map from LaunchApp/FocusWindow actions so Click nodes can
  // know whether they're targeting an Electron/Chrome app.
  const appKindMap = new Map<string, AppKind>();
  for (const a of walkthrough.actions) {
    if (a.kind.type === "LaunchApp" || a.kind.type === "FocusWindow") {
      appKindMap.set(a.kind.app_name, a.kind.app_kind);
    }
  }

  const draftNodes = walkthrough.draft?.nodes ?? [];
  const deletedSet = new Set(walkthrough.annotations.deleted_node_ids);
  const renameMap = new Map(walkthrough.annotations.renamed_nodes.map((r) => [r.node_id, r]));
  const targetMap = new Map(walkthrough.annotations.target_overrides.map((o) => [o.node_id, o]));
  const varPromoMap = new Map(walkthrough.annotations.variable_promotions.map((p) => [p.node_id, p]));

  const warningCount = walkthrough.actions.reduce((sum, a) => sum + a.warnings.length, 0) + walkthrough.warnings.length;

  // Compute groups from the ordered list
  const groups = computeAppGroups(
    walkthrough.nodeOrder, draftNodes, walkthrough.actions,
    walkthrough.actionNodeMap,
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
    const action = walkthrough.actions.find((a) => a.id === lightboxActionId);
    if (!action || action.artifact_paths.length === 0) return null;
    return {
      src: convertFileSrc(action.artifact_paths[0]),
      filename: action.artifact_paths[0].split("/").pop() ?? "screenshot",
      crosshair: crosshairs.get(action.id),
    };
  })();

  function renderScreenshotThumbnail(action: WalkthroughAction) {
    if (action.artifact_paths.length === 0) return null;
    const crosshair = crosshairs.get(action.id);
    return (
      <div>
        <label className="mb-1 block text-[10px] text-[var(--text-muted)]">Screenshot</label>
        <div
          className="relative inline-block cursor-pointer"
          onClick={(e) => { e.stopPropagation(); setLightboxActionId(action.id); }}
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
  }

  // Render a candidate item inline within a group
  function renderCandidateItem(item: Extract<RenderItem, { type: "candidate" }>, group: AppGroup) {
    const { action } = item;
    const { icon, color } = actionIcon(action.kind);
    const isCandidateExpanded = walkthrough.expandedAction === action.id;

    return (
      <div key={item.id}>
        <div
          className={`flex items-center gap-2 rounded-lg px-3 py-2 border border-dashed border-purple-500/30 cursor-pointer hover:bg-[var(--bg-hover)] ${
            isCandidateExpanded ? "bg-[var(--bg-hover)]" : ""
          }`}
          style={groupBorderStyle(group)}
          onClick={() => walkthrough.setExpandedAction(action.id)}
        >
          {/* Empty drag handle spacer */}
          <span className="w-5 shrink-0" />
          {/* Empty step number spacer */}
          <span className="w-5" />
          <span className={`w-4 text-center text-sm ${color}`}>{icon}</span>
          <span className="truncate text-xs text-[var(--text-secondary)]">{actionLabel(action)}</span>
          <span
            className="ml-auto shrink-0 rounded bg-purple-500/20 px-1.5 py-0.5 text-[10px] text-purple-300 cursor-help outline-none focus-visible:ring-1 focus-visible:ring-purple-400"
            tabIndex={0}
            role="note"
            aria-label="Candidate: a possible action detected during the recording that the system isn't sure about (for example, a hover that may or may not have been intentional). Choose Keep to include it in the workflow, or Dismiss to skip it."
            title="Candidate: a possible action detected during the recording that the system isn't sure about (for example, a hover that may or may not have been intentional). Choose Keep to include it in the workflow, or Dismiss to skip it."
          >
            Candidate
          </span>
          <div className="flex gap-0.5 shrink-0 text-[10px]">
            <button onClick={(e) => { e.stopPropagation(); walkthrough.keepCandidate(action.id); }} className="rounded bg-green-700 px-1.5 py-0.5 text-white hover:bg-green-600">Keep</button>
            <button onClick={(e) => { e.stopPropagation(); walkthrough.dismissCandidate(action.id); }} className="rounded bg-[var(--bg-input)] px-1.5 py-0.5 text-[var(--text-muted)] hover:bg-red-500/20">Dismiss</button>
          </div>
        </div>
        {isCandidateExpanded && (
          <div className="border-t border-purple-500/20 px-3 py-2 space-y-2 text-[10px]">
            {action.target_candidates.length > 0 && (
              <div>
                <label className="mb-1 block text-[10px] text-[var(--text-muted)]">Target</label>
                <div className="space-y-1">
                  {action.target_candidates.map((candidate, ci) => (
                    <div key={ci} className="flex items-center gap-2 px-2 py-1 text-xs text-[var(--text-secondary)]">
                      <span>{targetCandidateIcon(candidate)}</span>
                      <span className="truncate">{targetCandidateLabel(candidate)}</span>
                      <span className="ml-auto shrink-0 text-[10px] text-[var(--text-muted)]">{targetCandidateMethod(candidate)}</span>
                    </div>
                  ))}
                </div>
              </div>
            )}
            {renderScreenshotThumbnail(action)}
          </div>
        )}
      </div>
    );
  }

  // Render a single node item within a group
  function renderGroupItem(item: RenderItem, group: AppGroup, groupIndex: number, itemIndex: number) {
    if (item.type === "candidate") return renderCandidateItem(item, group);
    const isItemAnchor = group.anchorIndices.has(itemIndex);

    const { node, action } = item;
    const isDeleted = deletedSet.has(node.id);
    const isExpanded = walkthrough.expandedAction === node.id;
    const currentStep = stepNumbers.get(node.id) ?? null;
    const { icon, color } = action ? actionIcon(action.kind) : nodeTypeIcon(node.node_type);

    const renameEntry = renameMap.get(node.id);
    const defaultLabel = action ? actionLabel(action) : node.name;
    const displayLabel = renameEntry?.new_name || defaultLabel;

    const targetOverride = targetMap.get(node.id);
    const chosenTargetIdx = targetOverride?.chosen_candidate_index ?? (action ? preferredTargetIndex(action.target_candidates) : 0);

    const variablePromo = varPromoMap.get(node.id);

    // Pointer-drag displacement
    const isBeingDragged = itemDrag?.id === node.id;
    const nodeItems = group.items.filter((i) => i.type === "node");
    const nodeIdx = nodeItems.findIndex((i) => i.id === node.id);
    let transformY = 0;
    if (itemDrag && itemDrag.groupIndex === groupIndex && !isBeingDragged && nodeIdx >= 0) {
      const { originalIndex, hoverIndex, cardRect } = itemDrag;
      const h = cardRect.height + 4;
      if (hoverIndex < originalIndex && nodeIdx >= hoverIndex && nodeIdx < originalIndex) {
        transformY = h;
      } else if (hoverIndex > originalIndex && nodeIdx > originalIndex && nodeIdx <= hoverIndex) {
        transformY = -h;
      }
    }
    const dragStyle = itemDrag ? {
      transform: `translateY(${transformY}px)`,
      transition: "transform 200ms ease-out",
      opacity: isBeingDragged ? 0 : 1,
    } : {};

    return (
      <div
        key={node.id}
        data-item-id={node.id}
        ref={(el) => {
          if (el) cardRefsMap.current.set(node.id, el);
          else cardRefsMap.current.delete(node.id);
        }}
      >
        <div
          className={`rounded-lg border transition-colors ${
            isDeleted
              ? "border-[var(--border)] opacity-40"
              : isExpanded
                ? "border-[var(--accent-coral)]/30 bg-[var(--bg-hover)]"
                : "border-[var(--border)] hover:border-[var(--text-muted)]/30"
          }`}
          style={{ ...groupBorderStyle(group), ...dragStyle }}
        >
          {/* Collapsed row */}
          <div
            className="group flex cursor-pointer items-center gap-2 px-3 py-2"
            onClick={() => walkthrough.setExpandedAction(node.id)}
          >
            {/* Drag handle */}
            {isDeleted ? (
              <span className="w-5 shrink-0" />
            ) : isItemAnchor ? (
              <span
                className="flex w-5 shrink-0 items-center justify-center text-lg text-[var(--text-muted)] opacity-40 group-hover:opacity-100 transition-opacity select-none cursor-grab active:cursor-grabbing"
                draggable
                onClick={(e) => e.stopPropagation()}
                onDragStart={(e) => {
                  e.stopPropagation();
                  e.dataTransfer.setData(DND_GROUP_INDEX, String(groupIndex));
                  e.dataTransfer.effectAllowed = "move";
                  e.dataTransfer.dropEffect = "move";
                }}
                onDragEnd={() => setDragOverIndex(null)}
              >
                &#x2261;
              </span>
            ) : (
              <span
                className="flex w-5 shrink-0 items-center justify-center text-lg text-[var(--text-muted)] opacity-40 group-hover:opacity-100 transition-opacity select-none cursor-grab active:cursor-grabbing"
                onClick={(e) => e.stopPropagation()}
                onPointerDown={(e) => {
                  e.stopPropagation();
                  const card = (e.currentTarget as HTMLElement).closest("[data-item-id]") as HTMLElement;
                  if (!card) return;
                  const rect = card.getBoundingClientRect();
                  const ni = nodeItems.findIndex((i) => i.id === node.id);
                  if (ni < 0) return;
                  const drag: ItemDrag = {
                    id: node.id,
                    groupIndex,
                    originalIndex: ni,
                    hoverIndex: ni,
                    y: e.clientY,
                    offsetY: e.clientY - rect.top,
                    cardRect: rect,
                  };
                  itemDragRef.current = drag;
                  setItemDrag(drag);
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
                onClick={(e) => { e.stopPropagation(); walkthrough.restoreNode(node.id); }}
                className="rounded px-1.5 py-0.5 text-[10px] text-[var(--text-muted)] hover:text-green-400"
                title="Restore"
              >
                Restore
              </button>
            ) : (
              <button
                onClick={(e) => { e.stopPropagation(); walkthrough.deleteNode(node.id); }}
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
                  onChange={(e) => walkthrough.renameNode(node.id, e.target.value)}
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
                            onChange={() => walkthrough.overrideTarget(node.id, originalIndex)}
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
                          walkthrough.promoteToVariable(node.id, "");
                        } else {
                          walkthrough.removeVariablePromotion(node.id);
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
                      onChange={(e) => walkthrough.promoteToVariable(node.id, e.target.value)}
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
              {action && renderScreenshotThumbnail(action)}
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
          onClick={() => walkthrough.setPanelOpen(false)}
          className="rounded px-1.5 py-0.5 text-[var(--text-muted)] hover:bg-[var(--bg-hover)] hover:text-[var(--text-primary)]"
          title="Close panel"
        >
          &times;
        </button>
      </div>

      {/* Global warnings */}
      {walkthrough.warnings.length > 0 && (
        <div className="border-b border-[var(--border)] px-3 py-2 space-y-1">
          {walkthrough.warnings.map((w, i) => (
            <div key={i} className="rounded bg-yellow-500/10 px-2 py-1 text-[11px] text-yellow-400">
              {w}
            </div>
          ))}
        </div>
      )}

      {/* Error banner */}
      {walkthrough.error && (
        <div className="border-b border-[var(--border)] px-3 py-2">
          <div className="rounded border border-red-500/30 bg-red-500/10 px-3 py-2 text-[11px] text-red-400">
            {walkthrough.error}
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
                {group.appName && (group.items.length > 1 || group.anchorIndices.size > 0) && (
                  <div
                    className="flex items-center gap-2 px-2 py-1 cursor-grab active:cursor-grabbing group/header"
                    draggable
                    onDragStart={(e) => {
                      e.dataTransfer.setData(DND_GROUP_INDEX, String(groupIndex));
                      e.dataTransfer.effectAllowed = "move";
                      e.dataTransfer.dropEffect = "move";
                      (e.currentTarget as HTMLElement).style.opacity = "0.4";
                    }}
                    onDragEnd={(e) => {
                      setDragOverIndex(null);
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

                {/* Items (nodes and candidates inline, time-ordered) */}
                {group.items.map((item, itemIndex) =>
                  renderGroupItem(item, group, groupIndex, itemIndex)
                )}
              </div>
            ))}

            {/* Trailing drop zone for group reorder */}
            <div
              className="relative h-4"
              onDragOver={(e) => {
                e.preventDefault();
                e.dataTransfer.dropEffect = "move";
                setDragOverIndex(groups.length);
              }}
              onDragLeave={() => setDragOverIndex(null)}
              onDrop={(e) => {
                e.preventDefault();
                setDragOverIndex(null);
                const groupIdxStr = e.dataTransfer.getData(DND_GROUP_INDEX);
                if (groupIdxStr) {
                  const fromGroupIdx = parseInt(groupIdxStr, 10);
                  if (!isNaN(fromGroupIdx)) walkthrough.reorderGroup(fromGroupIdx, groups.length);
                }
              }}
            >
              {dragOverIndex === groups.length && (
                <div className="absolute left-0 right-0 top-0 h-0.5 bg-[var(--accent-coral)]" />
              )}
            </div>
          </div>
        )}
      </div>

      {/* Footer */}
      <div className="flex items-center gap-2 border-t border-[var(--border)] px-3 py-2.5">
        <button
          onClick={walkthrough.discardDraft}
          className="rounded-lg border border-red-500/40 px-3 py-1.5 text-xs font-medium text-red-400 hover:bg-red-500/10"
        >
          Cancel
        </button>
        {allDeleted && (
          <span className="text-[10px] text-[var(--text-muted)]">All steps deleted</span>
        )}
        <button
          onClick={walkthrough.applyDraftToCanvas}
          disabled={allDeleted || draftNodes.length === 0}
          className="ml-auto rounded-lg bg-[var(--accent-coral)] px-4 py-1.5 text-xs font-medium text-white hover:opacity-90 disabled:opacity-40"
        >
          Apply
        </button>
      </div>

      {/* Floating drag card */}
      {itemDrag && (() => {
        const dragGroup = groups[itemDrag.groupIndex];
        if (!dragGroup) return null;
        const dragNodeItems = dragGroup.items.filter((i) => i.type === "node");
        const dragItem = dragNodeItems[itemDrag.originalIndex];
        if (!dragItem || dragItem.type !== "node") return null;
        const { node: dragNode, action: dragAction } = dragItem;
        const { icon: dragIcon, color: dragColor } = dragAction ? actionIcon(dragAction.kind) : nodeTypeIcon(dragNode.node_type);
        const dragStep = stepNumbers.get(dragNode.id);
        const dragRename = renameMap.get(dragNode.id);
        const dragDefaultLabel = dragAction ? actionLabel(dragAction) : dragNode.name;
        const dragLabel = dragRename?.new_name || dragDefaultLabel;
        return (
          <div
            className="fixed z-50 pointer-events-none rounded-lg border border-[var(--accent-coral)]/50 bg-[var(--bg-panel)] shadow-lg shadow-black/30"
            style={{
              top: itemDrag.y - itemDrag.offsetY,
              left: itemDrag.cardRect.left,
              width: itemDrag.cardRect.width,
              ...groupBorderStyle(dragGroup),
            }}
          >
            <div className="flex items-center gap-2 px-3 py-2">
              <span className="flex w-5 shrink-0 items-center justify-center text-lg text-[var(--text-muted)]">&#x2261;</span>
              <span className="w-5 text-right text-[10px] text-[var(--text-muted)]">{dragStep ?? "\u2014"}</span>
              <span className={`w-4 text-center text-sm ${dragColor}`}>{dragIcon}</span>
              <span className="flex-1 truncate text-xs text-[var(--text-secondary)]">{dragLabel}</span>
            </div>
          </div>
        );
      })()}

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
