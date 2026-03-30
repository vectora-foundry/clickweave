import { useEffect } from "react";
import { listen } from "@tauri-apps/api/event";
import type { ChatEntry } from "../../bindings";
import { useStore } from "../../store/useAppStore";

/**
 * Subscribe to assistant events:
 * assistant://message, assistant://session_started, assistant://repairing.
 */
export function useAssistantEvents() {
  useEffect(() => {
    const unlisteners: (() => void)[] = [];
    let cancelled = false;

    const sub = (p: Promise<() => void>) =>
      p.then((u) => {
        if (cancelled) { u(); return; }
        unlisteners.push(u);
      }).catch((err) => {
        console.error("Failed to subscribe to assistant event:", err);
        useStore.getState().pushLog(`Critical: assistant event listener failed: ${err}`);
      });

    sub(listen("assistant://repairing", () => {
      useStore.setState({ assistantRetrying: true });
    }));
    sub(listen<{ session_id: string; entry: ChatEntry }>("assistant://message", (e) => {
      useStore.getState().appendAssistantMessage(e.payload.session_id, e.payload.entry);
    }));
    sub(listen<{ session_id: string }>("assistant://session_started", (e) => {
      useStore.getState().setExpectedSessionId(e.payload.session_id);
    }));

    return () => {
      cancelled = true;
      unlisteners.forEach((u) => u());
    };
  }, []);
}
