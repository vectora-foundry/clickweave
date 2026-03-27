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
  return Object.keys(nodeType)[0] ?? "";
}
