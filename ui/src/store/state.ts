import type { Workflow, ConversationSession } from "../bindings";

export type DetailTab = "setup" | "trace" | "runs";

export interface EndpointConfig {
  baseUrl: string;
  apiKey: string;
  model: string;
}

export function makeEmptyConversation(): ConversationSession {
  return { messages: [], summary: null, summary_cutoff: 0 };
}

export const DEFAULT_ENDPOINT: EndpointConfig = {
  baseUrl: "http://localhost:1234/v1",
  apiKey: "",
  model: "local",
};

export const DEFAULT_VLM_ENABLED = false;
export const DEFAULT_MCP_COMMAND = "npx";

export function makeDefaultWorkflow(): Workflow {
  return {
    id: crypto.randomUUID(),
    name: "New Workflow",
    nodes: [],
    edges: [],
    groups: [],
  };
}
