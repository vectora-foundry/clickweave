import type { ClickTarget, Edge, Node, NodeType, Position, WalkthroughAction, WalkthroughAnnotations, Workflow } from "../bindings";
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
): { nodes: Node[]; edges: Edge[] } {
  // Collect deleted node IDs directly (annotations already use node_id).
  const deletedNodeIds = new Set(annotations.deleted_node_ids);

  // Build action lookup by node_id for target candidates.
  const actionByNodeId = buildActionByNodeId(actionNodeMap, actions);

  // Pre-build annotation lookups for O(1) access in the loop.
  const renameMap = new Map(annotations.renamed_nodes.map((r) => [r.node_id, r]));
  const targetMap = new Map(annotations.target_overrides.map((o) => [o.node_id, o]));
  const varPromoMap = new Map(annotations.variable_promotions.map((p) => [p.node_id, p]));

  // Filter and transform nodes.
  const nodes = draft.nodes
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
          let nodeType: NodeType;
          if (candidate.type === "CdpElement") {
            nodeType = { ...updated.node_type, target: { type: "CdpElement", name: candidate.name, role: candidate.role, href: candidate.href, parent_role: candidate.parent_role, parent_name: candidate.parent_name }, template_image: null, x: null, y: null };
          } else if (candidate.type === "AccessibilityLabel" || candidate.type === "VlmLabel") {
            nodeType = { ...updated.node_type, target: { type: "Text", text: candidate.label }, template_image: null, x: null, y: null };
          } else if (candidate.type === "OcrText") {
            nodeType = { ...updated.node_type, target: { type: "Text", text: candidate.text }, template_image: null, x: null, y: null };
          } else if (candidate.type === "ImageCrop") {
            nodeType = { ...updated.node_type, target: null, template_image: candidate.image_b64, x: null, y: null };
          } else if (candidate.type === "Coordinates") {
            nodeType = { ...updated.node_type, target: null, template_image: null, x: candidate.x, y: candidate.y };
          } else {
            nodeType = updated.node_type;
          }
          updated = { ...updated, node_type: nodeType };
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

  return { nodes, edges };
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

  // Target resolution priority: CDP > actionable text label > image crop > coordinates
  // Mirrors backend preferred_label(): only actionable AX roles qualify.
  const cdp = action.target_candidates.find((c) => c.type === "CdpElement");
  const textLabel = action.target_candidates.find((c) => {
    if (c.type === "AccessibilityLabel") return ACTIONABLE_AX_ROLES.has(c.role ?? "");
    return c.type === "VlmLabel" || c.type === "OcrText";
  });
  const imageCrop = action.target_candidates.find((c) => c.type === "ImageCrop");

  let target: ClickTarget | null = null;
  let template_image: string | null = null;
  let x: number | null = null;
  let y: number | null = null;

  if (cdp && cdp.type === "CdpElement") {
    target = { type: "CdpElement", name: cdp.name, role: cdp.role, href: cdp.href, parent_role: cdp.parent_role, parent_name: cdp.parent_name };
  } else if (textLabel) {
    const label = textLabel.type === "OcrText"
      ? textLabel.text
      : textLabel.type === "AccessibilityLabel" || textLabel.type === "VlmLabel"
        ? textLabel.label
        : "";
    target = { type: "Text", text: label };
  } else if (imageCrop && imageCrop.type === "ImageCrop") {
    template_image = imageCrop.image_b64;
  } else {
    x = hx;
    y = hy;
  }

  const name = target?.type === "Text"
    ? `Hover '${target.text}'`
    : target?.type === "CdpElement"
      ? `Hover ${target.name || "element"}`
      : `Hover (${Math.round(hx)}, ${Math.round(hy)})`;

  return {
    id: nodeId,
    name,
    node_type: { type: "Hover", target, template_image, x, y, dwell_ms },
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
 */
export function findCandidateInsertIndex(
  actionId: string,
  actions: WalkthroughAction[],
  actionNodeMap: ActionNodeEntry[],
  draftNodes: Node[],
): number {
  const actionIdx = actions.findIndex((a) => a.id === actionId);

  // Find the first action AFTER this one that has a node in the draft.
  for (let i = actionIdx + 1; i < actions.length; i++) {
    const entry = actionNodeMap.find((e) => e.action_id === actions[i].id);
    if (entry) {
      const nodeIdx = draftNodes.findIndex((n) => n.id === entry.node_id);
      if (nodeIdx >= 0) return nodeIdx;
    }
  }

  return draftNodes.length; // append at end
}

/** Recompute vertical positions for a linear list of draft nodes. */
export function recomputeNodePositions(nodes: Node[]): Node[] {
  return nodes.map((n, i) => ({
    ...n,
    position: { x: NODE_X_POSITION, y: i * NODE_Y_SPACING },
  }));
}
