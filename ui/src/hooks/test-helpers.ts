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
    expected_outcome: null,
    checks: [],
  };
}

export function edge(from: string, to: string, output?: Edge["output"]): Edge {
  return { from, to, output: output ?? null };
}

export function makeWorkflow(nodes: Node[], edges: Edge[]): Workflow {
  return { id: "test-id", name: "test", nodes, edges };
}
