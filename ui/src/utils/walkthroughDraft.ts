import type { ClickTarget, Edge, Node, Position, WalkthroughAction, WalkthroughAnnotations, Workflow } from "../bindings";
import type { ActionNodeEntry } from "../store/slices/walkthroughSlice";
import { buildActionByNodeId } from "../store/slices/walkthroughSlice";
import { ACTIONABLE_AX_ROLES } from "./walkthroughFormatting";

const NODE_X_POSITION = 250;
const NODE_Y_SPACING = 100;

/**
 * Pure transformation that applies walkthrough annotations (deletes, renames,
 * target overrides, variable promotions) to a draft workflow, producing the
 * final nodes and edges ready to be set on the canvas.
 *
 * This bridges gaps left by deleted nodes by reconnecting consecutive
 * non-deleted nodes, which is correct for the linear walkthrough drafts.
 */
export function applyAnnotationsToDraft(
  draft: Workflow,
  annotations: WalkthroughAnnotations,
  actions: WalkthroughAction[],
  actionNodeMap: ActionNodeEntry[],
  nodeOrder?: string[],
): { nodes: Node[]; edges: Edge[] } {
  // Collect deleted node IDs directly (annotations already use node_id).
  const deletedNodeIds = new Set(annotations.deleted_node_ids);

  // Build action lookup by node_id for target candidates.
  const actionByNodeId = buildActionByNodeId(actionNodeMap, actions);

  // Pre-build annotation lookups for O(1) access in the loop.
  const renameMap = new Map(annotations.renamed_nodes.map((r) => [r.node_id, r]));
  const targetMap = new Map(annotations.target_overrides.map((o) => [o.node_id, o]));
  const varPromoMap = new Map(annotations.variable_promotions.map((p) => [p.node_id, p]));

  // Reorder draft nodes to match user-specified order if provided.
  let orderedDraftNodes = draft.nodes;
  if (nodeOrder && nodeOrder.length > 0) {
    const nodeById = new Map(draft.nodes.map((n) => [n.id, n]));
    const reordered: typeof draft.nodes = [];
    const reorderedIds = new Set<string>();
    for (const id of nodeOrder) {
      const n = nodeById.get(id);
      if (n) { reordered.push(n); reorderedIds.add(n.id); }
    }
    // Append any nodes not in nodeOrder (safety fallback)
    for (const n of draft.nodes) {
      if (!reorderedIds.has(n.id)) reordered.push(n);
    }
    orderedDraftNodes = reordered;
  }

  // Filter and transform nodes.
  const nodes = orderedDraftNodes
    .filter((n) => !deletedNodeIds.has(n.id))
    .map((n): Node => {
      let updated = { ...n };

      // Apply rename.
      const rename = renameMap.get(n.id);
      if (rename) updated = { ...updated, name: rename.new_name };

      // Apply target override (Click and Hover nodes).
      const targetOvr = targetMap.get(n.id);
      if (targetOvr && (updated.node_type.type === "Click" || updated.node_type.type === "Hover")) {
        const action = actionByNodeId.get(n.id);
        const candidate = action?.target_candidates[targetOvr.chosen_candidate_index];
        if (candidate) {
          if (candidate.type === "CdpElement") {
            // CDP candidates become CdpClick/CdpHover nodes rather than native Click/Hover with text targets.
            const cdpType = updated.node_type.type === "Click" ? "CdpClick" : "CdpHover";
            updated = { ...updated, node_type: { type: cdpType, target: { kind: "ExactLabel", value: candidate.name } } as unknown as typeof updated.node_type };
          } else {
            let target: ClickTarget | null;
            if (candidate.type === "AccessibilityLabel" || candidate.type === "VlmLabel") {
              target = { type: "Text", text: candidate.label };
            } else if (candidate.type === "OcrText") {
              target = { type: "Text", text: candidate.text };
            } else if (candidate.type === "Coordinates") {
              target = { type: "Coordinates", x: candidate.x, y: candidate.y };
            } else {
              target = updated.node_type.target;
            }
            updated = { ...updated, node_type: { ...updated.node_type, target } };
          }
        }
      }

      // Apply variable promotion (TypeText nodes).
      const varPromo = varPromoMap.get(n.id);
      if (varPromo?.variable_name && updated.node_type.type === "TypeText") {
        updated = { ...updated, node_type: { ...updated.node_type, text: `{{${varPromo.variable_name}}}` } };
      }

      return updated;
    });

  // Rebuild edges from scratch using consecutive node ordering.
  // Walkthrough drafts are always linear, so this is both simpler and
  // correct even when nodes have been inserted (kept candidates) or deleted.
  const edges: Edge[] = [];
  for (let i = 0; i < nodes.length - 1; i++) {
    edges.push({ from: nodes[i].id, to: nodes[i + 1].id, output: null });
  }

  return { nodes: recomputeNodePositions(nodes), edges };
}

/**
 * Synthesize a workflow node from a kept hover candidate action.
 * Mirrors the backend synthesis logic in synthesis.rs for Hover actions.
 */
export function synthesizeNodeForKeptCandidate(
  action: WalkthroughAction,
  nodeId: string,
  position: Position,
): Node {
  if (action.kind.type !== "Hover") {
    throw new Error(`Unexpected candidate action type: ${action.kind.type}`);
  }

  const { x: hx, y: hy, dwell_ms } = action.kind;

  // Target resolution priority: CDP name as text > actionable text label > coordinates
  // Mirrors backend preferred_label(): only actionable AX roles qualify.
  const cdp = action.target_candidates.find((c) => c.type === "CdpElement");
  const textLabel = action.target_candidates.find((c) => {
    if (c.type === "AccessibilityLabel") return ACTIONABLE_AX_ROLES.has(c.role ?? "");
    return c.type === "VlmLabel" || c.type === "OcrText";
  });

  // CDP candidates become CdpHover nodes rather than native Hover with text targets.
  if (cdp && cdp.type === "CdpElement") {
    const name = `Hover '${cdp.name}'`;
    return {
      id: nodeId,
      name,
      node_type: { type: "CdpHover", target: { kind: "ExactLabel", value: cdp.name } } as unknown as Node["node_type"],
      position,
      enabled: true,
      timeout_ms: null,
      settle_ms: null,
      retries: 0,
      supervision_retries: 2,
      trace_level: "Minimal",
      role: "Default",
      expected_outcome: null,
    };
  }

  let target: ClickTarget | null = null;

  if (textLabel) {
    const label = textLabel.type === "OcrText"
      ? textLabel.text
      : textLabel.type === "AccessibilityLabel" || textLabel.type === "VlmLabel"
        ? textLabel.label
        : "";
    target = { type: "Text", text: label };
  } else {
    target = { type: "Coordinates", x: hx, y: hy };
  }

  const name = target.type === "Text"
    ? `Hover '${target.text}'`
    : `Hover (${Math.round(hx)}, ${Math.round(hy)})`;

  return {
    id: nodeId,
    name,
    node_type: { type: "Hover", target, dwell_ms },
    position,
    enabled: true,
    timeout_ms: null,
    settle_ms: null,
    retries: 0,
    supervision_retries: 2,
    trace_level: "Minimal",
    role: "Default",
    expected_outcome: null,
  };
}

/**
 * Find the insertion index for a kept candidate node within the draft's node
 * array, based on its position in the actions list relative to already-mapped
 * actions.
 *
 * Uses "insert after the last mapped action at or before this one" so that
 * hover candidates land after the Launch/Focus setup they logically belong to,
 * matching the backend's insertion strategy.
 */
export function findCandidateInsertIndex(
  actionId: string,
  actions: WalkthroughAction[],
  actionNodeMap: ActionNodeEntry[],
  draftNodes: Node[],
): number {
  const actionIdx = actions.findIndex((a) => a.id === actionId);

  // Find the last action BEFORE this one that has a node in the draft,
  // then insert after that node.
  for (let i = actionIdx - 1; i >= 0; i--) {
    const entry = actionNodeMap.find((e) => e.action_id === actions[i].id);
    if (entry) {
      const nodeIdx = draftNodes.findIndex((n) => n.id === entry.node_id);
      if (nodeIdx >= 0) return nodeIdx + 1;
    }
  }

  return 0; // no preceding mapped action — insert at start
}

/** Recompute vertical positions for a linear list of draft nodes. */
export function recomputeNodePositions(nodes: Node[]): Node[] {
  return nodes.map((n, i) => ({
    ...n,
    position: { x: NODE_X_POSITION, y: i * NODE_Y_SPACING },
  }));
}
