import type { Node, Edge, Workflow } from "../bindings";

export function node(id: string, type: string, params?: Record<string, unknown>): Node {
  return {
    id,
    node_type: { type, ...params } as Node["node_type"],
    position: { x: 0, y: 0 },
    name: id,
    enabled: true,
    timeout_ms: null,
    settle_ms: null,
    retries: 0,
    trace_level: "Full",
    role: "Default",
    expected_outcome: null,
  };
}

export function edge(from: string, to: string, output?: Edge["output"]): Edge {
  return { from, to, output: output ?? null };
}

export function makeWorkflow(nodes: Node[], edges: Edge[], groups?: Workflow["groups"]): Workflow {
  return { id: "test-id", name: "test", nodes, edges, groups: groups ?? [] };
}
