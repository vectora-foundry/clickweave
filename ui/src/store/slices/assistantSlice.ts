import type { StateCreator } from "zustand";
import type { Workflow, AssistantChatRequest, WorkflowPatch, ChatEntry, Edge } from "../../bindings";
import { commands } from "../../bindings";
import { toEndpoint } from "../settings";
import { errorMessage, isCancelledError } from "../../utils/commandError";
import { isWalkthroughActive } from "./walkthroughSlice";
import { autoDissolveGroups } from "../useWorkflowMutations";
import type { StoreState } from "./types";

export interface AssistantSlice {
  /** Display-only message array, populated by assistant://message events. */
  messages: ChatEntry[];
  /** Session ID used to filter incoming assistant events. */
  expectedSessionId: string | null;
  assistantOpen: boolean;
  assistantLoading: boolean;
  assistantRetrying: boolean;
  assistantError: string | null;
  pendingPatch: WorkflowPatch | null;
  pendingPatchWarnings: string[];
  pendingIntent: string | null;
  hasPendingIntent: boolean;
  contextUsage: number | null;

  setAssistantOpen: (open: boolean) => void;
  toggleAssistant: () => void;
  sendAssistantMessage: (message: string) => Promise<void>;
  applyApprovedPatch: () => Promise<void>;
  discardPendingPatch: () => void;
  cancelAssistantChat: () => Promise<void>;
  clearConversation: () => void;
  appendAssistantMessage: (sessionId: string, entry: ChatEntry) => void;
  setExpectedSessionId: (sessionId: string) => void;
  setMessages: (messages: ChatEntry[]) => void;
}

export const createAssistantSlice: StateCreator<StoreState, [], [], AssistantSlice> = (set, get) => ({
  messages: [],
  expectedSessionId: null,
  assistantOpen: false,
  assistantLoading: false,
  assistantRetrying: false,
  assistantError: null,
  pendingPatch: null,
  pendingPatchWarnings: [],
  pendingIntent: null,
  hasPendingIntent: false,
  contextUsage: null,

  setAssistantOpen: (open) => {
    if (open && isWalkthroughActive(get().walkthroughStatus)) {
      const status = get().walkthroughStatus;
      if (status === "Recording" || status === "Paused") {
        get().cancelWalkthrough();
      }
      // Review/Processing: don't discard — just hide the walkthrough panel
      // while the assistant is open. Closing the assistant restores it.
    }
    set({ assistantOpen: open });
  },
  toggleAssistant: () => {
    const opening = !get().assistantOpen;
    if (opening && isWalkthroughActive(get().walkthroughStatus)) {
      const status = get().walkthroughStatus;
      if (status === "Recording" || status === "Paused") {
        get().cancelWalkthrough();
      }
      // Review/Processing: don't discard — just hide the walkthrough panel
      // while the assistant is open. Closing the assistant restores it.
    }
    set({ assistantOpen: opening });
  },

  sendAssistantMessage: async (message) => {
    const { plannerConfig, fastConfig, fastEnabled, allowAiTransforms, allowAgentSteps, maxRepairAttempts, pushLog, projectPath } = get();
    set({ assistantLoading: true, assistantError: null, assistantRetrying: false });

    // Messages are now appended by assistant://message events from the backend.
    // The frontend only sends the request and handles the response for patch/warnings.
    try {
      const request: AssistantChatRequest = {
        workflow: get().workflow,
        user_message: message,
        run_context: null,
        planner: toEndpoint(plannerConfig),
        fast: fastEnabled ? toEndpoint(fastConfig) : null,
        allow_ai_transforms: allowAiTransforms,
        allow_agent_steps: allowAgentSteps,
        max_repair_attempts: maxRepairAttempts,
        project_path: projectPath ?? null,
      };

      const result = await commands.assistantChat(request);
      if (result.status === "ok") {
        const data = result.data;

        const isNewWorkflow = get().workflow.nodes.length === 0;
        set((s) => ({
          pendingPatch: data.patch ?? s.pendingPatch,
          pendingPatchWarnings: data.patch ? data.warnings : s.pendingPatchWarnings,
          contextUsage: data.context_usage ?? s.contextUsage,
          pendingIntent: data.intent && isNewWorkflow ? data.intent : s.pendingIntent,
          hasPendingIntent: (data.intent && isNewWorkflow) ? true : s.hasPendingIntent,
        }));

        pushLog(`Assistant: ${data.patch ? "generated changes" : "responded"}`);
      } else {
        if (!isCancelledError(result.error)) {
          const msg = errorMessage(result.error);
          set({ assistantError: msg });
          pushLog(`Assistant error: ${msg}`);
        }
      }
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      set({ assistantError: msg });
      pushLog(`Assistant error: ${msg}`);
    } finally {
      set({ assistantLoading: false, assistantRetrying: false });
    }
  },

  applyApprovedPatch: async () => {
    const { pendingPatch, workflow, pushLog } = get();
    if (!pendingPatch) return;
    const edgeKey = (e: Edge) =>
      `${e.from}-${e.to}`;
    const removedEdgeKeys = new Set(
      pendingPatch.removed_edges.map(edgeKey),
    );
    const nodes = [
      ...workflow.nodes
        .filter((n) => !pendingPatch.removed_node_ids.includes(n.id))
        .map((n) => pendingPatch.updated_nodes.find((u) => u.id === n.id) ?? n),
      ...pendingPatch.added_nodes,
    ];
    const edges = [
      ...workflow.edges.filter((e) => !removedEdgeKeys.has(edgeKey(e))),
      ...pendingPatch.added_edges,
    ];
    const removedIdSet = new Set(pendingPatch.removed_node_ids);
    const cleanedGroups = autoDissolveGroups(
      (workflow.groups ?? []).map((g) => ({
        ...g,
        node_ids: g.node_ids.filter((id: string) => !removedIdSet.has(id)),
      })),
    );
    // Rebuild counters from merged nodes to include patch-added auto_ids
    const patchedCounters = { ...(workflow.next_id_counters ?? {}) } as Record<string, number>;
    for (const node of nodes) {
      if (!node.auto_id) continue;
      const idx = node.auto_id.lastIndexOf("_");
      if (idx === -1) continue;
      const base = node.auto_id.slice(0, idx);
      const num = parseInt(node.auto_id.slice(idx + 1), 10);
      if (!isNaN(num) && num > (patchedCounters[base] ?? 0)) {
        patchedCounters[base] = num;
      }
    }
    const { pendingIntent, hasPendingIntent } = get();
    const patched: Workflow = {
      ...workflow,
      nodes,
      edges,
      groups: cleanedGroups,
      next_id_counters: patchedCounters,
      ...(hasPendingIntent ? { intent: pendingIntent } : {}),
    };
    try {
      const validation = await commands.validate(patched);
      if (!validation.valid) {
        const msg = `Patch rejected: ${validation.errors.join(", ")}`;
        pushLog(msg);
        set({ assistantError: msg });
        return;
      }
    } catch (e) {
      const msg = `Patch validation failed: ${e instanceof Error ? e.message : String(e)}`;
      pushLog(msg);
      set({ assistantError: msg });
      return;
    }
    get().pushHistory("Apply AI Changes");
    set({
      workflow: patched,
      pendingPatch: null,
      pendingPatchWarnings: [],
      pendingIntent: null,
      hasPendingIntent: false,
      assistantError: null,
      isNewWorkflow: false,
    });
    pushLog("Applied assistant changes");
  },

  discardPendingPatch: () => {
    set({
      pendingPatch: null,
      pendingPatchWarnings: [],
      pendingIntent: null,
      hasPendingIntent: false,
      assistantError: null,
    });
  },

  cancelAssistantChat: async () => {
    await commands.cancelAssistantChat();
    set({ assistantLoading: false, assistantError: null, assistantRetrying: false });
  },

  clearConversation: () => {
    commands.clearAssistantSession().catch(() => {});
    set({
      messages: [],
      expectedSessionId: null,
      pendingPatch: null,
      pendingPatchWarnings: [],
      pendingIntent: null,
      hasPendingIntent: false,
      assistantError: null,
      contextUsage: null,
    });
  },

  appendAssistantMessage: (sessionId, entry) => {
    if (get().expectedSessionId !== null && get().expectedSessionId !== sessionId) return;
    set((s) => ({ messages: [...s.messages, entry] }));
  },

  setExpectedSessionId: (sessionId) => {
    const current = get().expectedSessionId;
    if (current === sessionId) return;
    set({ expectedSessionId: sessionId, messages: current === null ? get().messages : [] });
  },

  setMessages: (messages) => {
    set({ messages });
  },
});
