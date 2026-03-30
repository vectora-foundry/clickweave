import { useEffect } from "react";
import { commands } from "../bindings";
import { useStore } from "../store/useAppStore";
import { useExecutorNodeEvents } from "./events/useExecutorNodeEvents";
import { useSupervisionEvents } from "./events/useSupervisionEvents";
import { useAssistantEvents } from "./events/useAssistantEvents";
import { useWalkthroughEvents } from "./events/useWalkthroughEvents";
import { useMenuEvents } from "./events/useMenuEvents";

/**
 * Subscribe to all Tauri `executor://`, `walkthrough://`, `menu://`,
 * `assistant://`, and `recording-bar://` backend events, dispatching
 * them into the Zustand store via `getState()` to avoid stale closures.
 *
 * Composes focused sub-hooks — one per event domain — and runs the
 * MCP sidecar status check on mount.
 */
export function useExecutorEvents() {
  useExecutorNodeEvents();
  useSupervisionEvents();
  useAssistantEvents();
  useWalkthroughEvents();
  useMenuEvents();

  // Check MCP sidecar status on mount.
  useEffect(() => {
    commands.getMcpStatus().then((result) => {
      if (result.status === "ok") {
        useStore.getState().pushLog(`MCP sidecar ready: ${result.data}`);
      } else {
        useStore.getState().pushLog(`⚠ MCP sidecar not found: ${result.error}. Workflow execution, planning, and walkthroughs will fail.`);
      }
    });
  }, []);
}
