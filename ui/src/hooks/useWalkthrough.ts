import { useStore } from "../store/useAppStore";
import { useShallow } from "zustand/react/shallow";

/**
 * Selector hook that extracts walkthrough recording state and actions from
 * the store. Uses `useShallow` to batch the selection and avoid unnecessary
 * re-renders.
 */
export function useWalkthrough() {
  return useStore(
    useShallow((s) => ({
      // State
      status: s.walkthroughStatus,
      panelOpen: s.walkthroughPanelOpen,
      error: s.walkthroughError,
      sessionId: s.walkthroughSessionId,
      events: s.walkthroughEvents,
      actions: s.walkthroughActions,
      warnings: s.walkthroughWarnings,
      annotations: s.walkthroughAnnotations,
      saveSheetOpen: s.walkthroughSaveSheetOpen,
      cdpModalOpen: s.walkthroughCdpModalOpen,
      cdpProgress: s.walkthroughCdpProgress,

      // Core actions
      setStatus: s.setWalkthroughStatus,
      setPanelOpen: s.setWalkthroughPanelOpen,
      setSaveSheetOpen: s.setWalkthroughSaveSheetOpen,
      setDraft: s.setWalkthroughDraft,

      // Recording actions
      pushEvent: s.pushWalkthroughEvent,
      pushCdpProgress: s.pushCdpProgress,
      openCdpModal: s.openCdpModal,
      closeCdpModal: s.closeCdpModal,
      startWalkthrough: s.startWalkthrough,
      pauseWalkthrough: s.pauseWalkthrough,
      resumeWalkthrough: s.resumeWalkthrough,
      stopWalkthrough: s.stopWalkthrough,
      cancelWalkthrough: s.cancelWalkthrough,

      // Review actions
      deleteNode: s.deleteNode,
      restoreNode: s.restoreNode,
      renameNode: s.renameNode,
      resetAnnotations: s.resetAnnotations,
      discardDraft: s.discardDraft,
    })),
  );
}
