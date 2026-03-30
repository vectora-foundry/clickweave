import type { Node as RFNode } from "@xyflow/react";
import type { AppKind, Workflow } from "../../bindings";
import { usesCdp } from "../../utils/appKind";
import { nodeMetadata, defaultNodeMetadata } from "../../constants/nodeMetadata";
import { buildDag, type DagGraph, isAppAnchorNode } from "../../utils/appGroupComputation";
import { getFullOutputSchema, extractOutputRefs, fieldTypeFromAutoId, INPUT_SCHEMAS } from "../../utils/outputSchema";

// Layout constants for loop group positioning
export const LOOP_HEADER_HEIGHT = 40;
export const LOOP_PADDING = 20;
export const APPROX_NODE_WIDTH = 160;
export const APPROX_NODE_HEIGHT = 50;
export const MIN_GROUP_WIDTH = 300;
export const MIN_GROUP_HEIGHT = 150;

// App group layout constants
export const APP_GROUP_HEADER_HEIGHT = 36;
export const APP_GROUP_PADDING = 20;

// User group layout constants
export const USER_GROUP_HEADER_HEIGHT = 36;
export const USER_GROUP_PADDING = 20;

/** Return layout constants for a group node type. */
export function groupConstants(parentType: string): { headerHeight: number; padding: number } {
  if (parentType === "appGroup") return { headerHeight: APP_GROUP_HEADER_HEIGHT, padding: APP_GROUP_PADDING };
  if (parentType === "userGroup") return { headerHeight: USER_GROUP_HEADER_HEIGHT, padding: USER_GROUP_PADDING };
  return { headerHeight: LOOP_HEADER_HEIGHT, padding: LOOP_PADDING };
}

export function clickSubtitle(nt: Workflow["nodes"][number]["node_type"]): string | undefined {
  if (nt.type !== "Click") return undefined;
  if (nt.target) {
    if (nt.target.type === "Text") return nt.target.text;
    if (nt.target.type === "Coordinates") return `at (${Math.round(nt.target.x)}, ${Math.round(nt.target.y)})`;
    if (nt.target.type === "WindowControl") {
      const names: Record<string, string> = { Close: "Close window", Minimize: "Minimize window", Maximize: "Maximize window", Zoom: "Zoom window" };
      return names[nt.target.action] ?? nt.target.action;
    }
  }
  return undefined;
}

/** Forward-propagate app_kind from FocusWindow nodes to all downstream nodes. */
export function buildAppKindMap(workflow: Workflow, dag?: DagGraph): Map<string, AppKind> {
  const { nodeById, outgoing, inDegree: inDegreeOriginal } = dag ?? buildDag(workflow);
  const inDegree = new Map(inDegreeOriginal);

  const result = new Map<string, AppKind>();
  const queue: string[] = [];
  for (const [id, deg] of inDegree) {
    if (deg === 0) queue.push(id);
  }

  let head = 0;
  while (head < queue.length) {
    const id = queue[head++];
    const node = nodeById.get(id);

    if (node?.node_type.type === "FocusWindow") {
      result.set(id, node.node_type.app_kind ?? "Native");
    } else if (isAppAnchorNode(node)) {
      // launch_app McpToolCall — extract app_kind from arguments
      const args = node!.node_type.type === "McpToolCall" ? node!.node_type.arguments : null;
      const kind = (typeof args === "object" && args !== null && !Array.isArray(args)
        ? args.app_kind : undefined) as AppKind | undefined;
      result.set(id, kind ?? "Native");
    }

    const kind = result.get(id);
    for (const target of outgoing.get(id) ?? []) {
      if (kind && !result.has(target)) {
        result.set(target, kind);
      }
      inDegree.set(target, (inDegree.get(target) ?? 0) - 1);
      if (inDegree.get(target) === 0) queue.push(target);
    }
  }

  return result;
}

export function nodeSubtitle(
  nt: Workflow["nodes"][number]["node_type"],
  appKind: AppKind | undefined,
): string | undefined {
  // Click nodes: show target info
  const click = clickSubtitle(nt);
  if (click) {
    // Append DevTools context if applicable
    if (appKind && usesCdp(appKind)) return `${click} · via DevTools`;
    return click;
  }
  // Non-FocusWindow nodes: show DevTools context if inherited from upstream
  if (nt.type !== "FocusWindow" && appKind && usesCdp(appKind)) {
    return "via DevTools";
  }
  return undefined;
}

export function toRFNode(
  node: Workflow["nodes"][number],
  selectedNode: string | null,
  activeNode: string | null,
  onDelete: () => void,
  appKind: AppKind | undefined,
  existing?: RFNode,
): RFNode {
  const meta = nodeMetadata[node.node_type.type] ?? defaultNodeMetadata;
  return {
    ...(existing ?? {}),
    parentId: undefined,
    extent: undefined,
    hidden: undefined,
    style: undefined,
    id: node.id,
    type: "workflow",
    position: existing?.position ?? { x: node.position.x, y: node.position.y },
    selected: existing?.selected ?? (node.id === selectedNode),
    data: {
      label: node.name,
      nodeType: node.node_type.type,
      icon: meta.icon,
      color: meta.color,
      isActive: node.id === activeNode,
      enabled: node.enabled,
      onDelete,
      switchCases: node.node_type.type === "Switch"
        ? (node.node_type as { type: "Switch"; cases: { name: string }[] }).cases.map((c) => c.name)
        : [],
      role: node.role,
      autoId: node.auto_id,
      subtitle: nodeSubtitle(node.node_type, appKind),
      outputFields: getFullOutputSchema(node.node_type as unknown as Record<string, unknown>),
      wiredInputs: extractOutputRefs(node.node_type as Record<string, unknown>).map(({ key, ref }) => ({
        key,
        fieldType: fieldTypeFromAutoId(ref.node, ref.field),
      })),
      // All ref-capable input params (for drag-to-wire drop targets)
      availableInputs: (INPUT_SCHEMAS[node.node_type.type] ?? []).map((i) => ({
        key: i.param,
        fieldType: i.acceptedTypes[0] ?? "Any",
      })),
    },
  };
}
