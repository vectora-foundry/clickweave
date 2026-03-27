import { useEffect } from "react";
import { listen } from "@tauri-apps/api/event";
import { useStore } from "../store/useAppStore";

export function usePlannerEvents() {
  useEffect(() => {
    const subscriptions = Promise.all([
      listen<{
        session_id: string;
        tool_name: string;
        args: Record<string, unknown>;
        result: string | null;
      }>("planner://tool_call", (e) => {
        useStore.getState().pushPlannerToolCall({
          toolName: e.payload.tool_name,
          args: e.payload.args,
          result: e.payload.result ?? undefined,
        });
        useStore
          .getState()
          .pushLog(`Planning: called ${e.payload.tool_name}`);
      }),

      listen<{
        session_id: string;
        message: string;
        tool_name: string;
      }>("planner://confirmation_required", (e) => {
        useStore.getState().setPlannerConfirmation({
          sessionId: e.payload.session_id,
          message: e.payload.message,
          toolName: e.payload.tool_name,
        });
      }),

      listen<{ session_id: string }>("planner://session_ended", () => {
        useStore.getState().clearPlannerState();
      }),
    ]);

    return () => {
      subscriptions.then((unsubs) => unsubs.forEach((u) => u()));
    };
  }, []);
}
