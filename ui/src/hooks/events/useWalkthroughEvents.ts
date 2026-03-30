import { useEffect } from "react";
import { listen } from "@tauri-apps/api/event";
import { useStore } from "../../store/useAppStore";
import type { WalkthroughStatus } from "../../store/slices/walkthroughSlice";

/**
 * Subscribe to walkthrough and recording-bar events:
 * walkthrough://state, walkthrough://event, walkthrough://draft_ready,
 * walkthrough://cdp-setup, recording-bar://action.
 */
export function useWalkthroughEvents() {
  useEffect(() => {
    const unlisteners: (() => void)[] = [];
    let cancelled = false;

    const sub = (p: Promise<() => void>) =>
      p.then((u) => {
        if (cancelled) { u(); return; }
        unlisteners.push(u);
      }).catch((err) => {
        console.error("Failed to subscribe to walkthrough event:", err);
        useStore.getState().pushLog(`Critical: walkthrough event listener failed: ${err}`);
      });

    sub(listen<{ status: string }>("walkthrough://state", (e) => {
      useStore.getState().setWalkthroughStatus(
        e.payload.status as WalkthroughStatus,
      );
    }));
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    sub(listen<{ event: any }>("walkthrough://event", (e) => {
      useStore.getState().pushWalkthroughEvent(e.payload.event);
    }));
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    sub(listen<{ actions: any[]; draft: any; warnings: string[]; action_node_map: any[] }>("walkthrough://draft_ready", (e) => {
      useStore.getState().setWalkthroughDraft({
        actions: e.payload.actions,
        draft: e.payload.draft,
        warnings: e.payload.warnings,
        action_node_map: e.payload.action_node_map ?? [],
      });
    }));
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    sub(listen<any>("walkthrough://cdp-setup", (e) => {
      useStore.getState().pushCdpProgress(e.payload);
    }));
    sub(listen<{ action: string }>("recording-bar://action", (e) => {
      const s = useStore.getState();
      switch (e.payload.action) {
        case "pause": s.pauseWalkthrough(); break;
        case "resume": s.resumeWalkthrough(); break;
        case "stop": s.stopWalkthrough(); break;
        case "cancel": s.cancelWalkthrough(); break;
      }
    }));

    return () => {
      cancelled = true;
      unlisteners.forEach((u) => u());
    };
  }, []);
}
