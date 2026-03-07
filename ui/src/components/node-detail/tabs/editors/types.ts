import type { AppKind, Node, NodeType } from "../../../../bindings";

export { APP_KIND_LABELS, usesCdp } from "../../../../utils/appKind";

export interface NodeEditorProps {
  nodeType: NodeType;
  onUpdate: (u: Partial<Node>) => void;
  projectPath: string | null;
  appKind?: AppKind;
}

/** Convert an empty string to null, for optional string fields. */
export function optionalString(v: string): string | null {
  return v === "" ? null : v;
}
