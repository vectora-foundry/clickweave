export interface OutputFieldInfo {
  name: string;
  type: string;
  description: string;
}

export const OUTPUT_SCHEMAS: Record<string, OutputFieldInfo[]> = {
  FindText: [
    { name: "found", type: "Bool", description: "Whether any matches were found" },
    { name: "count", type: "Number", description: "Number of matches found" },
    { name: "text", type: "String", description: "Text of the first match" },
    { name: "coordinates", type: "Object", description: "Coordinates of the first match" },
  ],
  FindImage: [
    { name: "found", type: "Bool", description: "Whether any matches were found" },
    { name: "count", type: "Number", description: "Number of matches found" },
    { name: "coordinates", type: "Object", description: "Coordinates of the first match" },
    { name: "confidence", type: "Number", description: "Confidence score" },
  ],
  FindApp: [
    { name: "found", type: "Bool", description: "Whether the app is running" },
    { name: "name", type: "String", description: "App name" },
    { name: "pid", type: "Number", description: "Process ID" },
  ],
  TakeScreenshot: [{ name: "result", type: "String", description: "Screenshot data" }],
  CdpWait: [{ name: "found", type: "Bool", description: "Whether text appeared" }],
  AiStep: [{ name: "result", type: "String", description: "LLM response text" }],
  McpToolCall: [{ name: "result", type: "Any", description: "Raw tool result" }],
  AppDebugKitOp: [{ name: "result", type: "Any", description: "Raw tool result" }],
};

/** Get output schema fields for a node type name. */
export function getOutputSchema(nodeTypeName: string): OutputFieldInfo[] {
  return OUTPUT_SCHEMAS[nodeTypeName] ?? [];
}

/** Get the node type name from a NodeType tagged union object. */
export function nodeTypeName(nodeType: Record<string, unknown>): string {
  return (nodeType as { type?: string }).type ?? "";
}

export interface ExtractedRef {
  key: string;
  ref: { node: string; field: string };
}

/** Extract all OutputRef fields from a NodeType's inner params.
 *  NodeType uses internally-tagged serde: the variant name is in the `type` field
 *  and params are spread as sibling keys. */
export function extractOutputRefs(nodeType: Record<string, unknown>): ExtractedRef[] {
  return Object.entries(nodeType)
    .filter(([key, val]) => key.endsWith("_ref") && val != null)
    .map(([key, val]) => ({ key, ref: val as { node: string; field: string } }));
}

/** Map NodeType variant name to auto_id base string (mirrors Rust auto_id_base). */
const AUTO_ID_BASE: Record<string, string> = {
  FindText: "find_text",
  FindImage: "find_image",
  FindApp: "find_app",
  TakeScreenshot: "take_screenshot",
  Click: "click",
  Hover: "hover",
  Drag: "drag",
  TypeText: "type_text",
  PressKey: "press_key",
  Scroll: "scroll",
  FocusWindow: "focus_window",
  LaunchApp: "launch_app",
  QuitApp: "quit_app",
  CdpClick: "cdp_click",
  CdpHover: "cdp_hover",
  CdpFill: "cdp_fill",
  CdpType: "cdp_type",
  CdpPressKey: "cdp_press_key",
  CdpNavigate: "cdp_navigate",
  CdpNewPage: "cdp_new_page",
  CdpClosePage: "cdp_close_page",
  CdpSelectPage: "cdp_select_page",
  CdpWait: "cdp_wait",
  CdpHandleDialog: "cdp_handle_dialog",
  AiStep: "ai_step",
  If: "if",
  Switch: "switch",
  Loop: "loop",
  EndLoop: "end_loop",
  McpToolCall: "mcp_tool_call",
  AppDebugKitOp: "app_debug_kit_op",
};

/** Generate an auto_id for a new node, scanning existing auto_ids to pick the next counter.
 *  Returns the generated auto_id string. */
export function generateAutoId(
  nodeTypeName: string,
  existingAutoIds: (string | undefined)[],
): string {
  const base = AUTO_ID_BASE[nodeTypeName] ?? nodeTypeName.toLowerCase().replace(/\s+/g, "_");
  let maxCounter = 0;
  const prefix = base + "_";
  for (const id of existingAutoIds) {
    if (id && id.startsWith(prefix)) {
      const num = parseInt(id.slice(prefix.length), 10);
      if (!isNaN(num) && num > maxCounter) {
        maxCounter = num;
      }
    }
  }
  return `${base}_${maxCounter + 1}`;
}
