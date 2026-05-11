import { beforeEach, describe, expect, it, vi } from "vitest";

const commandMocks = vi.hoisted(() => ({
  loadAgentChat: vi.fn(),
  openProject: vi.fn(),
  pickWorkflowFile: vi.fn(),
}));

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(),
}));

vi.mock("../../bindings", () => {
  const cache = new Map<string | symbol, unknown>();
  const explicit: Record<string, unknown> = {
    loadAgentChat: commandMocks.loadAgentChat,
    openProject: commandMocks.openProject,
    pickWorkflowFile: commandMocks.pickWorkflowFile,
  };

  return {
    commands: new Proxy(explicit, {
      get(target, prop) {
        if (prop in target) return target[prop as string];
        if (!cache.has(prop)) cache.set(prop, vi.fn(async () => undefined));
        return cache.get(prop);
      },
    }),
  };
});

import { useStore } from "../useAppStore";

describe("projectSlice", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    commandMocks.loadAgentChat.mockResolvedValue({
      status: "ok",
      data: { messages: [] },
    });
    useStore.setState({
      agentStatus: "idle",
      assistantError: null,
      completionDisagreement: null,
      executorState: "idle",
      lastRunStatus: "completed",
      messages: [],
      projectPath: "/tmp/old.clickweave",
      projectId: "00000000-0000-0000-0000-000000000001",
      projectName: "Old Project",
      projectIntent: null,
    });
  });

  it("clears the previous run status when opening a project", async () => {
    commandMocks.pickWorkflowFile.mockResolvedValue({
      status: "ok",
      data: "/tmp/new.clickweave",
    });
    commandMocks.openProject.mockResolvedValue({
      status: "ok",
      data: {
        path: "/tmp/new.clickweave",
        manifest: {
          id: "00000000-0000-0000-0000-000000000002",
          name: "New Project",
          intent: null,
          schema_version: 1,
        },
      },
    });

    await useStore.getState().openProject();

    expect(useStore.getState().lastRunStatus).toBeNull();
  });

  it("clears the previous run status when creating a new project", () => {
    useStore.setState({ lastRunStatus: "failed" });

    useStore.getState().newProject();

    expect(useStore.getState().lastRunStatus).toBeNull();
  });
});
