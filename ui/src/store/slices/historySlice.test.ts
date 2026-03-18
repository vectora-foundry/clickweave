import { describe, it, expect, beforeEach } from "vitest";
import { create } from "zustand";
import { createHistoryEntry, pushToStack, preserveSelection, createHistorySlice, MAX_HISTORY } from "./historySlice";
import type { HistorySlice } from "./historySlice";
import type { Workflow, Node } from "../../bindings";

const makeWorkflow = (name: string): Workflow => ({
  id: "test-id",
  name,
  nodes: [],
  edges: [],
  groups: [],
});

describe("createHistoryEntry", () => {
  it("deep-clones the workflow", () => {
    const wf = makeWorkflow("original");
    const entry = createHistoryEntry("test", wf);
    wf.name = "mutated";
    expect(entry.workflow.name).toBe("original");
  });

  it("stores the label", () => {
    const entry = createHistoryEntry("Delete Node", makeWorkflow("wf"));
    expect(entry.label).toBe("Delete Node");
  });
});

describe("pushToStack", () => {
  it("adds entry to the end", () => {
    const stack = pushToStack([], { label: "a", workflow: makeWorkflow("a") });
    expect(stack).toHaveLength(1);
    expect(stack[0].label).toBe("a");
  });

  it("preserves existing entries", () => {
    const existing = [{ label: "first", workflow: makeWorkflow("first") }];
    const stack = pushToStack(existing, { label: "second", workflow: makeWorkflow("second") });
    expect(stack).toHaveLength(2);
    expect(stack[0].label).toBe("first");
    expect(stack[1].label).toBe("second");
  });

  it("caps at MAX_HISTORY by dropping oldest", () => {
    let stack: { label: string; workflow: Workflow }[] = [];
    for (let i = 0; i < MAX_HISTORY + 5; i++) {
      stack = pushToStack(stack, { label: `entry-${i}`, workflow: makeWorkflow(`wf-${i}`) });
    }
    expect(stack).toHaveLength(MAX_HISTORY);
    expect(stack[0].label).toBe("entry-5");
    expect(stack[stack.length - 1].label).toBe(`entry-${MAX_HISTORY + 4}`);
  });

  it("does not mutate the original stack", () => {
    const original = [{ label: "a", workflow: makeWorkflow("a") }];
    const result = pushToStack(original, { label: "b", workflow: makeWorkflow("b") });
    expect(original).toHaveLength(1);
    expect(result).toHaveLength(2);
  });
});

const makeNode = (id: string): Node => ({
  id,
  node_type: { type: "ListWindows", app_name: null },
  position: { x: 0, y: 0 },
  name: id,
  enabled: true,
  timeout_ms: null,
  settle_ms: null,
  retries: 0,
  trace_level: "Minimal",
  role: "Default",
  expected_outcome: null,
});

const makeWorkflowWithNodes = (name: string, nodeIds: string[]): Workflow => ({
  id: "test-id",
  name,
  nodes: nodeIds.map(makeNode),
  edges: [],
  groups: [],
});

describe("preserveSelection", () => {
  it("returns null when selectedNode is null", () => {
    const wf = makeWorkflowWithNodes("wf", ["a", "b"]);
    expect(preserveSelection(null, wf)).toBeNull();
  });

  it("preserves selection when node exists in workflow", () => {
    const wf = makeWorkflowWithNodes("wf", ["a", "b"]);
    expect(preserveSelection("a", wf)).toBe("a");
  });

  it("clears selection when node does not exist in workflow", () => {
    const wf = makeWorkflowWithNodes("wf", ["a", "b"]);
    expect(preserveSelection("c", wf)).toBeNull();
  });

  it("clears selection when workflow has no nodes", () => {
    const wf = makeWorkflow("empty");
    expect(preserveSelection("a", wf)).toBeNull();
  });
});

// Minimal store type for testing the slice in isolation
interface TestStore extends HistorySlice {
  workflow: Workflow;
  selectedNode: string | null;
  setWorkflow: (w: Workflow) => void;
}

function createTestStore(initialWorkflow?: Workflow) {
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  return create<TestStore>()((...a) => {
    const [set] = a;
    return {
      workflow: initialWorkflow ?? makeWorkflow("initial"),
      selectedNode: null,
      setWorkflow: (w: Workflow) => set({ workflow: w }),
      // Cast needed: createHistorySlice expects full StoreState but only
      // accesses workflow, selectedNode, past, and future at runtime.
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      ...(createHistorySlice as any)(...a),
    };
  });
}

describe("undo/redo stack transitions", () => {
  let store: ReturnType<typeof createTestStore>;

  beforeEach(() => {
    store = createTestStore();
  });

  it("pushHistory snapshots current workflow and clears future", () => {
    store.getState().pushHistory("Add Node");
    const { past, future } = store.getState();
    expect(past).toHaveLength(1);
    expect(past[0].label).toBe("Add Node");
    expect(past[0].workflow.name).toBe("initial");
    expect(future).toHaveLength(0);
  });

  it("undo pops from past and pushes current to future", () => {
    // Setup: push history then change workflow
    store.getState().pushHistory("first");
    store.getState().setWorkflow(makeWorkflow("second"));

    store.getState().undo();
    const { past, future, workflow } = store.getState();
    expect(past).toHaveLength(0);
    expect(future).toHaveLength(1);
    expect(future[0].workflow.name).toBe("second");
    expect(workflow.name).toBe("initial");
  });

  it("redo pops from future and pushes current to past", () => {
    store.getState().pushHistory("first");
    store.getState().setWorkflow(makeWorkflow("second"));
    store.getState().undo();

    store.getState().redo();
    const { past, future, workflow } = store.getState();
    expect(past).toHaveLength(1);
    expect(future).toHaveLength(0);
    expect(workflow.name).toBe("second");
  });

  it("undo is a no-op when past is empty", () => {
    store.getState().undo();
    const { past, future, workflow } = store.getState();
    expect(past).toHaveLength(0);
    expect(future).toHaveLength(0);
    expect(workflow.name).toBe("initial");
  });

  it("redo is a no-op when future is empty", () => {
    store.getState().redo();
    const { past, future, workflow } = store.getState();
    expect(past).toHaveLength(0);
    expect(future).toHaveLength(0);
    expect(workflow.name).toBe("initial");
  });

  it("new push clears the future stack", () => {
    store.getState().pushHistory("first");
    store.getState().setWorkflow(makeWorkflow("second"));
    store.getState().undo();
    expect(store.getState().future).toHaveLength(1);

    // New action should clear future
    store.getState().pushHistory("new action");
    expect(store.getState().future).toHaveLength(0);
    expect(store.getState().past).toHaveLength(1);
  });

  it("clearHistory resets both stacks", () => {
    store.getState().pushHistory("a");
    store.getState().setWorkflow(makeWorkflow("b"));
    store.getState().pushHistory("b");
    store.getState().undo();

    store.getState().clearHistory();
    const { past, future } = store.getState();
    expect(past).toHaveLength(0);
    expect(future).toHaveLength(0);
  });

  it("multiple undo/redo round-trips restore correct states", () => {
    // Create 3 states: initial -> second -> third
    store.getState().pushHistory("to-second");
    store.getState().setWorkflow(makeWorkflow("second"));
    store.getState().pushHistory("to-third");
    store.getState().setWorkflow(makeWorkflow("third"));

    // Undo twice: third -> second -> initial
    store.getState().undo();
    expect(store.getState().workflow.name).toBe("second");
    store.getState().undo();
    expect(store.getState().workflow.name).toBe("initial");

    // Redo twice: initial -> second -> third
    store.getState().redo();
    expect(store.getState().workflow.name).toBe("second");
    store.getState().redo();
    expect(store.getState().workflow.name).toBe("third");
  });
});
