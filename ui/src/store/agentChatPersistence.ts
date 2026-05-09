import { commands } from "../bindings";
import type { AssistantMessage } from "./slices/assistantSlice";

interface SaveContext {
  projectPath: string | null;
  projectName: string;
  projectId: string;
  storeTraces: boolean;
}

/**
 * Load the saved chat transcript for a workflow. Returns an empty
 * array on any error (missing file, malformed JSON) — transcript is
 * best-effort, never blocks project open.
 */
export async function loadAgentChat(
  ctx: Omit<SaveContext, "storeTraces">,
): Promise<AssistantMessage[]> {
  try {
    const res = await commands.loadAgentChat({
      project_path: ctx.projectPath,
      project_name: ctx.projectName,
      project_id: ctx.projectId,
    });
    if (res.status !== "ok") return [];
    return res.data.messages.map((m) => ({
      role: m.role,
      content: m.content,
      timestamp: m.timestamp,
      runId: m.run_id ?? undefined,
    }));
  } catch {
    return [];
  }
}

/**
 * Save the chat transcript. Calls are skipped when a Clear
 * conversation wipe is in flight so the backend's just-deleted file
 * isn't recreated by a stale save. The backend command itself is a
 * no-op when `store_traces === false` (D1.M4).
 */
export async function saveAgentChat(
  ctx: SaveContext,
  messages: AssistantMessage[],
): Promise<void> {
  const { isConversationWipeInProgress } = await import(
    "./slices/assistantSlice"
  );
  if (isConversationWipeInProgress()) return;

  try {
    await commands.saveAgentChat({
      project_path: ctx.projectPath,
      project_name: ctx.projectName,
      project_id: ctx.projectId,
      store_traces: ctx.storeTraces,
      chat: {
        messages: messages.map((m) => ({
          role: m.role,
          content: m.content,
          timestamp: m.timestamp,
          run_id: m.runId ?? null,
        })),
      },
    });
  } catch {
    // Transcript saves are best-effort; swallow errors so the
    // conversation stays responsive even when the disk is hostile.
  }
}
