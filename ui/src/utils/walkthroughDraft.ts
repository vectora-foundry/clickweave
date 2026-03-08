import type { Edge, Node, NodeType, WalkthroughAction, WalkthroughAnnotations, Workflow } from "../bindings";
import type { ActionNodeEntry } from "../store/slices/walkthroughSlice";
import { buildActionByNodeId } from "../store/slices/walkthroughSlice";

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

      // Apply target override (Click nodes).
      const targetOvr = targetMap.get(n.id);
      if (targetOvr && updated.node_type.type === "Click") {
        const action = actionByNodeId.get(n.id);
        const candidate = action?.target_candidates[targetOvr.chosen_candidate_index];
        if (candidate) {
          let nodeType: NodeType;
          if (candidate.type === "AccessibilityLabel" || candidate.type === "VlmLabel") {
            nodeType = { ...updated.node_type, target: candidate.label, template_image: null, x: null, y: null };
          } else if (candidate.type === "OcrText") {
            nodeType = { ...updated.node_type, target: candidate.text, template_image: null, x: null, y: null };
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

  // Rebuild edges: keep original edges between non-deleted nodes, and
  // bridge gaps left by deleted nodes. Walkthrough drafts are linear, so
  // consecutive non-deleted nodes should be connected.
  const keptEdges = draft.edges.filter(
    (e) => !deletedNodeIds.has(e.from) && !deletedNodeIds.has(e.to),
  );
  const connectedPairs = new Set(keptEdges.map((e) => `${e.from}->${e.to}`));
  const activeNodeIds = nodes.map((n) => n.id);
  for (let i = 0; i < activeNodeIds.length - 1; i++) {
    const key = `${activeNodeIds[i]}->${activeNodeIds[i + 1]}`;
    if (!connectedPairs.has(key)) {
      keptEdges.push({ from: activeNodeIds[i], to: activeNodeIds[i + 1], output: null });
    }
  }

  return { nodes, edges: keptEdges };
}
