import type { Node, WalkthroughAction } from "../bindings";
import type { ActionNodeEntry } from "../store/slices/walkthroughSlice";
import { buildActionByNodeId } from "../store/slices/walkthroughSlice";

// --- Types ---

export type RenderItem =
  | { type: "node"; id: string; node: Node; action: WalkthroughAction | undefined }
  | { type: "candidate"; id: string; action: WalkthroughAction };

export type AppGroup = {
  appName: string | null;
  color: string;
  items: RenderItem[];
  anchorIndex: number; // index within items of FocusWindow/LaunchApp, or -1
};

// --- Color palette ---

export const GROUP_COLORS = [
  "#6366f1", // indigo
  "#10b981", // emerald
  "#f59e0b", // amber
  "#ec4899", // pink
  "#8b5cf6", // violet
  "#06b6d4", // cyan
];

export function hashAppName(name: string): number {
  let hash = 0;
  for (let i = 0; i < name.length; i++) {
    hash = ((hash << 5) - hash + name.charCodeAt(i)) | 0;
  }
  return Math.abs(hash) % GROUP_COLORS.length;
}

// --- ID resolution ---

function resolveItem(
  id: string,
  nodeMap: Map<string, Node>,
  actionByNodeId: Map<string, WalkthroughAction>,
  candidateActionMap: Map<string, WalkthroughAction>,
): RenderItem | null {
  const node = nodeMap.get(id);
  if (node) {
    return { type: "node", id: node.id, node, action: actionByNodeId.get(node.id) };
  }
  const candidate = candidateActionMap.get(id);
  if (candidate) {
    return { type: "candidate", id: candidate.id, action: candidate };
  }
  return null; // stale
}

function itemAppName(item: RenderItem): string | null {
  if (item.type === "candidate") return item.action.app_name ?? null;
  return item.action?.app_name ?? null;
}

const ANCHOR_TYPES = new Set(["FocusWindow", "LaunchApp"]);

function isAnchor(item: RenderItem): boolean {
  if (item.type === "candidate") return ANCHOR_TYPES.has(item.action.kind.type);
  return ANCHOR_TYPES.has(item.node.node_type.type);
}

// --- Group computation ---

export function computeAppGroups(
  orderedIds: string[],
  draftNodes: Node[],
  actions: WalkthroughAction[],
  actionNodeMap: ActionNodeEntry[],
): AppGroup[] {
  const nodeMap = new Map(draftNodes.map((n) => [n.id, n]));
  const candidateActionMap = new Map(
    actions.filter((a) => a.candidate).map((a) => [a.id, a]),
  );
  const actionByNodeId = buildActionByNodeId(actionNodeMap, actions);

  // Resolve all IDs to render items, filtering only stale entries.
  // Deleted items are kept so the panel can render them with a Restore control.
  const resolvedItems: RenderItem[] = [];
  for (const id of orderedIds) {
    const item = resolveItem(id, nodeMap, actionByNodeId, candidateActionMap);
    if (item) resolvedItems.push(item);
  }

  // Group consecutive items by app_name
  const groups: AppGroup[] = [];
  for (const item of resolvedItems) {
    const app = itemAppName(item);
    const lastGroup = groups[groups.length - 1];
    if (lastGroup && lastGroup.appName === app) {
      if (isAnchor(item) && lastGroup.anchorIndex === -1) {
        lastGroup.anchorIndex = lastGroup.items.length;
      }
      lastGroup.items.push(item);
    } else {
      groups.push({
        appName: app,
        color: app ? GROUP_COLORS[hashAppName(app)] : "transparent",
        items: [item],
        anchorIndex: isAnchor(item) ? 0 : -1,
      });
    }
  }

  return groups;
}

// --- Drag validation ---

export function isValidItemDrop(
  dragId: string,
  targetIndex: number,
  groups: AppGroup[],
): boolean {
  // Anchors cannot be dragged
  for (const g of groups) {
    if (g.anchorIndex >= 0 && g.items[g.anchorIndex].id === dragId) {
      return false;
    }
  }

  // Find which group the target index falls into.
  // targetIndex is a flat index into the ordered (non-deleted) items.
  let flatIndex = 0;
  for (const group of groups) {
    const groupStart = flatIndex;
    const groupEnd = flatIndex + group.items.length;
    if (targetIndex >= groupStart && targetIndex < groupEnd) {
      // Dropping within this group — can't go above anchor
      if (group.anchorIndex >= 0) {
        const anchorFlat = groupStart + group.anchorIndex;
        if (targetIndex <= anchorFlat && dragId !== group.items[group.anchorIndex].id) {
          return false;
        }
      }
      return true;
    }
    flatIndex = groupEnd;
  }

  // Dropping at the very end or between groups — always valid
  return true;
}

// --- Initial order ---

export function buildInitialOrder(
  actions: WalkthroughAction[],
  draftNodes: Node[],
  actionNodeMap: ActionNodeEntry[],
): string[] {
  const nodeIdByActionId = new Map(actionNodeMap.map((e) => [e.action_id, e.node_id]));
  const draftNodeIds = new Set(draftNodes.map((n) => n.id));
  const order: string[] = [];
  const emittedNodeIds = new Set<string>();

  for (const action of actions) {
    if (action.candidate) {
      // Candidate: use action ID
      order.push(action.id);
    } else {
      const nodeId = nodeIdByActionId.get(action.id);
      if (nodeId && draftNodeIds.has(nodeId) && !emittedNodeIds.has(nodeId)) {
        order.push(nodeId);
        emittedNodeIds.add(nodeId);
      }
    }
  }

  // Safety: append any draft nodes not covered by actions
  for (const node of draftNodes) {
    if (!emittedNodeIds.has(node.id)) {
      order.push(node.id);
    }
  }

  return order;
}
