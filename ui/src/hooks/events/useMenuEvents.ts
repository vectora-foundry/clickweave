import { useEffect } from "react";
import { listen } from "@tauri-apps/api/event";
import { useStore } from "../../store/useAppStore";

/**
 * Subscribe to menu events:
 * menu://new, menu://open, menu://save, menu://toggle-sidebar,
 * menu://toggle-logs, menu://run-workflow, menu://stop-workflow, menu://toggle-assistant.
 */
export function useMenuEvents() {
  useEffect(() => {
    const unlisteners: (() => void)[] = [];
    let cancelled = false;

    const sub = (p: Promise<() => void>) =>
      p.then((u) => {
        if (cancelled) { u(); return; }
        unlisteners.push(u);
      }).catch((err) => {
        console.error("Failed to subscribe to menu event:", err);
        useStore.getState().pushLog(`Critical: menu event listener failed: ${err}`);
      });

    sub(listen("menu://new", () => useStore.getState().newProject()));
    sub(listen("menu://open", () => useStore.getState().openProject()));
    sub(listen("menu://save", () => useStore.getState().saveProject()));
    sub(listen("menu://toggle-sidebar", () => useStore.getState().toggleSidebar()));
    sub(listen("menu://toggle-logs", () => useStore.getState().toggleLogsDrawer()));
    sub(listen("menu://run-workflow", () => useStore.getState().runWorkflow()));
    sub(listen("menu://stop-workflow", () => useStore.getState().stopWorkflow()));
    // D21 — on Overview this is a no-op (the embedded assistant card is
    // always rendered); on Canvas it toggles the drawer surface via
    // `assistantSurface`. The slice action handles both cases.
    sub(listen("menu://toggle-assistant", () => useStore.getState().toggleAssistant()));

    return () => {
      cancelled = true;
      unlisteners.forEach((u) => u());
    };
  }, []);
}
